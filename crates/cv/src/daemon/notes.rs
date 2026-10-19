//! Vault/note RPC handlers and the writer-job helpers they share.

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

    /// Runs `f` as one writer job; every layer's failure (shutdown, writer gone, the job's own
    /// error) flattens into one [`RpcError`].
    pub(crate) async fn writer_job<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut cv_vault::VaultWriter) -> Result<T, JobError> + Send + 'static,
    ) -> Result<T, RpcError> {
        match self.with_writer(move |w| w.call(f)).await? {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(JobError::Invalid(m))) => Err(rpc_err(error_codes::INVALID_PARAMS, m)),
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

    /// `write_note`: a CAS write; a hash conflict is a result, not an error.
    pub(super) async fn write_note(&self, p: WriteNoteParams) -> Result<WriteNoteResult, RpcError> {
        let expected = (!p.base_hash.is_empty()).then(|| ContentHash::from_hex(p.base_hash));
        let res = self
            .with_writer(move |w| w.write(&p.path, p.content.into_bytes(), expected))
            .await?;
        match res {
            Ok(h) => Ok(WriteNoteResult::Written {
                hash: h.as_str().to_owned(),
            }),
            Err(VaultError::Conflict { found, .. }) => Ok(WriteNoteResult::Conflict {
                current_hash: found.map(|h| h.as_str().to_owned()).unwrap_or_default(),
            }),
            Err(e) => Err(rpc_from_vault(e)),
        }
    }

    /// `rename`: read the hash and rename in one writer job, so the CAS token is the hash the
    /// writer itself just read and no other writer job can interleave.
    pub(super) async fn rename(&self, p: RenameParams) -> Result<RenameResult, RpcError> {
        let out = self
            .writer_job(move |vw| {
                let h = vw
                    .fs()
                    .hash(&p.from)?
                    .ok_or_else(|| VaultError::NotFound(p.from.clone()))?;
                Ok(vw.rename(&p.from, &p.to, h)?)
            })
            .await?;
        Ok(RenameResult {
            rewritten: out.rewritten,
            skipped_conflicts: out.skipped_conflicts,
        })
    }

    /// `delete`: hash read + CAS delete in one writer job.
    pub(super) async fn delete(&self, p: DeleteParams) -> Result<DeleteResult, RpcError> {
        let permanent = p.mode == DeleteMode::Permanent;
        self.writer_job(move |vw| {
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
        let (removed, skipped) = self
            .writer_job(move |vw| Ok(vw.delete_note(&p.path, &p.attachments, permanent)?))
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
        let hash = self
            .writer_job(move |vw| {
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
