//! Vault/note RPC handlers and the writer-job helpers they share.

use cv_protocol::{BindNoteParams, BindNoteResult, LocateNoteParams};
use cv_vault::NoteLocation;

use super::*;

impl Shared {
    /// Runs `f` with the writer on a blocking thread.
    pub async fn with_writer<T: Send + 'static>(
        &self,
        f: impl FnOnce(WriterHandle) -> T + Send + 'static,
    ) -> Result<T, RpcError> {
        let w = self
            .writer()
            .ok_or_else(|| unavailable("daemon is shutting down"))?;
        tokio::task::spawn_blocking(move || f(w))
            .await
            .map_err(internal)
    }

    /// The runner's note holds (empty before the runner starts).
    pub(crate) fn holds_view(&self) -> crate::runner::HoldsView {
        self.runner().map(Runner::holds_view).unwrap_or_default()
    }

    /// Runs `f` as one writer job; every layer's failure (shutdown, writer gone, the job's own
    /// error) flattens into one [`RpcError`].
    pub(crate) async fn writer_job<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut cv_vault::VaultWriter) -> Result<T, JobError> + Send + 'static,
    ) -> Result<T, RpcError> {
        match self.with_writer(move |w| w.call(f)).await? {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(JobError::Invalid(m))) => Err(rpc_err(error_codes::INVALID_PARAMS, m)),
            Ok(Err(JobError::Held { path, label })) => Err(rpc_held(&path, &label)),
            Ok(Err(JobError::Vault(e))) | Err(e) => Err(rpc_from_vault(e)),
        }
    }

    /// Rebuilds the index; on success broadcasts `index_generation{n}`. On failure the writer
    /// has already re-opened the newest generation (`VaultWriter::rebuild`); the error is logged.
    pub async fn rebuild_index(&self) -> Result<u64, RpcError> {
        match self.with_writer(|w| w.rebuild()).await? {
            Ok(n) => {
                self.publish(Notification::IndexGeneration { n });
                Ok(n)
            }
            Err(e) => {
                tracing::error!(error = %e, "index rebuild failed; writer reopened current");
                Err(rpc_from_vault(e))
            }
        }
    }

    /// `write_note`: a CAS write; a hash conflict is a result, not an error. With an `id` (plan
    /// note-identity D2) the note's current path is resolved in the same writer job as the write
    /// ([`resolve`]), so no other mutation can land between the two.
    pub(super) async fn write_note(&self, p: WriteNoteParams) -> Result<WriteNoteResult, RpcError> {
        let expected = (!p.base_hash.is_empty()).then(|| ContentHash::from_hex(p.base_hash));
        let holds = self.holds_view();
        let at = NoteRef::new(p.path, p.id);
        let (to, res) = self
            .writer_job(move |vw| {
                let to = resolve(vw, at, expected.is_none())?;
                if to.revive.is_some()
                    && let Some(found) = vw.fs().hash(&to.path)?
                {
                    // D3 refused: another file already sits at the request's path.
                    let err = VaultError::Conflict {
                        path: to.path.clone(),
                        expected: None,
                        found: Some(found),
                    };
                    return Ok((to, Err(err)));
                }
                // Plan round-18 Decision 2: refused inside the writer job, before mutation.
                holds.refuse([to.path.as_str()])?;
                if let Some(id) = to.revive {
                    // D3: the same id, live again at the request's path, before the write binds
                    // it (so the write's event carries this id, never a fresh one).
                    if !vw.revive_note(id, &to.path).map_err(VaultError::from)? {
                        let err = VaultError::Conflict {
                            path: to.path.clone(),
                            expected: None,
                            found: None,
                        };
                        return Ok((to, Err(err)));
                    }
                }
                let res = vw.write(&to.path, p.content.as_bytes(), expected);
                if to.revive.is_some()
                    && let Err(e) = &res
                {
                    tracing::warn!(error = %e, "write after a note-id revive failed");
                }
                Ok((to, res))
            })
            .await?;
        let path = to.reply_path();
        match res {
            Ok(h) => Ok(WriteNoteResult::Written {
                hash: h.as_str().to_owned(),
                path,
            }),
            Err(VaultError::Conflict { found, .. }) => Ok(WriteNoteResult::Conflict {
                current_hash: found.map(|h| h.as_str().to_owned()).unwrap_or_default(),
                path,
            }),
            Err(e) => Err(rpc_from_vault(e)),
        }
    }

    /// `bind_note` (plan note-identity D2): the id of the file now at `path`, on the writer
    /// thread. A path with no file is `NOT_FOUND`, never a mint; a store failure is an error.
    pub(super) async fn bind_note(&self, p: BindNoteParams) -> Result<BindNoteResult, RpcError> {
        cv_vault::validate_rel_path(&p.path)
            .map_err(|e| rpc_err(error_codes::INVALID_PARAMS, e.to_string()))?;
        let id = self
            .writer_job(move |vw| {
                if vw.fs().hash(&p.path)?.is_none() {
                    return Err(VaultError::NotFound(p.path).into());
                }
                Ok(vw.bind_note(&p.path).map_err(VaultError::from)?)
            })
            .await?;
        Ok(BindNoteResult { id })
    }

    /// `locate_note` (plan note-identity D2): where the id is now, on the writer thread, so it
    /// observes every mutation that returned before it.
    pub(super) async fn locate_note(
        &self,
        p: LocateNoteParams,
    ) -> Result<cv_protocol::NoteLocation, RpcError> {
        self.writer_job(move |vw| Ok(vw.locate_note(p.id).map_err(VaultError::from)?))
            .await
    }

    /// `rename`: read the hash and rename in one writer job, so the CAS token is the hash the
    /// writer itself just read and no other writer job can interleave.
    pub(super) async fn rename(&self, p: RenameParams) -> Result<RenameResult, RpcError> {
        let holds = self.holds_view();
        let at = NoteRef::new(p.from, p.id);
        let (from, out) = self
            .writer_job(move |vw| {
                // Plan note-identity D2: by id, the note's current path, resolved in this job.
                let from = resolve(vw, at, false)?;
                // Plan round-18 Decision 2: both endpoints and every backlink source the rename
                // would rewrite (`VaultWriter::rename_touched`, the set `rename` itself gathers),
                // before mutation. With no holds nothing is queried (the request runs exactly as
                // before).
                if !holds.is_empty() {
                    let touched = vw.rename_touched(&from.path, &p.to)?;
                    holds.refuse(touched.iter().map(String::as_str))?;
                }
                let h = vw
                    .fs()
                    .hash(&from.path)?
                    .ok_or_else(|| VaultError::NotFound(from.path.clone()))?;
                let out = vw.rename(&from.path, &p.to, h)?;
                Ok((from, out))
            })
            .await?;
        Ok(RenameResult {
            from: from.reply_path(),
            rewritten: out.rewritten,
            skipped_conflicts: out.skipped_conflicts,
        })
    }

    /// `delete`: resolve, hash read + CAS delete in one writer job.
    pub(super) async fn delete(&self, p: DeleteParams) -> Result<DeleteResult, RpcError> {
        let permanent = p.mode == DeleteMode::Permanent;
        let holds = self.holds_view();
        let at = NoteRef::new(p.path, p.id);
        let at = self
            .writer_job(move |vw| {
                let at = resolve(vw, at, false)?;
                holds.refuse_under(&at.path)?;
                let h = vw
                    .fs()
                    .hash(&at.path)?
                    .ok_or_else(|| VaultError::NotFound(at.path.clone()))?;
                vw.delete_with(&at.path, h, permanent)?;
                Ok(at)
            })
            .await?;
        Ok(DeleteResult {
            path: at.reply_path(),
        })
    }

    /// `delete_preview`: the note's embeds/attachments and whether anything else references them.
    pub(super) async fn delete_preview(
        &self,
        p: PathParams,
    ) -> Result<DeletePreviewResult, RpcError> {
        let at = NoteRef::new(p.path, p.id);
        let (at, items) = self
            .writer_job(move |vw| {
                let at = resolve(vw, at, false)?;
                let items = cv_index::delete_preview(&vw.index().query(), &at.path)
                    .map_err(VaultError::from)?;
                Ok((at, items))
            })
            .await?;
        Ok(DeletePreviewResult {
            items: items
                .into_iter()
                .map(|(path, referenced_elsewhere)| DeletePreviewItem {
                    path,
                    referenced_elsewhere,
                })
                .collect(),
            path: at.reply_path(),
        })
    }

    /// `delete_note`: one writer job — resolve, the note's CAS delete, then each attachment
    /// re-checked against the index at that moment (never removes something referenced since the
    /// preview).
    pub(super) async fn delete_note(
        &self,
        p: DeleteNoteParams,
    ) -> Result<DeleteNoteResult, RpcError> {
        let permanent = p.mode == DeleteMode::Permanent;
        let holds = self.holds_view();
        let at = NoteRef::new(p.path, p.id);
        let (at, (removed, skipped)) = self
            .writer_job(move |vw| {
                let at = resolve(vw, at, false)?;
                let touched = std::iter::once(at.path.as_str())
                    .chain(p.attachments.iter().map(String::as_str));
                holds.refuse(touched)?;
                let out = vw.delete_note(&at.path, &p.attachments, permanent)?;
                Ok((at, out))
            })
            .await?;
        Ok(DeleteNoteResult {
            removed,
            skipped,
            path: at.reply_path(),
        })
    }

    /// `note_set_bool` (plan round-8-intake-views-anchor WP-3): resolve → read → frontmatter
    /// edit → CAS write in one writer job (the CAS token is the hash the writer itself read), so
    /// the writer's refusals apply. An already-equal value writes nothing.
    pub(super) async fn note_set_bool(
        &self,
        p: cv_protocol::NoteSetBoolParams,
    ) -> Result<cv_protocol::NoteSetBoolResult, RpcError> {
        let holds = self.holds_view();
        let at = NoteRef::new(p.path, p.id);
        let (at, hash) = self
            .writer_job(move |vw| {
                let at = resolve(vw, at, false)?;
                let path = at.path.as_str();
                holds.refuse([path])?;
                let bytes = vw
                    .fs()
                    .read(path)?
                    .ok_or_else(|| VaultError::NotFound(path.to_owned()))?;
                let hash = ContentHash::of(&bytes);
                let text = String::from_utf8(bytes)
                    .map_err(|_| JobError::Invalid(format!("{path} is not UTF-8")))?;
                // The editor's toggle shares this writer (B10): one minimal splice.
                let doc = cv_note::FrontmatterDoc::new(text);
                let hash = match doc.set_edit(&p.key, &cv_note::JsValue::Bool(p.value)) {
                    Err(e) => return Err(JobError::Invalid(format!("{path}: {e}"))),
                    Ok(None) => hash,
                    Ok(Some((range, with))) => {
                        let mut text = doc.into_text();
                        text.replace_range(range, &with);
                        vw.write(path, text.as_bytes(), Some(hash))?
                    }
                };
                Ok((at, hash))
            })
            .await?;
        Ok(cv_protocol::NoteSetBoolResult {
            hash: hash.as_str().to_owned(),
            path: at.reply_path(),
        })
    }

    /// `search` (plan semsearch-crate WP-4): hybrid search over the runner's shared index,
    /// off the request loop ([`crate::search_query`]).
    pub async fn search(
        &self,
        params: &cv_protocol::SearchParams,
    ) -> Result<cv_protocol::SearchResult, RpcError> {
        let runner = self.runner_or_err()?;
        let mgr = crate::steps::agent::manager().map_err(|e| internal(e.to_string()))?;
        crate::search_query::search(runner.search_db(), &runner.config(), mgr, params)
            .await
            .map_err(internal)
    }

    /// `write_attachment`: validates, resolves the path with the `localize` default templates,
    /// reuses an existing file (content-MD5 name ⇒ identical bytes), else creates it through the
    /// writer.
    pub(super) async fn write_attachment(
        &self,
        p: WriteAttachmentParams,
    ) -> Result<WriteAttachmentResult, RpcError> {
        use base64::Engine as _;
        use cv_text::attachments::{
            DEFAULT_FOLDER_TEMPLATE, DEFAULT_NAME_TEMPLATE, resolve_attachment_path,
        };
        let ext_ok = (1..=8).contains(&p.ext.len())
            && p.ext
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        if !ext_ok {
            return Err(rpc_err(
                error_codes::INVALID_PARAMS,
                format!("invalid attachment ext {:?}", p.ext),
            ));
        }
        // Reject before decoding: base64 is 4/3 of the decoded size.
        if p.data_base64.len() / 4 * 3 > MAX_ATTACHMENT_BYTES + 3 {
            return Err(too_large());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(p.data_base64.as_bytes())
            .map_err(|e| rpc_err(error_codes::INVALID_PARAMS, format!("bad base64: {e}")))?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(too_large());
        }
        let md5 = crate::steps::util::md5_hex(&bytes);
        let target = resolve_attachment_path(
            DEFAULT_FOLDER_TEMPLATE,
            DEFAULT_NAME_TEMPLATE,
            &p.note_path,
            "",
            &p.ext,
            &md5,
            chrono::Local::now().naive_local(),
        );
        let t = target.clone();
        self.writer_job(move |vw| {
            if vw.fs().hash(&t)?.is_none() {
                match vw.create(&t, &bytes) {
                    Ok(_) | Err(VaultError::Exists(_)) => {}
                    Err(e) => return Err(e.into()),
                }
            } else {
                // Reuse: bump the mtime so the `tidy` sweep's grace period protects the file
                // until the pasted note links it (plan tidiness-trash-orphans WP-2, audit B4).
                crate::steps::util::touch_now(vw.fs().root(), &t);
            }
            Ok(())
        })
        .await?;
        Ok(WriteAttachmentResult { path: target })
    }

    /// Creates `path` with Obsidian's ` N` collision suffix; returns the final path.
    pub(super) async fn create_suffixed(
        &self,
        path: String,
        content: String,
    ) -> Result<String, RpcError> {
        let res = self
            .with_writer(move |w| {
                for n in 0..MAX_SUFFIX {
                    let p = uri::suffixed(&path, n);
                    match w.create(&p, content.clone().into_bytes()) {
                        Ok(_) => return Ok(p),
                        Err(VaultError::Exists(_)) => continue,
                        Err(e) => return Err(e),
                    }
                }
                Err(VaultError::Exists(path))
            })
            .await?;
        res.map_err(rpc_from_vault)
    }

    /// `obsidian://` intake (see [`crate::uri`]).
    pub async fn intake(&self, url: &str) -> Result<UriResult, RpcError> {
        let name = self
            .vault_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let action = uri::parse(url, &name)
            .map_err(|e| rpc_err(error_codes::INVALID_PARAMS, e.to_string()))?;
        match action {
            UriAction::New {
                path,
                content,
                silent,
            } => {
                let path = self.create_suffixed(path, content).await?;
                // Obsidian opens a non-silent new note; we only tell subscribed clients.
                if !silent && self.open_subscribers.load(Ordering::SeqCst) > 0 {
                    self.publish(Notification::OpenRequested { path: path.clone() });
                }
                Ok(UriResult::Created { path })
            }
            UriAction::Open { path } => {
                if self.open_subscribers.load(Ordering::SeqCst) > 0 {
                    self.publish(Notification::OpenRequested { path: path.clone() });
                } else if let Some(cmd) = self.config.open_cmd.clone() {
                    let abs = self.vault_root.join(&path);
                    tokio::task::spawn_blocking(move || run_open_cmd(&cmd, &abs));
                } else {
                    tracing::info!(%path, "open requested; no subscriber and no CV_OPEN_CMD");
                }
                Ok(UriResult::OpenRequested { path })
            }
        }
    }
}

/// One note address (plan tn-review-2026-11-10-remediation D2): the request's path, and the id
/// that wins over it when present.
struct NoteRef {
    path: String,
    id: Option<cv_protocol::NoteId>,
}

impl NoteRef {
    fn new(path: String, id: Option<cv_protocol::NoteId>) -> Self {
        Self { path, id }
    }
}

/// Where a request acts, resolved on the writer thread inside the mutating job.
struct Resolved {
    /// The path acted on.
    path: String,
    /// A by-id write that creates (D3 Keep mine): the retired id to revive at `path`.
    revive: Option<cv_protocol::NoteId>,
    /// The request named an id: the reply carries `path`.
    by_id: bool,
}

impl Resolved {
    /// The reply's optional acted-on path: present for by-id requests only, so a by-path reply
    /// keeps its shape (audit F3).
    fn reply_path(&self) -> Option<String> {
        self.by_id.then(|| self.path.clone())
    }
}

/// Resolves `at` (plan note-identity D2/D3): by path as given; by id `Live` → its live path (a
/// stale `path` is ignored); `Gone` when `creates` (a write with the "no file" base) → revive at
/// `path`; otherwise `Gone` or `Unknown` → `NOT_FOUND`, never a fallback to `path`. A store
/// failure is an error (Decision 4).
fn resolve(
    vw: &mut cv_vault::VaultWriter,
    at: NoteRef,
    creates: bool,
) -> Result<Resolved, JobError> {
    let Some(id) = at.id else {
        return Ok(Resolved {
            path: at.path,
            revive: None,
            by_id: false,
        });
    };
    let (path, revive) = match vw.locate_note(id).map_err(VaultError::from)? {
        NoteLocation::Live { path } => (path, None),
        NoteLocation::Gone { .. } if creates => (at.path, Some(id)),
        NoteLocation::Gone { .. } | NoteLocation::Unknown => {
            return Err(VaultError::NotFound(at.path).into());
        }
    };
    Ok(Resolved {
        path,
        revive,
        by_id: true,
    })
}

pub(super) fn run_open_cmd(cmd: &Path, abs: &Path) {
    match std::process::Command::new(cmd).arg(abs).spawn() {
        Ok(mut child) => match child.wait() {
            Ok(s) if s.success() => {}
            Ok(s) => tracing::warn!(status = %s, "CV_OPEN_CMD exited unsuccessfully"),
            Err(e) => tracing::warn!(error = %e, "waiting for CV_OPEN_CMD"),
        },
        Err(e) => tracing::warn!(error = %e, cmd = %cmd.display(), "running CV_OPEN_CMD"),
    }
}
