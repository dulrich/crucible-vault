//! `image_description` (plan round-24-quick-wins WP-3): the stored description narrative of one
//! vault image, for the editor's hover alt text and Copy alt text.

use std::path::Component;

use cv_protocol::{ImageDescriptionParams, ImageDescriptionResult};
use cv_search::prepare::localized_image_md5;

use super::*;
use crate::steps::image_describe::is_image;
use crate::steps::util::md5_hex;

impl Shared {
    /// `image_description`: hashes the file off the executor, then reads the store by md5.
    pub(super) async fn image_description(
        &self,
        p: ImageDescriptionParams,
    ) -> Result<ImageDescriptionResult, RpcError> {
        let root = self.vault_root.clone();
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || lookup(&root, &state, &p.path))
            .await
            .map_err(internal)
    }
}

/// The narrative for the vault image `rel`, or none. A missing file, a path outside `root`, a
/// non-image, a store error or an empty/absent narrative all answer none.
pub(super) fn lookup(root: &Path, state: &Mutex<StateDb>, rel: &str) -> ImageDescriptionResult {
    let none = ImageDescriptionResult::default();
    if !is_image(rel) {
        return none;
    }
    let rel_path = Path::new(rel);
    if !rel_path
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return none;
    }
    let (Ok(canon_root), Ok(abs)) = (root.canonicalize(), root.join(rel_path).canonicalize())
    else {
        return none;
    };
    if !abs.starts_with(&canon_root) {
        return none;
    }
    let Ok(bytes) = std::fs::read(&abs) else {
        return none;
    };
    // The store is keyed by the `_MD5` file-name md5 (`image_describe`), which `localize` derives
    // from the content; look up both so a renamed or hand-placed image still resolves.
    let mut keys = vec![md5_hex(&bytes)];
    if let Some((named, _)) = localized_image_md5(rel)
        && named != keys[0]
    {
        keys.push(named);
    }
    let records = match lock(state).image_descriptions_for(&keys) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "image_description: store read failed");
            return none;
        }
    };
    ImageDescriptionResult {
        narrative: records
            .into_iter()
            .filter(|r| !r.narrative.trim().is_empty())
            .max_by(|a, b| a.described_at.cmp(&b.described_at))
            .map(|r| r.narrative),
    }
}

#[cfg(test)]
mod tests {
    use cv_state::{ImageDescription, QueueConfig};

    use super::*;

    fn rec(md5: &str, narrative: &str, at: &str) -> ImageDescription {
        ImageDescription {
            md5: md5.to_owned(),
            kind: "vision".to_owned(),
            narrative: narrative.to_owned(),
            extraction: String::new(),
            description_hash: String::new(),
            described_at: at.to_owned(),
            raw: "{}".to_owned(),
        }
    }

    #[test]
    fn described_image_answers_narrative_and_others_none() {
        let dir = tempfile::tempdir().expect("tmp");
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(vault.join("img")).expect("mkdir");
        std::fs::write(vault.join("img/a.png"), b"described").expect("w");
        std::fs::write(vault.join("img/b.png"), b"undescribed").expect("w");
        std::fs::write(vault.join("note.md"), b"described").expect("w");
        std::fs::write(dir.path().join("out.png"), b"described").expect("w");
        let mut db =
            StateDb::open(&dir.path().join("state.db"), 0, QueueConfig::default()).expect("db");
        let a = md5_hex(b"described");
        db.put_image_description(&rec(&a, "a red bicycle", "2026-01-01T00:00:00Z"))
            .expect("put");
        // Newer record under the `_MD5` file-name key wins over the content-md5 one.
        let named = "0123456789abcdef0123456789abcdef";
        std::fs::write(vault.join(format!("img/c_{named}_MD5.png")), b"described").expect("w");
        db.put_image_description(&rec(named, "newer", "2026-02-01T00:00:00Z"))
            .expect("put");
        let state = Mutex::new(db);

        let get = |p: &str| lookup(&vault, &state, p).narrative;
        assert_eq!(get("img/a.png").as_deref(), Some("a red bicycle"));
        assert_eq!(
            get(&format!("img/c_{named}_MD5.png")).as_deref(),
            Some("newer")
        );
        assert_eq!(get("img/b.png"), None);
        assert_eq!(get("img/missing.png"), None);
        assert_eq!(get("note.md"), None);
        assert_eq!(get("../out.png"), None);
    }
}
