//! Form input: [`FormMsg`] and its one reducer ([`Settings::form_msg_paths`]), and the active
//! typeahead ([`form::ActiveTa`]) — keys, commits and the typed [`PickValue`] they write.

use cv_schema::Node;

use super::form::{self, ActiveTa, TaTarget, draft_key};
use super::pickers::{self, PickValue, Picker};
use super::{DocEdit, ScalarValue, Settings, SettingsMsg};
use crate::message::Message;
use crate::widget::typeahead::{Candidate, Key, Outcome, Status};

impl Settings {
    /// A form UI message on `file` (drafts, expansion, typed input that may edit the doc),
    /// with no vault path list (path / folder typeaheads see no candidates).
    pub fn form_msg(&mut self, file: &str, m: FormMsg) {
        self.form_msg_paths(file, m, &[]);
    }

    /// The visible candidates of the active typeahead (plan round-5 WP-4).
    pub fn ta_visible(&self, paths: &[String]) -> Vec<Candidate> {
        self.form.typeahead.as_ref().map_or_else(Vec::new, |a| {
            pickers::visible(self, a.picker, &a.file, &a.path, paths, &a.draft)
        })
    }

    /// The active typeahead's status line (plan round-5 WP-6 async sources).
    pub fn ta_status(&self) -> Status {
        self.form
            .typeahead
            .as_ref()
            .map_or_else(Default::default, |a| {
                pickers::status(self, a.picker, &a.file, &a.path, &a.draft)
            })
    }

    /// A key on the active typeahead (Up/Down/Enter/Escape; Escape doubles as blur).
    pub fn typeahead_key(&mut self, key: Key, paths: &[String]) {
        let Some(mut a) = self.form.typeahead.take() else {
            return;
        };
        let text = a.draft.clone();
        let visible = pickers::visible(self, a.picker, &a.file, &a.path, paths, &text);
        let valid = pickers::is_valid(self, a.picker, &a.file, &a.path, paths, &text);
        // Enter on an open list commits the highlighted candidate (a typed value otherwise).
        let from_candidate =
            key == Key::Enter && a.ta.open && visible.get(a.ta.highlight).is_some();
        let out = a.ta.key(key, &text, &visible, valid);
        let close = !a.ta.open;
        self.apply_ta(&mut a, out, from_candidate);
        if !close {
            self.form.typeahead = Some(a);
        }
    }

    /// Closes the active typeahead as a blur (Escape semantics), when it is not `(file, path)`.
    fn blur_other(&mut self, file: &str, path: &cv_schema::Path, paths: &[String]) {
        if self
            .form
            .typeahead
            .as_ref()
            .is_some_and(|a| !a.is(file, path))
        {
            self.typeahead_key(Key::Escape, paths);
            self.form.typeahead = None;
        }
    }

    /// The typed value of a commit: a picked place candidate resolves to its hit.
    fn pick_value(&self, a: &ActiveTa, v: String, from_candidate: bool) -> PickValue {
        if from_candidate
            && a.picker == Picker::Place
            && let Some(p) = pickers::place_pick(self, &a.draft, &v)
        {
            return p;
        }
        PickValue::Str(v)
    }

    /// Writes a typeahead outcome (`from_candidate`: a commit of a listed candidate).
    fn apply_ta(&mut self, a: &mut ActiveTa, out: Outcome, from_candidate: bool) {
        let file = a.file.clone();
        if a.target == TaTarget::Chips {
            let add = |s: &mut Self, a: &mut ActiveTa, value| {
                a.ta.committed.clear();
                a.draft.clear();
                s.form_msg(
                    &file,
                    FormMsg::ChipAdd {
                        path: a.path.clone(),
                        value,
                    },
                );
            };
            match out {
                Outcome::Commit(v) => add(self, a, Some(v)),
                Outcome::Restore(_) => {
                    a.draft.clear();
                    self.form.set_draft(&file, draft_key("chip", &a.path), None);
                }
                // A valid typed id: add it as a chip.
                Outcome::None if !a.ta.open && !a.draft.is_empty() => add(self, a, None),
                Outcome::None => {}
            }
            return;
        }
        let v = match out {
            Outcome::Commit(v) => self.pick_value(a, v, from_candidate),
            Outcome::Restore(v) => PickValue::Str(v),
            Outcome::None if !a.ta.open => PickValue::Str(a.draft.clone()),
            Outcome::None => return,
        };
        let TaTarget::Text { required } = a.target else {
            return;
        };
        match v {
            PickValue::Place { label, lat, lon } => {
                // A geocoded pick: label + sibling lat/lon in one edit.
                let parent = cv_schema::Path(a.path.0[..a.path.0.len().saturating_sub(1)].to_vec());
                a.draft.clone_from(&label);
                a.ta.committed.clone_from(&label);
                let e = DocEdit::SetMany(vec![
                    (a.path.clone(), ScalarValue::Str(label)),
                    (parent.key("lat"), ScalarValue::Float(lat)),
                    (parent.key("lon"), ScalarValue::Float(lon)),
                ]);
                let _ = self.edit(&file, &e);
            }
            PickValue::Str(v) => {
                a.draft.clone_from(&v);
                if self.stored_str(&file, &a.path) != v {
                    let e = self.text_edit(&file, a.path.clone(), v, required);
                    let _ = self.edit(&file, &e);
                }
            }
        }
    }

    /// Typing into a typeahead field: (re)opens it on `(file, path)` with `text` as its draft.
    fn ta_typed(
        &mut self,
        file: &str,
        path: cv_schema::Path,
        picker: Picker,
        target: TaTarget,
        text: String,
        paths: &[String],
    ) {
        self.blur_other(file, &path, paths);
        let committed = match target {
            TaTarget::Text { .. } => self.stored_str(file, &path),
            TaTarget::Chips => String::new(),
        };
        self.lookups.reopened |= self.form.typeahead.as_ref().is_none_or(|a| !a.ta.open);
        let a = self
            .form
            .typeahead
            .get_or_insert_with(|| ActiveTa::new(file, path, picker, target, committed));
        a.draft = text;
        a.ta.typed();
    }

    /// [`Self::form_msg`] with the vault path list (path / folder typeahead candidates): the
    /// one form reducer.
    pub fn form_msg_paths(&mut self, file: &str, m: FormMsg, paths: &[String]) {
        let edit = match m {
            FormMsg::Text {
                path,
                text,
                required,
                picker: Some(picker),
            } => {
                let target = TaTarget::Text { required };
                self.ta_typed(file, path, picker, target, text, paths);
                None
            }
            FormMsg::Text {
                path,
                text,
                required,
                picker: None,
            } => Some(self.text_edit(file, path, text, required)),
            FormMsg::ChipDraft { path, text, picker } => {
                // The chip row shows (and a plain adder adds) the form draft; a typeahead
                // adder keeps its query in the active typeahead too.
                let key = draft_key("chip", &path);
                if let Some(picker) = picker {
                    self.ta_typed(file, path, picker, TaTarget::Chips, text.clone(), paths);
                }
                self.form.set_draft(file, key, Some(text));
                None
            }
            FormMsg::Pick { path, value } => {
                self.blur_other(file, &path, paths);
                let mut a = self.form.typeahead.take().unwrap_or_else(|| {
                    ActiveTa::new(
                        file,
                        path,
                        Picker::Path,
                        TaTarget::Text { required: false },
                        "",
                    )
                });
                let out = a.ta.pick(value);
                self.apply_ta(&mut a, out, true);
                None
            }
            FormMsg::RefreshModels { path, picker } => {
                let text = self
                    .form
                    .typeahead
                    .as_ref()
                    .filter(|a| a.is(file, &path))
                    .map(|a| a.draft.clone())
                    .unwrap_or_default();
                if let Some(p) = pickers::model_provider(self, picker, file, &path, &text) {
                    self.lookups.refresh_models(p);
                }
                None
            }
            FormMsg::Clear { path, required } => {
                self.blur_other(file, &path, paths);
                self.form.typeahead = None;
                Some(self.text_edit(file, path, String::new(), required))
            }
            FormMsg::Toggle(p) => {
                self.form.toggle(file, &p);
                None
            }
            FormMsg::Number { path, text, float } => {
                self.form
                    .set_draft(file, draft_key("num", &path), Some(text.clone()));
                if text.trim().is_empty() {
                    self.present(file, &path).then_some(DocEdit::Remove(path))
                } else {
                    form::parse_number(&text, float).map(|v| match v.as_integer() {
                        Some(i) => DocEdit::Set(path, ScalarValue::Int(i)),
                        None => DocEdit::Set(path, ScalarValue::Float(v.as_float().unwrap_or(0.0))),
                    })
                }
            }
            FormMsg::Step { path, value } => {
                self.form.set_draft(file, draft_key("num", &path), None);
                Some(DocEdit::Set(path, value))
            }
            FormMsg::ChipAdd { path, value } => {
                let key = draft_key("chip", &path);
                let v = value.or_else(|| self.form.draft(file, &key).map(str::to_owned));
                self.form.set_draft(file, key, None);
                if let Some(a) = self.form.typeahead.as_mut()
                    && a.is(file, &path)
                    && a.target == TaTarget::Chips
                {
                    a.draft.clear();
                }
                let v = v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
                v.map(|v| {
                    let mut items = self.str_list(file, &path);
                    items.push(v);
                    DocEdit::Set(path, ScalarValue::StrList(items))
                })
            }
            FormMsg::ChipRemove { path, index } => {
                let mut items = self.str_list(file, &path);
                (index < items.len()).then(|| {
                    items.remove(index);
                    DocEdit::Set(path, ScalarValue::StrList(items))
                })
            }
            FormMsg::NewKeyDraft { map, text } => {
                self.form
                    .set_draft(file, draft_key("newkey", &map), Some(text));
                None
            }
            FormMsg::NewKeyAdd { map, value } => {
                let key = draft_key("newkey", &map);
                let name = self
                    .form
                    .draft(file, &key)
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if name.is_empty() {
                    None
                } else {
                    self.form.set_draft(file, key, None);
                    Some(DocEdit::InsertKey(map, name, value))
                }
            }
            FormMsg::RenameDraft { at, text } => {
                self.form
                    .set_draft(file, draft_key("rename", &at), Some(text));
                None
            }
            FormMsg::RenameCommit { map, from } => {
                let key = draft_key("rename", &map.key(&from));
                let to = self
                    .form
                    .draft(file, &key)
                    .unwrap_or(&from)
                    .trim()
                    .to_owned();
                self.form.set_draft(file, key, None);
                (!to.is_empty() && to != from).then_some(DocEdit::RenameKey(map, from, to))
            }
        };
        if let Some(e) = edit {
            let _ = self.edit(file, &e);
        }
    }
}

/// Plan round-5 WP-4: the active typeahead's keys — Up/Down, and Escape whether or not the
/// focused input captured it (a captured Escape also unfocuses: blur = Escape).
pub fn typeahead_key_event(
    ev: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    use Key as K;
    use iced::keyboard::{self, key::Named};
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) = ev else {
        return None;
    };
    let k = match key.as_ref() {
        keyboard::Key::Named(Named::ArrowUp) => K::Up,
        keyboard::Key::Named(Named::ArrowDown) => K::Down,
        keyboard::Key::Named(Named::Escape) => K::Escape,
        _ => return None,
    };
    Some(Message::Settings(SettingsMsg::Typeahead(k)))
}

/// Form UI messages (typing, expansion); see [`Settings::form_msg`].
#[derive(Debug, Clone)]
pub enum FormMsg {
    /// Expand / collapse a list item or map entry.
    Toggle(cv_schema::Path),
    /// A text field changed (empty + optional removes the key).
    Text {
        path: cv_schema::Path,
        text: String,
        required: bool,
        /// A typeahead field: the text is a query, written on commit (plan round-5 WP-4).
        picker: Option<Picker>,
    },
    /// A typeahead candidate was clicked: store `value`.
    Pick {
        path: cv_schema::Path,
        value: String,
    },
    /// A typeahead field's `x`: clear the committed value.
    Clear {
        path: cv_schema::Path,
        required: bool,
    },
    /// A number field changed (applied when it parses).
    Number {
        path: cv_schema::Path,
        text: String,
        float: bool,
    },
    /// A stepper button of a bounded number: write `value`, drop the typed draft.
    Step {
        path: cv_schema::Path,
        value: ScalarValue,
    },
    ChipDraft {
        path: cv_schema::Path,
        text: String,
        /// A typeahead chips field (plan round-5 WP-4).
        picker: Option<Picker>,
    },
    /// The refresh control beside a model field: re-fetch its provider's live list (plan
    /// round-6-feedback-todos WP-8).
    RefreshModels {
        path: cv_schema::Path,
        picker: Picker,
    },
    /// Add a chip (`None`: the draft).
    ChipAdd {
        path: cv_schema::Path,
        value: Option<String>,
    },
    ChipRemove {
        path: cv_schema::Path,
        index: usize,
    },
    NewKeyDraft {
        map: cv_schema::Path,
        text: String,
    },
    NewKeyAdd {
        map: cv_schema::Path,
        /// The map's entry node.
        value: &'static Node,
    },
    RenameDraft {
        at: cv_schema::Path,
        text: String,
    },
    RenameCommit {
        map: cv_schema::Path,
        from: String,
    },
}
