//! A bare `--help` (alone or after any subcommand) prints usage and exits 0 without loading
//! config, connecting to the daemon or spooling anything — `cv uri --help` once spooled "--help".
use std::process::Command;

#[test]
fn help_prints_usage_and_touches_nothing() {
    let dir = std::env::temp_dir().join(format!("cv-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        &["--help"][..],
        &["-h"][..],
        &["help"][..],
        &["daemon", "--help"][..],
        &["uri", "--help"][..],
        &["status", "--help"][..],
        &["signals", "--help"][..],
        &["signals", "export", "--help"][..],
        &["jobs", "--help"][..],
        &["jobs", "list", "--help"][..],
        &["intake", "--help"][..],
        &["run", "--help"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_cv"))
            .args(args)
            .current_dir(&dir)
            .env_clear()
            .output()
            .expect("spawn cv");
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("usage: cv"), "{args:?}: {text}");
    }
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "help created files"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_command_is_still_a_usage_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_cv"))
        .arg("bogus")
        .env_clear()
        .output()
        .expect("spawn cv");
    assert_eq!(out.status.code(), Some(2));
}
