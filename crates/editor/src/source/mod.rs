//! Source editor: highlighting, own undo/redo, save/conflict (plan `v0-editor.md` WP-2).
//!
//! [`SourceBuffer`] wraps an iced `text_editor::Content` and routes every editing `Action`
//! through `editor_model::undo` (iced 0.14 has no undo). Saves go through
//! [`DaemonClient::write_note`] only (D1), CAS against the buffer's base hash, off the UI thread.
//!
//! External changes replace Crucible's `ChainManager.reconcileOpenEditor`
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/chains.ts:212`, inventory §3): there the plugin had to diff an
//! editor it didn't own after its own chains wrote the file; here the daemon notifies the editor
//! (`file_changed`) and the editor decides — clean buffer reloads silently keeping the cursor
//! line, dirty buffer shows a conflict bar. The Obsidian-induced part (polling the open editor
//! after a plugin write) is dropped.

mod buffer;
mod edit;
pub mod highlighter;
mod keys;
mod place;
mod save;
mod view;

use std::sync::Arc;
use std::time::Instant;

use iced::widget::text_editor::Action;
use iced::{Point, Task, clipboard};

use crate::daemon::DaemonClient;
use crate::message::{Message, SourceKey, SourceMsg};

pub use buffer::SourceBuffer;
pub use edit::{OUTDENT_SPACES, outdent_width};
pub use highlighter::{
    FindSpan, KindColors, MarkdownHighlighter, Paint, Settings as HighlightSettings,
};
pub use keys::{command_click_msg, shift_click};
#[cfg(test)]
pub(crate) use keys::{test_key_binding, test_key_binding_link, test_key_binding_with};
pub use place::{EDGE_SCROLL, Relayout, VISIBLE_LINES, rows_at, viewport_lines};
#[cfg(test)]
pub(crate) use place::{EditorOps, fresh_view, scroll_content};
pub use save::{AUTOSAVE_DELAY, Conflict, SaveJob, SaveOutcome, file_changed};
pub(crate) use save::{autosave_timer, flush, spawn_save};
pub(crate) use view::EDITOR_PADDING;
pub use view::{SLOTS, editor_id, view};

/// What [`update`] asks the app to do (WP-R15 SP5: the one effect shape `App` drains, as
/// `views::Effect`).
pub enum Effect {
    /// Run this task (saves, clipboard, timers, follow-up messages).
    Task(Task<Message>),
    /// An error notice for the status bar.
    Error(String),
}

/// Handles a [`SourceMsg`] for `buf`, the document [`SourceMsg::target`] names (one
/// document per path, plan editor-feedback-round-2 WP-7).
pub fn update(
    buf: &mut SourceBuffer,
    msg: SourceMsg,
    daemon: Option<&Arc<dyn DaemonClient>>,
) -> Vec<Effect> {
    let mut fx = Vec::new();
    let edited = |buf: &mut SourceBuffer, changed: bool| {
        let primary = buf
            .take_new_primary()
            .map_or_else(Task::none, clipboard::write_primary);
        if changed {
            Task::batch([primary, autosave_timer(buf.path.clone(), buf.edit_seq)])
        } else {
            primary
        }
    };
    let task = match msg {
        SourceMsg::Action(a) => {
            let changed = buf.perform(a, Instant::now());
            edited(buf, changed)
        }
        SourceMsg::Key(k) => {
            let now = Instant::now();
            let changed = match k {
                SourceKey::Enter => buf.enter(now),
                SourceKey::DeleteWordBack => buf.delete_word(false, now),
                SourceKey::DeleteWordForward => buf.delete_word(true, now),
                SourceKey::Home { select } => {
                    buf.smart_home_end(false, select);
                    false
                }
                SourceKey::End { select } => {
                    buf.smart_home_end(true, select);
                    false
                }
                SourceKey::Indent => buf.indent(false, now),
                SourceKey::Outdent => buf.indent(true, now),
            };
            edited(buf, changed)
        }
        SourceMsg::MiddleClick { path, at: p } => {
            buf.perform(
                Action::Click(Point::new(p.x - EDITOR_PADDING, p.y - EDITOR_PADDING)),
                Instant::now(),
            );
            clipboard::read_primary().map(move |text| {
                Message::Source(SourceMsg::PastePrimary {
                    path: path.clone(),
                    text,
                })
            })
        }
        SourceMsg::CommandClick { path, at: p } => {
            buf.perform(
                Action::Click(Point::new(p.x - EDITOR_PADDING, p.y - EDITOR_PADDING)),
                Instant::now(),
            );
            let pos = buf.cursor().position;
            let line = buf.shaped().line(pos.line).map(|l| l.text.into_owned());
            match line.and_then(|l| command_click_msg(&path, &l, pos.column)) {
                Some(m) => Task::done(m),
                None => Task::none(),
            }
        }
        SourceMsg::PastePrimary { text, .. } => {
            let changed = text.is_some_and(|t| !t.is_empty() && buf.paste(t, Instant::now()));
            edited(buf, changed)
        }
        SourceMsg::Cut => {
            let (clip, changed) = buf.cut(Instant::now());
            let write = clip.map_or_else(Task::none, clipboard::write);
            Task::batch([write, edited(buf, changed)])
        }
        SourceMsg::Paste => {
            let path = buf.path.clone();
            let daemon = daemon.map(Arc::clone);
            let p = path.clone();
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        crate::clipboard_image::resolve(
                            &crate::clipboard_image::Xclip,
                            daemon.as_deref(),
                            &p,
                        )
                    })
                    .await
                    .unwrap_or_else(|e| {
                        crate::clipboard_image::PasteOutcome::Text {
                            notice: Some(format!("image paste failed: {e}")),
                        }
                    })
                },
                move |outcome| {
                    Message::Source(SourceMsg::PasteResolved {
                        path: path.clone(),
                        outcome,
                    })
                },
            )
        }
        SourceMsg::PasteResolved { path, outcome } => {
            use crate::clipboard_image::PasteOutcome;
            match outcome {
                PasteOutcome::Insert(s) => {
                    let changed = buf.paste(s, Instant::now());
                    edited(buf, changed)
                }
                PasteOutcome::Failed(e) => {
                    fx.push(Effect::Error(e));
                    Task::none()
                }
                PasteOutcome::Text { notice } => {
                    fx.extend(notice.map(Effect::Error));
                    clipboard::read().map(move |text| {
                        Message::Source(SourceMsg::PastePrimary {
                            path: path.clone(),
                            text,
                        })
                    })
                }
            }
        }
        SourceMsg::Undo | SourceMsg::Redo => {
            let changed = if matches!(msg, SourceMsg::Undo) {
                buf.undo()
            } else {
                buf.redo()
            };
            if changed {
                autosave_timer(buf.path.clone(), buf.edit_seq)
            } else {
                Task::none()
            }
        }
        SourceMsg::Save => spawn_save(daemon, buf.begin_save()),
        SourceMsg::AutosaveDue { seq, .. } => {
            if seq == buf.edit_seq {
                spawn_save(daemon, buf.begin_save())
            } else {
                Task::none()
            }
        }
        SourceMsg::KeepMine(_) => spawn_save(daemon, buf.keep_mine()),
        // Routed by the app (`App::update_inner`); never reaches here.
        SourceMsg::Viewport { .. }
        | SourceMsg::Saved { .. }
        | SourceMsg::LinkKey(_)
        | SourceMsg::LinkPick(_) => Task::none(),
        SourceMsg::TakeTheirs(path) => {
            if let Some(d) = daemon {
                match d.read_file(&path) {
                    Ok((bytes, h)) => buf.reload(&bytes, h),
                    Err(e) => fx.push(Effect::Error(format!("reload failed: {e}"))),
                }
            }
            Task::none()
        }
    };
    fx.push(Effect::Task(task));
    fx
}

#[cfg(test)]
mod tests;
