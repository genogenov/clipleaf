//! Socket path resolution from XDG_RUNTIME_DIR.

use std::{ffi::OsString, io, path::PathBuf};

use clipleaf_ipc::{RELATIVE_SOCKET_PATH, socket_path};

#[test]
fn relative_path_constant() {
    assert_eq!(RELATIVE_SOCKET_PATH, "clipleaf/daemon.sock");
}

#[test]
fn missing_empty_or_relative_runtime_dir_is_not_found() {
    for dir in [None, Some(""), Some("run/user/1000"), Some("./run"), Some("~/run")] {
        let res = socket_path(dir.map(OsString::from));
        let err = res.expect_err(&format!("{dir:?} must be rejected"));
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{dir:?}");
    }
}

#[test]
fn absolute_runtime_dir_joins_the_relative_path() {
    assert_eq!(
        socket_path(Some("/run/user/1000".into())).unwrap(),
        PathBuf::from("/run/user/1000/clipleaf/daemon.sock")
    );
    assert_eq!(
        socket_path(Some("/run/user/1000/".into())).unwrap(),
        PathBuf::from("/run/user/1000/clipleaf/daemon.sock")
    );
    assert_eq!(socket_path(Some("/".into())).unwrap(), PathBuf::from("/clipleaf/daemon.sock"));
}

#[test]
fn non_utf8_absolute_runtime_dir_is_kept_verbatim() {
    use std::os::unix::ffi::OsStringExt;
    let dir = OsString::from_vec(b"/tmp/\xff\xfe".to_vec());
    let mut want = PathBuf::from(dir.clone());
    want.push("clipleaf");
    want.push("daemon.sock");
    assert_eq!(socket_path(Some(dir)).unwrap(), want);
}
