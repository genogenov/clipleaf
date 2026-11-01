//! The gui.sock line protocol and socket path (the public lib API shared by the GUI and the ctl).

use std::{ffi::OsString, io::ErrorKind, path::PathBuf};

use clipleaf_default_gui::{GuiCommand, GuiResponse, socket_path};

#[test]
fn commands_round_trip() {
    for (cmd, line) in [
        (GuiCommand::Show, &b"show\n"[..]),
        (GuiCommand::Hide, b"hide\n"),
        (GuiCommand::Toggle, b"toggle\n"),
    ] {
        assert_eq!(cmd.as_sock_line(), line);
        assert_eq!(GuiCommand::parse_line(line), Some(cmd));
    }
}

#[test]
fn command_parse_needs_exactly_one_terminated_line() {
    assert_eq!(GuiCommand::parse_line(b"show"), None, "no newline");
    assert_eq!(GuiCommand::parse_line(b"show\nx"), None, "trailing bytes");
}

#[test]
fn responses_round_trip() {
    assert_eq!(GuiResponse::CommandOk.as_sock_line(), b"ok\n");
    assert_eq!(GuiResponse::Rejected.as_sock_line(), b"rejected\n");
    assert!(matches!(GuiResponse::parse_line(b"ok\n"), Some(GuiResponse::CommandOk)));
    assert!(matches!(GuiResponse::parse_line(b"rejected\n"), Some(GuiResponse::Rejected)));
    assert!(GuiResponse::parse_line(b"ok").is_none(), "no newline");
}

#[test]
fn socket_path_is_under_an_absolute_runtime_dir() {
    assert_eq!(
        socket_path(Some(OsString::from("/run/user/1000"))).unwrap(),
        PathBuf::from("/run/user/1000/clipleaf-default-gui/gui.sock")
    );
    for dir in [None, Some(OsString::from("run/user/1000")), Some(OsString::new())] {
        let err = socket_path(dir.clone()).expect_err(&format!("{dir:?} must be rejected"));
        assert_eq!(err.kind(), ErrorKind::NotFound, "{dir:?}");
    }
}
