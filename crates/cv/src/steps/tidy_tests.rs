//! WP-2 `tidy` tests: a synthetic temp vault, temp `state.db` and temp XDG trash (never the live
//! vault or the home trash).

use std::path::Path;
use std::time::{Duration, SystemTime};

use cv_actions::{Action, ConfigSet, ResourceSpec, Step};
use cv_state::NewTrashEntry;
use cv_vault::test_support::LiveFixture;
use cv_vault::{ContentHash, FileIdentity, TrashLocation, TrashMove, TrashReason, TrashRecord};

use super::{DAY_MS, Params, run_in, sweep_one};
use crate::steps::testkit::{ActionBuilder, StepEnv, config_of};
use crate::steps::{StepOutcome, validate};

const ORPH: &str = "a/orph_MD5.png";
const REFD: &str = "a/refd_MD5.png";
const YOUNG: &str = "a/young_MD5.png";

fn action() -> Action {
    ActionBuilder::new(
        "vault-tidy",
        vec![],
        ResourceSpec {
            note: false,
            keys: vec![],
        },
    )
    .build()
}

fn env_with(files: &[(&str, &str)], config: ConfigSet) -> StepEnv {
    StepEnv::new(LiveFixture::new(files.iter().copied()), config, action())
}

fn env(files: &[(&str, &str)]) -> StepEnv {
    env_with(files, ConfigSet::new())
}

fn now() -> i64 {
    crate::now_ms()
}

fn p() -> Params {
    Params { min_age_hours: 24 }
}

fn run(e: &StepEnv, sys: Option<&Path>, at: i64) -> StepOutcome {
    run_in(p(), &mut e.ctx(), sys, at).expect("tidy runs")
}

fn n(o: &StepOutcome, k: &str) -> u64 {
    o.vars[k].as_u64().unwrap_or_else(|| panic!("var {k}"))
}

/// Sets the mtime of vault file `rel` `days` days into the past.
fn age(e: &StepEnv, rel: &str, days: u64) {
    set_old(&e.fx.path(rel), days);
}

fn set_old(p: &Path, days: u64) {
    let old = SystemTime::now() - Duration::from_secs(days * 86_400);
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(old)
        .unwrap();
}

fn row_for(
    location: TrashLocation,
    trash_path: &str,
    identity: FileIdentity,
    trashed_ms: i64,
) -> NewTrashEntry {
    crate::daemon::trash::entry_of(&TrashRecord {
        moved: TrashMove {
            location,
            trash_path: trash_path.into(),
            identity,
        },
        original_path: "orig/x.md".into(),
        trashed_ms,
        reason: TrashReason::Deleted,
    })
}

fn fid(bytes: u64, mtime_ms: i64, inode: u64) -> FileIdentity {
    FileIdentity {
        bytes,
        mtime_ms,
        inode,
    }
}

fn rows(e: &StepEnv) -> Vec<cv_state::TrashEntry> {
    e.state.lock().unwrap().trash_entries().unwrap()
}

/// A temp XDG `Trash` with one entry; `marked` adds the cv crash receipt.
fn system_trash(root: &Path, name: &str, body: &str, marked: bool) {
    std::fs::create_dir_all(root.join("files")).unwrap();
    std::fs::create_dir_all(root.join("info")).unwrap();
    std::fs::write(root.join("files").join(name), body).unwrap();
    let mut info = format!("[Trash Info]\nPath=/x/{name}\nDeletionDate=2026-01-01T00:00:00\n");
    if marked {
        info.push_str(&format!("{}=sub/{name}\n", cv_vault::trash::CV_MARKER_KEY));
    }
    std::fs::write(root.join("info").join(format!("{name}.trashinfo")), info).unwrap();
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ---- params -------------------------------------------------------------------------------------

#[test]
fn params_default_and_reject_non_integers_and_zero() {
    let c = ConfigSet::new();
    let step = |src: &str| {
        Step::from_table(
            format!("type = \"tidy\"\n{src}")
                .parse::<toml::Table>()
                .unwrap(),
        )
        .unwrap()
    };
    assert!(validate(&step(""), &c).is_ok());
    assert!(validate(&step("min_age_hours = 1"), &c).is_ok());
    assert!(validate(&step("min_age_hours = 72"), &c).is_ok());
    for bad in [
        "min_age_hours = 0",
        "min_age_hours = -3",
        "min_age_hours = 1.5",
        "min_age_hours = \"24\"",
        "min_age_hours = true",
        "bogus = 1",
    ] {
        assert!(validate(&step(bad), &c).is_err(), "{bad}");
    }
    assert_eq!(super::parse(&toml::Table::new()).unwrap().min_age_hours, 24);
}

// ---- intrinsic keys -----------------------------------------------------------------------------

#[test]
fn attachment_writers_and_tidy_share_the_attachments_key() {
    use cv_actions::{ATTACHMENTS_KEY, INTAKE_IDENTITY_KEY, StepKind};
    for k in [
        StepKind::Tidy,
        StepKind::Localize,
        StepKind::TrackMail,
        StepKind::TrackBlogs,
    ] {
        assert!(
            k.intrinsic_resource_keys().contains(&ATTACHMENTS_KEY),
            "{k:?}"
        );
    }
    for k in [StepKind::TrackMail, StepKind::TrackBlogs] {
        assert!(
            k.intrinsic_resource_keys().contains(&INTAKE_IDENTITY_KEY),
            "{k:?}"
        );
    }
    assert_eq!(
        StepKind::Localize.intrinsic_resource_keys(),
        &[ATTACHMENTS_KEY]
    );
    assert!(
        !StepKind::Lint
            .intrinsic_resource_keys()
            .contains(&ATTACHMENTS_KEY)
    );
    assert!(
        !StepKind::AutoFile
            .intrinsic_resource_keys()
            .contains(&ATTACHMENTS_KEY)
    );
}

// ---- orphans ------------------------------------------------------------------------------------

fn orphan_env() -> StepEnv {
    let e = env(&[
        ("n.md", "see ![[refd_MD5.png]]\n"),
        (ORPH, "orphan bytes"),
        (REFD, "referenced bytes"),
        (YOUNG, "young bytes"),
    ]);
    age(&e, ORPH, 3);
    age(&e, REFD, 3);
    e
}

#[test]
fn orphan_trashed_while_referenced_and_young_ones_stay() {
    let e = orphan_env();
    let o = run(&e, None, now());
    assert_eq!(n(&o, "orphans_trashed"), 1);
    assert_eq!(n(&o, "orphan_bytes"), "orphan bytes".len() as u64);
    assert_eq!(
        n(&o, "orphans_skipped"),
        1,
        "the young one is skipped and counted"
    );
    assert!(e.fx.read(ORPH).is_none());
    assert!(e.fx.vault.join(".trash").join(ORPH).exists());
    assert!(e.fx.read(REFD).is_some());
    assert!(e.fx.read(YOUNG).is_some());
}

#[test]
fn orphan_too_young_by_a_longer_grace_period() {
    let e = orphan_env();
    let o = run_in(
        Params {
            min_age_hours: 24 * 7,
        },
        &mut e.ctx(),
        None,
        now(),
    )
    .unwrap();
    assert_eq!(n(&o, "orphans_trashed"), 0);
    assert_eq!(n(&o, "orphans_skipped"), 2);
    assert!(e.fx.read(ORPH).is_some());
}

fn sweep(e: &StepEnv, path: &str, hash: ContentHash) -> bool {
    let (root, path) = (e.fx.vault.clone(), path.to_owned());
    let at = now();
    e.fx.writer
        .call(move |vw| sweep_one(vw, &root, &path, hash, 24 * 3_600_000, at))
        .unwrap()
}

#[test]
fn orphan_changed_under_us_is_skipped() {
    let e = orphan_env();
    let hash = ContentHash::of(b"orphan bytes");
    e.fx.write(ORPH, "changed after the read");
    age(&e, ORPH, 3);
    assert!(!sweep(&e, ORPH, hash), "stale hash must not trash");
    assert!(e.fx.read(ORPH).is_some());
    assert!(sweep(&e, ORPH, ContentHash::of(b"changed after the read")));
    assert!(e.fx.read(ORPH).is_none());
}

#[test]
fn orphan_referenced_after_the_listing_is_skipped() {
    let e = orphan_env();
    e.fx.writer
        .call(|vw| vw.create("late.md", b"now ![[orph_MD5.png]]\n"))
        .unwrap()
        .unwrap();
    assert!(!sweep(&e, ORPH, ContentHash::of(b"orphan bytes")));
    assert!(e.fx.read(ORPH).is_some());
}

#[test]
fn orphan_trash_goes_through_the_seam_with_reason_orphan() {
    let e = orphan_env();
    let mut sink = Vec::new();
    e.fx.writer
        .call(|vw| vw.take_trash_records())
        .unwrap()
        .into_iter()
        .for_each(|r| sink.push(r));
    assert!(sweep(&e, ORPH, ContentHash::of(b"orphan bytes")));
    let recs = e.fx.writer.call(|vw| vw.take_trash_records()).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].reason, TrashReason::Orphan);
    assert_eq!(recs[0].original_path, ORPH);
}

#[test]
fn index_only_boundary_canvas_reference_does_not_protect_an_attachment() {
    // Audit E2: only `.md` notes are parsed, so a `.canvas` naming the file gives no protection.
    let e = env(&[
        (
            "board.canvas",
            "{\"nodes\":[{\"type\":\"file\",\"file\":\"a/canvas_MD5.png\"}]}",
        ),
        ("a/canvas_MD5.png", "only a canvas names me"),
    ]);
    age(&e, "a/canvas_MD5.png", 3);
    let o = run(&e, None, now());
    assert_eq!(n(&o, "orphans_trashed"), 1);
    assert!(e.fx.read("a/canvas_MD5.png").is_none());
}

#[test]
fn index_only_boundary_resolved_wikilink_does_protect_an_attachment() {
    let e = env(&[
        ("n.md", "![[a/link_MD5.png]]\n"),
        ("a/link_MD5.png", "linked from a note"),
    ]);
    age(&e, "a/link_MD5.png", 3);
    let o = run(&e, None, now());
    assert_eq!(n(&o, "orphans_trashed"), 0);
    assert!(e.fx.read("a/link_MD5.png").is_some());
}

#[test]
fn hand_placed_non_md5_attachments_are_never_swept() {
    let e = env(&[("a/photo.png", "mine")]);
    age(&e, "a/photo.png", 30);
    let o = run(&e, None, now());
    assert_eq!(n(&o, "orphans_trashed") + n(&o, "orphans_skipped"), 0);
    assert!(e.fx.read("a/photo.png").is_some());
}

// ---- seed ---------------------------------------------------------------------------------------

#[test]
fn seed_is_idempotent_and_honours_the_ctime_guard() {
    let e = env(&[]);
    e.fx.write(".trash/old/x.md", "trashed earlier");
    let t = now();
    // A fresh ctime: its row is probably in flight, so the first run leaves it.
    assert_eq!(n(&run(&e, None, t), "seeded"), 0);
    assert!(rows(&e).is_empty());
    // Eleven minutes later it is seeded, once.
    let later = t + 11 * 60 * 1000;
    assert_eq!(n(&run(&e, None, later), "seeded"), 1);
    assert_eq!(n(&run(&e, None, later), "seeded"), 0);
    let r = rows(&e);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].entry.location, TrashLocation::Vault);
    assert_eq!(r[0].entry.trash_path, ".trash/old/x.md");
    assert_eq!(r[0].entry.original_path, "old/x.md");
    assert_eq!(r[0].entry.reason, TrashReason::Seeded);
    assert_eq!(r[0].entry.trashed_ms, later);
    assert_eq!(r[0].entry.bytes, "trashed earlier".len() as i64);
    assert!(r[0].entry.inode.is_some() && r[0].entry.mtime_ms.is_some());
}

#[test]
fn seed_skips_files_that_already_have_a_matching_row() {
    let e = env(&[]);
    e.fx.write(".trash/k.md", "kept");
    let id = cv_vault::trash::identity(&e.fx.path(".trash/k.md")).unwrap();
    e.state
        .lock()
        .unwrap()
        .insert_trash_entry(&row_for(TrashLocation::Vault, ".trash/k.md", id, now()))
        .unwrap();
    assert_eq!(n(&run(&e, None, now() + 11 * 60 * 1000), "seeded"), 0);
    assert_eq!(rows(&e).len(), 1);
}

#[test]
fn adopts_a_marked_system_entry_and_never_an_unmarked_one() {
    let e = env(&[]);
    let d = tmp();
    let root = d.path().join("Trash");
    system_trash(&root, "mine.md", "cv moved me", true);
    system_trash(&root, "theirs.md", "another program moved me", false);
    let later = now() + 11 * 60 * 1000;
    assert_eq!(n(&run(&e, Some(&root), now()), "adopted"), 0, "ctime guard");
    assert_eq!(n(&run(&e, Some(&root), later), "adopted"), 1);
    assert_eq!(n(&run(&e, Some(&root), later), "adopted"), 0);
    let r = rows(&e);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].entry.location, TrashLocation::System);
    assert_eq!(r[0].entry.trash_path, "mine.md");
    assert_eq!(r[0].entry.original_path, "sub/mine.md");
    assert_eq!(r[0].entry.info_name.as_deref(), Some("mine.md.trashinfo"));
}

// ---- expire -------------------------------------------------------------------------------------

fn old_ms() -> i64 {
    now() - 31 * DAY_MS
}

#[test]
fn vault_row_past_retention_deletes_the_file_and_the_row() {
    let e = env(&[]);
    e.fx.write(".trash/d/gone.md", "expire me");
    e.fx.write(".trash/d/keep.md", "recent");
    let id = |rel: &str| cv_vault::trash::identity(&e.fx.path(rel)).unwrap();
    {
        let mut db = e.state.lock().unwrap();
        db.insert_trash_entry(&row_for(
            TrashLocation::Vault,
            ".trash/d/gone.md",
            id(".trash/d/gone.md"),
            old_ms(),
        ))
        .unwrap();
        db.insert_trash_entry(&row_for(
            TrashLocation::Vault,
            ".trash/d/keep.md",
            id(".trash/d/keep.md"),
            now(),
        ))
        .unwrap();
    }
    let o = run(&e, None, now());
    assert_eq!(n(&o, "expired"), 1);
    assert_eq!(n(&o, "expired_bytes"), "expire me".len() as u64);
    assert_eq!(n(&o, "rows_dropped"), 0);
    assert!(e.fx.read(".trash/d/gone.md").is_none());
    assert!(e.fx.read(".trash/d/keep.md").is_some());
    let r = rows(&e);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].entry.trash_path, ".trash/d/keep.md");
}

#[test]
fn system_row_past_retention_deletes_file_and_trashinfo() {
    let e = env(&[]);
    let d = tmp();
    let root = d.path().join("Trash");
    system_trash(&root, "s.md", "system expire", true);
    let id = cv_vault::trash::identity(&root.join("files/s.md")).unwrap();
    e.state
        .lock()
        .unwrap()
        .insert_trash_entry(&row_for(TrashLocation::System, "s.md", id, old_ms()))
        .unwrap();
    // Without a system root the row cannot be evaluated and stays.
    assert_eq!(n(&run(&e, None, now()), "expired"), 0);
    assert_eq!(rows(&e).len(), 1);
    let o = run(&e, Some(&root), now());
    assert_eq!(n(&o, "expired"), 1);
    assert!(!root.join("files/s.md").exists());
    assert!(!root.join("info/s.md.trashinfo").exists());
    assert!(rows(&e).is_empty());
}

#[test]
fn missing_file_drops_the_row_without_error() {
    let e = env(&[]);
    e.state
        .lock()
        .unwrap()
        .insert_trash_entry(&row_for(
            TrashLocation::Vault,
            ".trash/restored.md",
            fid(3, 5, 7),
            old_ms(),
        ))
        .unwrap();
    let o = run(&e, None, now());
    assert_eq!((n(&o, "expired"), n(&o, "rows_dropped")), (0, 1));
    assert!(rows(&e).is_empty());
}

#[test]
fn name_reuse_after_a_restore_drops_the_row_and_keeps_the_new_file() {
    // Audit E1: trash a file, "restore" it (move it out), put a different file at the same trash
    // name, expire past retention -> the row goes, the new file survives (vault and system).
    let e = env(&[]);
    e.fx.write(".trash/r.md", "the original");
    let id = cv_vault::trash::identity(&e.fx.path(".trash/r.md")).unwrap();
    e.state
        .lock()
        .unwrap()
        .insert_trash_entry(&row_for(TrashLocation::Vault, ".trash/r.md", id, old_ms()))
        .unwrap();
    std::fs::rename(e.fx.path(".trash/r.md"), e.fx.path("restored.md")).unwrap();
    e.fx.write(".trash/r.md", "a different file entirely");
    let d = tmp();
    let root = d.path().join("Trash");
    system_trash(&root, "r.md", "the original", true);
    let sid = cv_vault::trash::identity(&root.join("files/r.md")).unwrap();
    e.state
        .lock()
        .unwrap()
        .insert_trash_entry(&row_for(TrashLocation::System, "r.md", sid, old_ms()))
        .unwrap();
    std::fs::remove_file(root.join("files/r.md")).unwrap();
    std::fs::write(root.join("files/r.md"), "someone else's file, longer").unwrap();

    let o = run(&e, Some(&root), now());
    assert_eq!(n(&o, "expired"), 0);
    assert_eq!(n(&o, "rows_dropped"), 2);
    assert_eq!(e.fx.text(".trash/r.md"), "a different file entirely");
    assert_eq!(
        std::fs::read_to_string(root.join("files/r.md")).unwrap(),
        "someone else's file, longer"
    );
    assert!(e.fx.read("restored.md").is_some());
    // The surviving unrecorded files are only seeded (a fresh row), never deleted.
    assert!(
        rows(&e)
            .iter()
            .all(|r| r.entry.reason != TrashReason::Deleted)
    );
}

#[test]
fn retention_zero_expires_nothing() {
    let cfg = config_of(&[("vault.toml", "[trash]\nretention_days = 0\n")]);
    let e = env_with(&[], cfg);
    e.fx.write(".trash/old.md", "ancient");
    let id = cv_vault::trash::identity(&e.fx.path(".trash/old.md")).unwrap();
    e.state
        .lock()
        .unwrap()
        .insert_trash_entry(&row_for(TrashLocation::Vault, ".trash/old.md", id, 1))
        .unwrap();
    let o = run(&e, None, now());
    assert_eq!((n(&o, "expired"), n(&o, "rows_dropped")), (0, 0));
    assert!(e.fx.read(".trash/old.md").is_some());
    assert_eq!(rows(&e).len(), 1);
}

#[test]
fn escaping_trash_paths_never_delete_anything() {
    let e = env(&[("secret.md", "outside the trash")]);
    for bad in [".trash/../secret.md", "secret.md", ".trash//x"] {
        e.state
            .lock()
            .unwrap()
            .insert_trash_entry(&row_for(TrashLocation::Vault, bad, fid(17, 1, 1), old_ms()))
            .unwrap();
    }
    let o = run(&e, None, now());
    assert_eq!((n(&o, "expired"), n(&o, "rows_dropped")), (0, 3));
    assert!(e.fx.read("secret.md").is_some());
}
