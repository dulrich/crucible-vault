//! Plan WP-R4 (review E1, E2): where an open note lives, settled in one place and off the UI
//! thread. [`settle`] is the one daemon-side question (run through [`App::rpc`]); [`App::apply_settled`]
//! is the one UI-side effect. Note opens (bind + read) complete the same way: a tab appears when
//! its open completes. Every completion carries a freshness key and a stale one is dropped.

use super::*;
use crate::daemon::{ContentHash, NoteId, NoteLocation};

/// Where note `id` is relative to the path its document had when the question was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Live at the document's path.
    Here,
    /// Live elsewhere: the document follows.
    Moved(String),
    /// Retired, absorbed by the note now live at this path.
    Absorbed(String),
    /// Retired or unknown, with no live replacement.
    Gone,
    /// The daemon did not answer (disconnected, an error): the document is left as it is.
    NoAnswer,
}

/// The one daemon-side interpreter of a [`NoteLocation`] (blocking: run it off the UI thread).
pub(crate) fn settle(d: &dyn DaemonClient, id: NoteId, cur: &str) -> Settled {
    match d.locate_note(id) {
        Ok(NoteLocation::Live { path }) if path == cur => Settled::Here,
        Ok(NoteLocation::Live { path }) => Settled::Moved(path),
        Ok(NoteLocation::Gone {
            replaced_by: Some(by),
        }) => match d.locate_note(by) {
            Ok(NoteLocation::Live { path }) => Settled::Absorbed(path),
            _ => Settled::Gone,
        },
        Ok(NoteLocation::Gone { .. } | NoteLocation::Unknown) => Settled::Gone,
        Err(_) => Settled::NoAnswer,
    }
}

/// What the site that asked does once [`App::apply_settled`] ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterSettle {
    /// A `Deleted` at the document's path: no answer → re-read the path as before.
    Deleted { hash: Option<String> },
    /// A change at `path`, not the document's: reload once the document is at `path`.
    Changed { path: String, hash: Option<String> },
    /// `reload_doc`'s read found the path gone: still here → re-read once; no answer → `err`.
    Reread {
        hash: Option<String>,
        err: Option<String>,
    },
    /// A followed move found `failed` gone: an answer naming `failed` again stops there.
    Follow { failed: String },
    /// A reconnect: re-read and resync at the document's path unless the note is gone.
    Resync,
}

/// A finished open: the bound id and the bytes read for it (never logged).
#[derive(Clone)]
pub struct Opened {
    pub id: NoteId,
    pub bytes: Arc<Vec<u8>>,
    pub hash: ContentHash,
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opened")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// What an open ([`App::start_open`]) is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenFor {
    /// A tab in `pane` per `how`; `record` = push the visit onto the pane's history.
    Tab {
        pane: Pane,
        how: Place,
        record: bool,
        then: OpenThen,
    },
    /// The absorbing note of retired document `0` ([`App::follow_replaced`]).
    Absorb(NoteId),
}

/// What a tab open does once its tab is placed (inline when the note was already open).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenThen {
    Nothing,
    /// Jump to the heading `0` (a link's fragment).
    Heading(String),
    /// Go to 0-based line `0` (a view row).
    Line(usize),
    /// A nav link: a missing note is created (plan round-11 WP-7) with the link's seed `0`
    /// (review E7).
    CreateMissing(String),
}

/// Why an open failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenFailed {
    NotFound,
    Other(String),
}

impl OpenFor {
    fn absorbs(&self) -> Option<NoteId> {
        match self {
            Self::Tab { .. } => None,
            Self::Absorb(id) => Some(*id),
        }
    }
}

/// Opens in flight by `(path, absorbed id)`: the request number the completion must carry.
pub(super) type Opening = std::collections::HashMap<(String, Option<NoteId>), u64>;

impl App {
    /// Queues `t` to be returned from the current `update` (helpers that return nothing).
    pub(super) fn defer(&mut self, t: Task<Message>) {
        if t.units() > 0 {
            self.deferred.push(t);
        }
    }

    fn next_req(&mut self) -> u64 {
        self.req_seq += 1;
        self.req_seq
    }

    /// Asks where document `id` lives, off the UI thread; `then` runs on the answer. The request
    /// number goes on the document: only the newest settle of this document instance applies.
    pub(super) fn settle_doc(&mut self, id: NoteId, then: AfterSettle) {
        let req = self.next_req();
        let Some(od) = self.docs.by_id_mut(id) else {
            return;
        };
        od.settle = req;
        let from = od.buffer.path().to_owned();
        let cur = from.clone();
        let t = self.rpc(
            move |d| Ok(settle(d, id, &cur)),
            move |r| Message::Settled {
                id,
                req,
                from,
                settled: r.unwrap_or(Settled::NoAnswer),
                then,
            },
        );
        self.defer(t);
    }

    /// A settle completion. Freshness key: the document instance's newest request number and
    /// the path it had when asked — a closed (or closed and reopened) document, a newer settle,
    /// or a move applied meanwhile drops it.
    pub(super) fn settled(
        &mut self,
        id: NoteId,
        req: u64,
        from: &str,
        s: Settled,
        then: AfterSettle,
    ) -> Task<Message> {
        if !self
            .docs
            .by_id(id)
            .is_some_and(|d| d.settle == req && d.buffer.path() == from)
        {
            return Task::none();
        }
        match (&then, &s) {
            (AfterSettle::Follow { failed }, Settled::Moved(p)) if p == failed => {
                return Task::none();
            }
            (AfterSettle::Follow { failed }, Settled::NoAnswer) => {
                self.status
                    .set_error(open_error(&DaemonError::NotFound(failed.clone())));
                return Task::none();
            }
            _ => {}
        }
        let answered = s != Settled::NoAnswer;
        let live = matches!(s, Settled::Here | Settled::Moved(_) | Settled::NoAnswer);
        let here = s == Settled::Here;
        self.apply_settled(id, s);
        match then {
            AfterSettle::Deleted { hash } if !answered => self.reload_doc(id, hash.as_deref()),
            AfterSettle::Changed { path, hash }
                if self.docs.path_of(id).as_deref() == Some(path.as_str()) =>
            {
                self.reload_doc(id, hash.as_deref());
            }
            AfterSettle::Reread { hash, .. } if here => self.reread_doc(id, hash.as_deref()),
            AfterSettle::Reread { err: Some(e), .. } if !answered => self.status.set_error(e),
            AfterSettle::Resync if live => return self.resync_read(id),
            _ => {}
        }
        Task::none()
    }

    /// The one UI-side effect of a [`Settled`] answer for document `id`.
    pub(super) fn apply_settled(&mut self, id: NoteId, s: Settled) {
        match s {
            Settled::Here | Settled::NoAnswer => {}
            Settled::Moved(to) => {
                if let Some(o) = self.docs.by_id_mut(id) {
                    o.buffer.clear_missing();
                }
                self.follow_doc(id, &to, super::identity::Follow::Daemon);
            }
            Settled::Absorbed(to) => {
                if !self.follow_replaced(id, &to) {
                    self.doc_gone(id);
                }
            }
            Settled::Gone => self.doc_gone(id),
        }
    }

    /// Opens note `path` off the UI thread: bind + read ([`super::tabs::bind_read_note`]), then
    /// [`Self::note_opened`] finishes it. A newer open of the same path for the same purpose
    /// supersedes this one.
    pub(super) fn start_open(&mut self, path: &str, purpose: OpenFor) {
        let req = self.next_req();
        self.opening
            .insert((path.to_owned(), purpose.absorbs()), req);
        let (p, path) = (path.to_owned(), path.to_owned());
        let t = self.rpc(
            move |d| {
                Ok(super::tabs::bind_read_note(d, &p)
                    .map(|(id, bytes, hash)| Opened {
                        id,
                        bytes: Arc::new(bytes),
                        hash,
                    })
                    .map_err(|e| match e {
                        DaemonError::NotFound(_) => OpenFailed::NotFound,
                        e => OpenFailed::Other(open_error(&e)),
                    }))
            },
            move |r| Message::NoteOpened {
                path,
                req,
                purpose,
                result: r.unwrap_or_else(|e| Err(OpenFailed::Other(e))),
            },
        );
        self.defer(t);
    }

    /// An open completion. Freshness key: the newest open request for `(path, purpose)` (a
    /// reopen supersedes it). A tab open whose target pane closed meanwhile is cancelled, and a
    /// document opened for the note meanwhile is joined, never duplicated. An absorb applies
    /// only while the retired document is open and clean.
    pub(super) fn note_opened(
        &mut self,
        path: &str,
        req: u64,
        purpose: OpenFor,
        result: Result<Opened, OpenFailed>,
    ) {
        let key = (path.to_owned(), purpose.absorbs());
        if self.opening.get(&key) != Some(&req) {
            return;
        }
        self.opening.remove(&key);
        match purpose {
            OpenFor::Tab {
                pane,
                how,
                record,
                then,
            } => {
                if self.panes.get(pane).is_none() {
                    return;
                }
                let o = match result {
                    Ok(o) => o,
                    Err(OpenFailed::NotFound) if matches!(then, OpenThen::CreateMissing(_)) => {
                        self.focus = pane;
                        let OpenThen::CreateMissing(content) = then else {
                            return;
                        };
                        let t = self.dispatch_op(FileOp::Create {
                            path: path.to_owned(),
                            content,
                        });
                        return self.defer(t);
                    }
                    Err(OpenFailed::NotFound) => {
                        let e = DaemonError::NotFound(path.to_owned());
                        return self.status.set_error(open_error(&e));
                    }
                    Err(OpenFailed::Other(e)) => return self.status.set_error(e),
                };
                let path = match self.docs.path_of(o.id) {
                    Some(p) => p,
                    None => {
                        if !self.docs.contains_key(path) {
                            self.insert_doc(o.id, path, &o.bytes, o.hash);
                        }
                        path.to_owned()
                    }
                };
                let nav = std::mem::replace(&mut self.history_nav, !record);
                let r = self.open_then(pane, &path, how, then);
                self.history_nav = nav;
                match r {
                    Ok(t) => self.defer(t),
                    Err(e) => self.status.set_error(open_error(&e)),
                }
            }
            OpenFor::Absorb(rid) => {
                if self.docs.by_id(rid).is_none_or(|d| d.buffer.is_dirty()) {
                    return;
                }
                let absorber = match result {
                    _ if self.docs.id_of(path).is_some() => None,
                    Ok(o) if self.docs.by_id(o.id).is_none() => {
                        let buffer = crate::source::SourceBuffer::new(o.id, path, &o.bytes, o.hash);
                        Some(OpenDoc {
                            id: o.id,
                            buffer,
                            close_waiters: Vec::new(),
                            reading: crate::reading::DocReading::default(),
                            settle: 0,
                        })
                    }
                    Ok(_) => None,
                    Err(e) => {
                        let e = match e {
                            OpenFailed::NotFound => open_error(&DaemonError::NotFound(path.into())),
                            OpenFailed::Other(e) => e,
                        };
                        self.status.set_error(e);
                        return self.doc_gone(rid);
                    }
                };
                if !self.absorb(rid, path, absorber) {
                    self.doc_gone(rid);
                }
            }
        }
    }

    /// The continuation of a tab open of `path`, once its tab is placed.
    pub(super) fn open_continue(&mut self, path: &str, then: OpenThen) -> Task<Message> {
        match then {
            OpenThen::Heading(f) => self.jump_to_heading(path, &f),
            OpenThen::Line(line) if self.active_note().is_some_and(|t| t.path == path) => {
                self.jump_to_line(path, line)
            }
            _ => Task::none(),
        }
    }

    /// A new document for note `id` read at `path`.
    pub(super) fn insert_doc(&mut self, id: NoteId, path: &str, bytes: &[u8], hash: ContentHash) {
        let buffer = crate::source::SourceBuffer::new(id, path, bytes, hash);
        let mode = self.initial_mode(path);
        let mut od = OpenDoc {
            id,
            buffer,
            close_waiters: Vec::new(),
            reading: crate::reading::DocReading::default(),
            settle: 0,
        };
        self.reading
            .register(&mut od, mode, !self.restoring, self.daemon.as_ref());
        self.docs.insert(od);
    }
}
