use std::fmt::Display;
use std::io::Result;
use std::{ffi::OsString, os::unix::ffi::OsStrExt};

use clipleaf_core::{Colors, LOGGER, log_debug, parse_mime};

#[derive(Debug)]
pub struct PasteArgs {
    pub no_newline: bool,
    pub mime: Option<String>,
    pub primary: bool,
    pub list_types: bool,
}
pub enum Command {
    Help(String),
    Paste(PasteArgs),
    ListTypes { primary: bool },
}

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Command> {
    let mut args = args.into_iter();
    let program = args.next().unwrap_or_default();
    let mut out = PasteArgs {
        no_newline: false,
        mime: None,
        primary: false,
        list_types: false,
    };
    while let Some(arg) = args.next() {
        match arg.as_encoded_bytes() {
            b"-h" | b"--help" => return Ok(Command::Help(usage(&program))),
            b"-p" | b"--primary" => out.primary = true,
            b"-l" | b"--list-types" => {
                out.list_types = true;
            }
            b"-n" | b"--no-newline" => out.no_newline = true,
            b"-t" | b"--type" => {
                out.mime = Some(parse_mime_from_args(args.next(), &program, &arg.display())?)
            }
            _ => {
                return Err(usage_error(
                    &program,
                    &format!("unknown option '{}'", arg.display()),
                ));
            }
        }
    }

    log_debug!("Args parsed: {:?}", out);

    if out.list_types {
        return Ok(Command::ListTypes {
            primary: out.primary,
        });
    }

    Ok(Command::Paste(out))
}

fn usage_error(program_cmd: &OsString, error: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{}\n{}", error, usage(program_cmd),),
    )
}

pub(crate) fn usage(program_cmd: &OsString) -> String {
    let Colors {
        bold, cyan, reset, ..
    } = &LOGGER.colors;
    format!(
        concat!(
            "\n",
            "Usage:\n",
            "    {program} {bold}[options]{reset}\n",
            "Options:\n",
            "    {bold}-t, --type {cyan}<mime>{reset}           paste as this MIME type if its offered.\n",
            "    {bold}-p, --primary{reset}               paste from the primary selection instead of the clipboard\n",
            "    {bold}-n, --no-newline{reset}                do not append a newline to the pasted content if its text and pasting to terminal\n",
            "    {bold}-l, --list-types{reset}            list available MIME types\n",
            "    {bold}-h, --help{reset}                  show this help\n",
        ),
        program = program_cmd.display(),
        bold = bold,
        cyan = cyan,
        reset = reset,
    )
}

fn value(next: Option<OsString>, program: &OsString, option: impl Display) -> Result<OsString> {
    next.ok_or_else(|| {
        usage_error(
            program,
            &format!("expected a value for option '{}'", option),
        )
    })
}

fn parse_mime_from_args(
    next: Option<OsString>,
    program: &OsString,
    option: &impl Display,
) -> Result<String> {
    let arg_value = value(next, program, option)?;
    let value = parse_mime(arg_value.as_bytes()).map_err(|e| {
        usage_error(
            program,
            &format!("invalid mime type for option '{}' - {e}", option),
        )
    })?;

    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsStr, io::ErrorKind, iter};

    fn parse_os(args: Vec<OsString>) -> Result<Command> {
        parse_args(iter::once(OsString::from("paste")).chain(args))
    }

    fn parse(args: &[&str]) -> Result<Command> {
        parse_os(args.iter().map(OsString::from).collect())
    }

    fn paste_args(args: &[&str]) -> PasteArgs {
        match parse(args) {
            Ok(Command::Paste(parsed)) => parsed,
            Ok(Command::Help(_)) => panic!("{args:?} parsed as --help"),
            Ok(Command::ListTypes { .. }) => panic!("{args:?} parsed as --list-types"),
            Err(err) => panic!("{args:?} failed: {err}"),
        }
    }

    fn invalid(args: &[&str]) -> String {
        match parse(args) {
            Err(err) => {
                assert_eq!(err.kind(), ErrorKind::InvalidInput, "{err}");
                err.to_string()
            }
            Ok(_) => panic!("{args:?} accepted"),
        }
    }

    #[test]
    fn no_arguments_pastes_the_best_clipboard_type() {
        let parsed = paste_args(&[]);
        assert!(!parsed.no_newline && !parsed.primary);
        assert_eq!(parsed.mime, None);
    }

    #[test]
    fn short_and_long_options() {
        for flag in ["-p", "--primary"] {
            assert!(paste_args(&[flag]).primary);
        }
        for flag in ["-n", "--no-newline"] {
            assert!(paste_args(&[flag]).no_newline);
        }
        for flag in ["-t", "--type"] {
            assert_eq!(
                paste_args(&[flag, "image/png"]).mime.as_deref(),
                Some("image/png")
            );
        }
        let parsed = paste_args(&["-n", "-p", "-t", "text/html"]);
        assert!(parsed.no_newline && parsed.primary);
        assert_eq!(parsed.mime.as_deref(), Some("text/html"));
    }

    #[test]
    fn list_types_keeps_the_selection_choice() {
        for args in [
            &["-l"][..],
            &["--list-types"],
            &["-p", "-l"],
            &["-l", "--primary"],
        ] {
            let Ok(Command::ListTypes { primary }) = parse(args) else {
                panic!("{args:?} did not list types");
            };
            assert_eq!(
                primary,
                args.iter()
                    .any(|a| a.starts_with("-p") || *a == "--primary")
            );
        }
    }

    #[test]
    fn help_wins_over_other_arguments() {
        for args in [&["-h"][..], &["--help"], &["-p", "-l", "-h"]] {
            let Ok(Command::Help(usage)) = parse(args) else {
                panic!("{args:?} did not return help");
            };
            assert!(usage.contains("Usage:") && usage.contains("--no-newline"));
        }
    }

    #[test]
    fn type_needs_a_valid_value() {
        for flag in ["-t", "--type"] {
            assert!(invalid(&[flag]).contains(&format!("'{flag}'")), "{flag}");
            assert!(invalid(&[flag, "text/\u{1b}[31m"]).contains(&format!("'{flag}'")));
        }
        let non_utf8 = OsStr::from_bytes(b"text/\xff").to_os_string();
        let Err(err) = parse_os(vec![OsString::from("-t"), non_utf8]) else {
            panic!("non-UTF-8 type accepted");
        };
        assert!(err.to_string().contains("'-t'"), "{err}");
    }

    #[test]
    fn words_and_unknown_options_are_errors_that_name_them() {
        for arg in ["hello", "-x", "--bogus", "-pn"] {
            assert!(invalid(&[arg]).contains(&format!("'{arg}'")), "{arg}");
        }
    }
}
