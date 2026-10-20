//! The keymap conflict-resolution core:
//! generic over the bound payload `B`, fed a claimant list. The editor keeps its command type,
//! its payload (`palette::Bound`) and the adapter that builds the claimants.
//!
//! Policy (plan `editor-feedback-round-2` WP-1 "Hotkey conflict policy"): every claimant claims
//! its chords (an `[keys]` override replaces a bindable's default; `[]` releases it), and a chord
//! claimed by two or more distinct claimants is bound to none of them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::chord::{self, Chord};

/// A bound payload the core can query.
pub trait Binding: Clone {
    /// Whether the focused source editor hands a chord bound to this to the app.
    fn routed(&self) -> bool;
    /// The config action id, when this runs a config action.
    fn action_id(&self) -> Option<&str>;
    /// The claimants of the default keymap (no overrides, no config).
    fn default_claimants() -> Vec<Claimant<Self>>;
}

/// What kind of claimant a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimKind {
    /// An editor-owned bindable: `keys` are its defaults, an override replaces them; a `fixed`
    /// one (text-editing chords) is shown, never routed, never overridden.
    Bindable { fixed: bool },
    /// A config claimant (action / bookmark `keys`): checked, never overridden.
    Config,
}

/// One claimant: its id, its chords, what it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimant<B> {
    pub id: String,
    pub keys: Vec<String>,
    pub run: B,
    pub kind: ClaimKind,
}

/// How [`Keymap::claim_keys`] checks a chord list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Editor defaults: malformed chords are skipped silently, nothing else is checked.
    Default,
    /// An `[keys]` override: malformed, reserved and modifier-less chords become notices.
    Override,
    /// Config keys: malformed and reserved chords become notices.
    Config,
}

/// The effective keymap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap<B> {
    map: HashMap<Chord, B>,
    /// Every malformed / reserved / conflicting chord, as one line each.
    pub notices: Vec<String>,
    /// id → the chords it effectively holds (canonical spelling), for display.
    pub chords: HashMap<String, Vec<String>>,
    /// id → its notices (Hotkeys inline conflicts).
    pub row_notices: HashMap<String, Vec<String>>,
    /// The ids an override applies to.
    pub overridden: BTreeSet<String>,
    /// Chord → every claimant id claiming it (registered or conflicted), in claim order.
    pub claims: HashMap<Chord, Vec<String>>,
    /// The ids holding at least one conflicted (unregistered) chord.
    pub conflicted: BTreeSet<String>,
}

impl<B: Binding> Default for Keymap<B> {
    fn default() -> Self {
        resolve(&BTreeMap::new(), &B::default_claimants())
    }
}

/// A pending claim: chord, claimant id, payload.
type Claims<B> = Vec<(Chord, String, B)>;

impl<B: Binding> Keymap<B> {
    /// What `c` runs.
    pub fn get(&self, c: &Chord) -> Option<&B> {
        self.map.get(c)
    }

    /// Whether the focused source editor must hand `c` to the app instead of handling it:
    /// chords bound to a [`Binding::routed`] payload, Ctrl+, and the inert Ctrl+Shift+,.
    pub fn routed(&self, c: &Chord) -> bool {
        self.map.get(c).is_some_and(Binding::routed) || (c.command && c.key == ",")
    }

    /// Chord → config action id.
    pub fn actions(&self) -> HashMap<Chord, String> {
        self.map
            .iter()
            .filter_map(|(c, b)| b.action_id().map(|id| (c.clone(), id.to_owned())))
            .collect()
    }
}

impl<B: Clone> Keymap<B> {
    fn empty() -> Self {
        Self {
            map: HashMap::new(),
            notices: Vec::new(),
            chords: HashMap::new(),
            row_notices: HashMap::new(),
            overridden: BTreeSet::new(),
            claims: HashMap::new(),
            conflicted: BTreeSet::new(),
        }
    }

    fn note(&mut self, id: &str, n: String) {
        self.row_notices
            .entry(id.to_owned())
            .or_default()
            .push(n.clone());
        self.notices.push(n);
    }

    /// Checks `keys` under `policy` and records `id`'s claim on each accepted chord (a chord
    /// listed twice by one claimant is collapsed).
    pub fn claim_keys(
        &mut self,
        claims: &mut Claims<B>,
        id: &str,
        keys: &[String],
        run: &B,
        policy: Policy,
    ) {
        for k in keys {
            let Some(c) = chord::parse(k) else {
                if policy != Policy::Default {
                    self.note(id, format!("{id}: unreadable key \"{k}\""));
                }
                continue;
            };
            if policy != Policy::Default && chord::text_editing(&c) {
                self.note(id, format!("{id}: {c} is reserved (not bound)"));
                continue;
            }
            if policy == Policy::Override && !c.command && !c.alt {
                self.note(id, format!("{id}: {c} needs Ctrl or Alt (not bound)"));
                continue;
            }
            if claims.iter().any(|(x, i, _)| *x == c && i == id) {
                continue;
            }
            self.claims
                .entry(c.clone())
                .or_default()
                .push(id.to_owned());
            claims.push((c, id.to_owned(), run.clone()));
        }
    }
}

/// Resolves the effective keymap from `[keys]` `overrides` (id → chords; `[]` unbinds) and the
/// `claimants`, in claim order. Malformed and reserved ([`chord::text_editing`]) chords become
/// notices; a chord claimed by two or more distinct claimants is registered for none of them,
/// every claimant is marked [`Keymap::conflicted`] and gets a notice naming the chord.
pub fn resolve<B: Clone>(
    overrides: &BTreeMap<String, Vec<String>>,
    claimants: &[Claimant<B>],
) -> Keymap<B> {
    let mut km = Keymap::empty();
    for id in overrides.keys() {
        let known = claimants
            .iter()
            .any(|r| matches!(r.kind, ClaimKind::Bindable { .. }) && &r.id == id);
        if !known {
            km.note(id, format!("{id}: unknown command id (override ignored)"));
        }
    }
    let mut claims: Claims<B> = Vec::new();
    for r in claimants {
        km.chords.entry(r.id.clone()).or_default();
        let fixed = match r.kind {
            ClaimKind::Config => {
                km.claim_keys(&mut claims, &r.id, &r.keys, &r.run, Policy::Config);
                continue;
            }
            ClaimKind::Bindable { fixed } => fixed,
        };
        let over = overrides.get(&r.id);
        if over.is_some() && fixed {
            km.note(
                &r.id,
                format!("{}: a text-editing command (override ignored)", r.id),
            );
        }
        if let Some(list) = over.filter(|_| !fixed) {
            km.overridden.insert(r.id.clone());
            km.claim_keys(&mut claims, &r.id, list, &r.run, Policy::Override);
        } else if fixed {
            // Handled by the source editor's own bindings; shown, never routed.
            let shown = r.keys.iter().filter_map(|k| chord::parse(k));
            let shown: Vec<String> = shown.map(|c| c.to_string()).collect();
            km.chords.entry(r.id.clone()).or_default().extend(shown);
        } else {
            km.claim_keys(&mut claims, &r.id, &r.keys, &r.run, Policy::Default);
        }
    }
    // Register the uncontested chords; a contested chord is registered for nobody.
    for (c, id, b) in claims {
        let others: Vec<String> = km
            .claims
            .get(&c)
            .into_iter()
            .flatten()
            .filter(|o| **o != id)
            .cloned()
            .collect();
        if others.is_empty() {
            km.chords.entry(id).or_default().push(c.to_string());
            km.map.insert(c, b);
        } else {
            km.conflicted.insert(id.clone());
            let n = format!("{id}: {c} conflicts with {} (not bound)", others.join(", "));
            km.note(&id, n);
        }
    }
    km
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Run {
        Cmd(&'static str),
        Action(String),
    }

    impl Binding for Run {
        fn routed(&self) -> bool {
            matches!(self, Run::Cmd(_))
        }
        fn action_id(&self) -> Option<&str> {
            match self {
                Run::Action(a) => Some(a),
                Run::Cmd(_) => None,
            }
        }
        fn default_claimants() -> Vec<Claimant<Self>> {
            vec![
                bindable("save", &["Ctrl+S"], true),
                bindable("quit", &["Ctrl+Q"], false),
            ]
        }
    }

    fn bindable(id: &'static str, keys: &[&str], fixed: bool) -> Claimant<Run> {
        Claimant {
            id: id.into(),
            keys: keys.iter().map(|k| (*k).to_owned()).collect(),
            run: Run::Cmd(id),
            kind: ClaimKind::Bindable { fixed },
        }
    }

    fn config(id: &str, keys: &[&str]) -> Claimant<Run> {
        Claimant {
            id: id.into(),
            keys: keys.iter().map(|k| (*k).to_owned()).collect(),
            run: Run::Action(id.into()),
            kind: ClaimKind::Config,
        }
    }

    fn c(s: &str) -> Chord {
        chord::parse(s).expect("chord")
    }

    #[test]
    fn default_keymap_binds_defaults_and_shows_fixed() {
        let km: Keymap<Run> = Keymap::default();
        assert_eq!(km.get(&c("Ctrl+Q")), Some(&Run::Cmd("quit")));
        assert_eq!(km.get(&c("Ctrl+S")), None, "fixed: shown, never routed");
        assert_eq!(km.chords["save"], ["Ctrl+S"]);
        assert!(km.routed(&c("Ctrl+Q")) && km.routed(&c("Ctrl+,")));
        assert!(km.notices.is_empty());
    }

    #[test]
    fn conflicts_overrides_and_notices() {
        let mut over = BTreeMap::new();
        over.insert("quit".to_owned(), vec!["Ctrl+J".to_owned(), "J".to_owned()]);
        over.insert("save".to_owned(), vec!["Ctrl+K".to_owned()]);
        over.insert("nope".to_owned(), vec![]);
        let rows = vec![
            bindable("save", &["Ctrl+S"], true),
            bindable("quit", &["Ctrl+Q"], false),
            config("a", &["Ctrl+J", "Ctrl+Q", "Ctrl+Z", "Bad+X", "Ctrl+Q"]),
        ];
        let km = resolve(&over, &rows);
        assert_eq!(km.get(&c("Ctrl+J")), None, "contested chord binds nobody");
        assert_eq!(km.get(&c("Ctrl+Q")), Some(&Run::Action("a".into())));
        assert_eq!(km.actions().len(), 1);
        assert!(km.overridden.contains("quit") && !km.overridden.contains("save"));
        assert!(km.conflicted.contains("quit") && km.conflicted.contains("a"));
        assert_eq!(km.claims[&c("Ctrl+Q")], ["a"], "duplicate claim collapsed");
        assert_eq!(
            km.notices,
            [
                "nope: unknown command id (override ignored)",
                "save: a text-editing command (override ignored)",
                "quit: J needs Ctrl or Alt (not bound)",
                "a: Ctrl+Z is reserved (not bound)",
                "a: unreadable key \"Bad+X\"",
                "quit: Ctrl+J conflicts with a (not bound)",
                "a: Ctrl+J conflicts with quit (not bound)",
            ]
        );
        assert_eq!(km.row_notices["a"].len(), 3);
    }
}
