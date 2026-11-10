//! Vault/note RPC handlers and the writer-job helpers they share.

use cv_protocol::{BindNoteParams, BindNoteResult, LocateNoteParams};
use cv_vault::{NoteId, NoteLocation};

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
            Ok(Err(JobError::Unavailable(m))) => Err(unavailable(&m)),
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
    /// ([`write_target`]), so no other mutation can land between the two.
    pub(super) async fn write_note(&self, p: WriteNoteParams) -> Result<WriteNoteResult, RpcError> {
        let expected = (!p.base_hash.is_empty()).then(|| ContentHash::from_hex(p.base_hash));
        let by_id = p.id.is_some();
        let holds = self.holds_view();
        let (path, res) = self
            .writer_job(move |vw| {
                let target = match p.id {
                    None => WriteTarget::At(p.path),
                    Some(id) => write_target(vw, id, p.path, expected.is_none())?,
                };
                let (path, revive) = match target {
                    WriteTarget::At(path) => (path, None),
                    WriteTarget::Revive(path) => (path, p.id),
                    WriteTarget::Taken { path, found } => {
                        let err = VaultError::Conflict {
                            path: path.clone(),
                            expected: None,
                            found,
                        };
                        return Ok((path, Err(err)));
                    }
                };
                // Plan round-18 Decision 2: refused inside the writer job, before mutation.
                holds.refuse([path.as_str()])?;
                if let Some(id) = revive {
                    // D3: the same id, live again at the request's path, before the write binds
                    // it (so the write's event carries this id, never a fresh one).
                    if vw.revive_note(NoteId(id.0), &path).is_none() {
                        let err = VaultError::Conflict {
                            path: path.clone(),
                            expected: None,
                            found: None,
                        };
                        return Ok((path, Err(err)));
                    }
                }
                let res = vw.write(&path, p.content.as_bytes(), expected);
                if revive.is_some()
                    && let Err(e) = &res
                {
                    tracing::warn!(error = %e, "write after a note-id revive failed");
                }
                Ok((path, res))
            })
            .await?;
        let path = by_id.then_some(path);
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
    /// thread. A path with no file is `NOT_FOUND`, never a mint.
    pub(super) async fn bind_note(&self, p: BindNoteParams) -> Result<BindNoteResult, RpcError> {
        cv_vault::validate_rel_path(&p.path)
            .map_err(|e| rpc_err(error_codes::INVALID_PARAMS, e.to_string()))?;
        let id = self
            .writer_job(move |vw| {
                if vw.fs().hash(&p.path)?.is_none() {
                    return Err(VaultError::NotFound(p.path).into());
                }
                vw.bind_note(&p.path).ok_or_else(ids_unavailable)
            })
            .await?;
        Ok(BindNoteResult {
            id: cv_protocol::NoteId(id.0),
        })
    }

    /// `locate_note` (plan note-identity D2): where the id is now, on the writer thread, so it
    /// observes every mutation that returned before it.
    pub(super) async fn locate_note(
        &self,
        p: LocateNoteParams,
    ) -> Result<cv_protocol::NoteLocation, RpcError> {
        let loc = self
            .writer_job(move |vw| vw.locate_note(NoteId(p.id.0)).ok_or_else(ids_unavailable))
            .await?;
        Ok(wire_location(loc))
    }

    /// `resolve_path` (plan note-path-continuity D2): answered on the writer thread, so it
    /// observes every mutation that returned before it (the causal barrier).
    pub(super) async fn resolve_path(
        &self,
        p: ResolvePathParams,
    ) -> Result<ResolvePathResult, RpcError> {
        cv_vault::validate_rel_path(&p.path)
            .map_err(|e| rpc_err(error_codes::INVALID_PARAMS, e.to_string()))?;
        let state = Arc::clone(&self.state);
        let root = self.vault_root.clone();
        self.with_writer(move |w| {
            w.call(move |_vw| crate::lineage::resolve(&state, &root, &p.path, p.since))
        })
        .await?
        .map_err(rpc_from_vault)?
        .map_err(internal)
    }

    /// `rename`: read the hash and rename in one writer job, so the CAS token is the hash the
    /// writer itself just read and no other writer job can interleave.
    pub(super) async fn rename(&self, p: RenameParams) -> Result<RenameResult, RpcError> {
        let by_id = p.id.is_some();
        let holds = self.holds_view();
        let (from, out) = self
            .writer_job(move |vw| {
                // Plan note-identity D2: by id, the note's current path, resolved in this job.
                let from = match p.id {
                    None => p.from,
                    Some(id) => live_path(vw, id, p.from)?,
                };
                // Plan round-18 Decision 2: both endpoints and every backlink source the rename
                // would rewrite (`VaultWriter::rename_touched`, the set `rename` itself gathers),
                // before mutation. With no holds nothing is queried (the request runs exactly as
                // before).
                if !holds.is_empty() {
                    let touched = vw.rename_touched(&from, &p.to)?;
                    holds.refuse(touched.iter().map(String::as_str))?;
                }
                let h = vw
                    .fs()
                    .hash(&from)?
                    .ok_or_else(|| VaultError::NotFound(from.clone()))?;
                let out = vw.rename(&from, &p.to, h)?;
                Ok((from, out))
            })
            .await?;
        Ok(RenameResult {
            from: by_id.then_some(from),
            rewritten: out.rewritten,
            skipped_conflicts: out.skipped_conflicts,
        })
    }

    /// `delete`: hash read + CAS delete in one writer job.
    pub(super) async fn delete(&self, p: DeleteParams) -> Result<DeleteResult, RpcError> {
        let permanent = p.mode == DeleteMode::Permanent;
        let holds = self.holds_view();
        self.writer_job(move |vw| {
            holds.refuse_under(&p.path)?;
            let h = vw
                .fs()
                .hash(&p.path)?
                .ok_or_else(|| VaultError::NotFound(p.path.clone()))?;
            Ok(vw.delete_with(&p.path, h, permanent)?)
        })
        .await?;
        Ok(DeleteResult {})
    }

    /// `delete_preview`: the note's embeds/attachments and whether anything else references them.
    pub(super) async fn delete_preview(
        &self,
        p: PathParams,
    ) -> Result<DeletePreviewResult, RpcError> {
        let items = self
            .writer_job(move |vw| {
                Ok(cv_index::delete_preview(&vw.index().query(), &p.path)
                    .map_err(VaultError::from)?)
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
        })
    }

    /// `delete_note`: one writer job — the note's CAS delete, then each attachment re-checked
    /// against the index at that moment (never removes something referenced since the preview).
    pub(super) async fn delete_note(
        &self,
        p: DeleteNoteParams,
    ) -> Result<DeleteNoteResult, RpcError> {
        let permanent = p.mode == DeleteMode::Permanent;
        let holds = self.holds_view();
        let (removed, skipped) = self
            .writer_job(move |vw| {
                let touched = std::iter::once(p.path.as_str())
                    .chain(p.attachments.iter().map(String::as_str));
                holds.refuse(touched)?;
                Ok(vw.delete_note(&p.path, &p.attachments, permanent)?)
            })
            .await?;
        Ok(DeleteNoteResult { removed, skipped })
    }

    /// `note_set_bool` (plan round-8-intake-views-anchor WP-3): read → frontmatter edit → CAS
    /// write in one writer job (the CAS token is the hash the writer itself read), so the writer's
    /// refusals apply. An already-equal value writes nothing.
    pub(super) async fn note_set_bool(
        &self,
        p: cv_protocol::NoteSetBoolParams,
    ) -> Result<cv_protocol::NoteSetBoolResult, RpcError> {
        let holds = self.holds_view();
        let hash = self
            .writer_job(move |vw| {
                holds.refuse([p.path.as_str()])?;
                let bytes = vw
                    .fs()
                    .read(&p.path)?
                    .ok_or_else(|| VaultError::NotFound(p.path.clone()))?;
                let hash = ContentHash::of(&bytes);
                let text = String::from_utf8(bytes)
                    .map_err(|_| JobError::Invalid(format!("{} is not UTF-8", p.path)))?;
                // The editor's toggle shares this writer (B10): one minimal splice.
                let doc = cv_note::FrontmatterDoc::new(text);
                match doc.set_edit(&p.key, &cv_note::JsValue::Bool(p.value)) {
                    Err(e) => Err(JobError::Invalid(format!("{}: {e}", p.path))),
                    Ok(None) => Ok(hash),
                    Ok(Some((range, with))) => {
                        let mut text = doc.into_text();
                        text.replace_range(range, &with);
                        Ok(vw.write(&p.path, text.as_bytes(), Some(hash))?)
                    }
                }
            })
            .await?;
        Ok(cv_protocol::NoteSetBoolResult {
            hash: hash.as_str().to_owned(),
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

/// Where a by-id `write_note` acts (plan note-identity D2/D3).
enum WriteTarget {
    /// Write here under the request's CAS.
    At(String),
    /// The id is retired and the write creates (D3 Keep mine): revive it here, then write.
    Revive(String),
    /// D3 refused: another file already sits at the request's path.
    Taken {
        path: String,
        found: Option<ContentHash>,
    },
}

/// Resolves a by-id write on the writer thread: `Live` → its live path (a stale `path` is
/// ignored); `Gone` with the "no file" base → revive at `path` unless a file is there; `Gone`
/// otherwise, or `Unknown` → `NOT_FOUND` (never a write at `path`).
fn write_target(
    vw: &mut cv_vault::VaultWriter,
    id: cv_protocol::NoteId,
    path: String,
    creates: bool,
) -> Result<WriteTarget, JobError> {
    match vw.locate_note(NoteId(id.0)).ok_or_else(ids_unavailable)? {
        NoteLocation::Live { path } => Ok(WriteTarget::At(path)),
        NoteLocation::Gone { .. } if creates => match vw.fs().hash(&path)? {
            Some(found) => Ok(WriteTarget::Taken {
                path,
                found: Some(found),
            }),
            None => Ok(WriteTarget::Revive(path)),
        },
        NoteLocation::Gone { .. } | NoteLocation::Unknown => Err(VaultError::NotFound(path).into()),
    }
}

/// The live path of `id` (a by-id rename's source); `Gone` / `Unknown` → `NOT_FOUND` at the
/// caller's stale path.
fn live_path(
    vw: &mut cv_vault::VaultWriter,
    id: cv_protocol::NoteId,
    stale: String,
) -> Result<String, JobError> {
    match vw.locate_note(NoteId(id.0)).ok_or_else(ids_unavailable)? {
        NoteLocation::Live { path } => Ok(path),
        NoteLocation::Gone { .. } | NoteLocation::Unknown => {
            Err(VaultError::NotFound(stale).into())
        }
    }
}

fn ids_unavailable() -> JobError {
    JobError::Unavailable("note ids unavailable".into())
}

/// A vault [`NoteLocation`] on the wire.
pub(crate) fn wire_location(loc: NoteLocation) -> cv_protocol::NoteLocation {
    match loc {
        NoteLocation::Live { path } => cv_protocol::NoteLocation::Live { path },
        NoteLocation::Gone { replaced_by } => cv_protocol::NoteLocation::Gone {
            replaced_by: replaced_by.map(|i| cv_protocol::NoteId(i.0)),
        },
        NoteLocation::Unknown => cv_protocol::NoteLocation::Unknown,
    }
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
