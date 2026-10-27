//! `CLIPLEAF_LOG` as the user sees it: `debug:` lines on stderr of the real `clipleaf-paste`.
//! The run fails on purpose (nothing listens on the socket), which is after the first debug line
//! and needs no compositor. Tests are built with debug assertions on by default, so unset means
//! "on" there; the release default is checked through `cfg!(debug_assertions)`.

use std::{
    process::Output,
    sync::atomic::{AtomicU32, Ordering},
};

use clipleaf_test_common::bins;

const PASTE: &str = env!("CARGO_BIN_EXE_clipleaf-paste");

/// Runs `clipleaf-paste` against an empty runtime dir with `CLIPLEAF_LOG` set to `log`.
fn run_failing(log: Option<&str>) -> Output {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "clipleaf-log-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut command = bins::bare(PASTE, &[]);
    command.env("XDG_RUNTIME_DIR", &dir);
    if let Some(log) = log {
        command.env("CLIPLEAF_LOG", log);
    }
    let output = command.output().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    // The failure itself is always reported, whatever the log level.
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(err.contains("error:") && err.contains("cannot connect"), "{err}");
    output
}

fn has_debug_lines(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr).lines().any(|line| line.starts_with("debug:"))
}

#[test]
fn clipleaf_log_debug_turns_debug_lines_on() {
    assert!(has_debug_lines(&run_failing(Some("debug"))));
}

#[test]
fn clipleaf_log_off_turns_debug_lines_off_but_keeps_errors() {
    assert!(!has_debug_lines(&run_failing(Some("off"))));
}

#[test]
fn without_or_with_an_unrecognized_value_the_build_default_applies() {
    for log in [None, Some(""), Some("verbose"), Some("DEBUG")] {
        assert_eq!(has_debug_lines(&run_failing(log)), cfg!(debug_assertions), "CLIPLEAF_LOG={log:?}");
    }
}
