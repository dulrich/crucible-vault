//! Per-pane back/forward history (plan editor-usability-round WP-3). Pure: no iced types.
//!
//! Every open into a pane (link, switcher, explorer, nav) [`History::push`]es; back/forward move a
//! cursor and never push. Bounded at [`CAP`], no duplicate consecutive entries, a push after
//! going back drops the forward entries. Not persisted.

/// The per-pane entry bound.
pub const CAP: usize = 50;

/// One pane's visited paths and the cursor (index of the current entry).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    entries: Vec<String>,
    cursor: usize,
}

impl History {
    /// Records a visit to `path` (no-op when it is already the current entry).
    pub fn push(&mut self, path: &str) {
        if self.current() == Some(path) {
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.cursor + 1);
        }
        self.entries.push(path.to_owned());
        if self.entries.len() > CAP {
            let over = self.entries.len() - CAP;
            self.entries.drain(..over);
        }
        self.cursor = self.entries.len() - 1;
    }

    /// The current entry.
    pub fn current(&self) -> Option<&str> {
        self.entries.get(self.cursor).map(String::as_str)
    }

    /// Whether [`History::back`] has somewhere to go.
    pub fn can_back(&self) -> bool {
        self.cursor > 0
    }

    /// Whether [`History::forward`] has somewhere to go.
    pub fn can_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// Steps back; returns the new current entry.
    pub fn back(&mut self) -> Option<&str> {
        if !self.can_back() {
            return None;
        }
        self.cursor -= 1;
        self.current()
    }

    /// Steps forward; returns the new current entry.
    pub fn forward(&mut self) -> Option<&str> {
        if !self.can_forward() {
            return None;
        }
        self.cursor += 1;
        self.current()
    }

    /// A rename: every `from` entry becomes `to`, then consecutive duplicates collapse.
    pub fn retarget(&mut self, from: &str, to: &str) {
        for e in &mut self.entries {
            if e == from {
                to.clone_into(e);
            }
        }
        let mut out: Vec<String> = Vec::with_capacity(self.entries.len());
        let mut cursor = 0;
        for (i, e) in self.entries.drain(..).enumerate() {
            if out.last() != Some(&e) {
                out.push(e);
            }
            if i == self.cursor {
                cursor = out.len().saturating_sub(1);
            }
        }
        self.entries = out;
        self.cursor = cursor;
    }

    /// The entries, oldest first (tests).
    pub fn entries(&self) -> &[String] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_back_forward_and_truncate() {
        let mut h = History::default();
        assert!(!h.can_back() && !h.can_forward());
        h.push("a");
        h.push("b");
        h.push("c");
        assert_eq!(h.back(), Some("b"));
        assert_eq!(h.back(), Some("a"));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some("b"));
        h.push("d");
        assert!(!h.can_forward());
        assert_eq!(h.entries(), ["a", "b", "d"]);
    }

    #[test]
    fn dedupes_consecutive_and_bounds() {
        let mut h = History::default();
        h.push("a");
        h.push("a");
        assert_eq!(h.entries().len(), 1);
        for i in 0..(CAP + 10) {
            h.push(&format!("n{i}"));
        }
        assert_eq!(h.entries().len(), CAP);
        assert_eq!(h.current(), Some(format!("n{}", CAP + 9).as_str()));
    }

    #[test]
    fn retarget_rewrites_and_collapses() {
        let mut h = History::default();
        for p in ["a", "b", "c", "b"] {
            h.push(p);
        }
        h.back();
        h.retarget("b", "c");
        // a, c, c, c -> a, c ; cursor was on "c" (index 2) -> index 1.
        assert_eq!(h.entries(), ["a", "c"]);
        assert_eq!(h.current(), Some("c"));
    }
}
