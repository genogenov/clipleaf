use std::fmt::Display;
use std::io::Result;
use std::os::unix::ffi::OsStrExt;
use std::{ffi::OsString, path::PathBuf};

use clipleaf_core::{Colors, LOGGER, log_debug, parse_mime};

#[derive(Debug, PartialEq)]
pub enum Input {
    Args(Vec<OsString>),
    Stdin,
}

#[derive(Debug)]
pub struct CopyArgs {
    pub input: Input,
    pub files: bool,
    pub foreground: bool,
    pub mime: Option<String>,
    pub primary: bool,
    pub temp_dir: Option<PathBuf>,
}
pub enum Command {
    Help(String),
    Copy(CopyArgs),
}

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Command> {
    let mut args = args.into_iter();
    let program = args.next().unwrap_or_default();
    let mut out = CopyArgs {
        input: Input::Stdin,
        files: false,
        foreground: false,
        mime: None,
        primary: false,
        temp_dir: None,
    };
    let mut words = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_encoded_bytes() {
            b"--" => {
                words.extend(args);
                break;
            }
            b"-h" | b"--help" => return Ok(Command::Help(usage(&program))),
            b"-p" | b"--primary" => out.primary = true,
            b"--file" => out.files = true,
            b"--foreground" | b"-f" => out.foreground = true,
            b"-t" | b"--type" => out.mime = Some(parse_mime_from_args(args.next(), &program, &arg.display())?),
            b"--temp-dir" => out.temp_dir = Some(value(args.next(), &program, "--temp-dir")?.into()),
            [b'-', _, ..] => {
                return Err(usage_error(&program, &format!("unknown option '{}'", arg.display())));
            }
            _ => words.push(arg),
        }
    }

    if out.files && out.mime.is_some() {
        return Err(usage_error(&program, "cannot use --file together with --type"));
    }

    if !words.is_empty() {
        out.input = Input::Args(words);
    }

    log_debug!("Args parsed: {:?}", out);

    Ok(Command::Copy(out))
}

fn usage_error(program_cmd: &OsString, error: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{}\n{}", error, usage(program_cmd),))
}

pub(crate) fn usage(program_cmd: &OsString) -> String {
    let Colors { bold, cyan, reset, .. } = &LOGGER.colors;
    format!(
        concat!(
            "\n",
            "Usage:\n",
            "    {program} {bold}[options]{reset} [<content> | -- <content>]\n",
            "Options:\n",
            "    {bold}-t, --type {cyan}<mime>{reset}           offer the data as this MIME type\n",
            "    {bold}-p, --primary{reset}               set the primary selection instead of the clipboard\n",
            "    {bold}-f, --foreground{reset}            keep serving in the foreground instead of forking\n",
            "    {bold}--file{reset}                      treat the input as file paths and copy the files\n",
            "    {bold}--temp-dir {cyan}<dir>{reset}            where input over 128 KiB is kept (default: $TMPDIR or /tmp)\n",
            "    {bold}-h, --help{reset}                  show this help\n",
        ),
        program = program_cmd.display(),
        bold = bold,
        cyan = cyan,
        reset = reset,
    )
}

pub(crate) fn fallback_temp_dir() -> PathBuf {
    std::env::var_os("TMPDIR")
        .map(|v| if v.is_empty() { PathBuf::from("/tmp") } else { PathBuf::from(v) })
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn value(next: Option<OsString>, program: &OsString, option: impl Display) -> Result<OsString> {
    next.ok_or_else(|| usage_error(program, &format!("expected a value for option '{}'", option)))
}

fn parse_mime_from_args(next: Option<OsString>, program: &OsString, option: &impl Display) -> Result<String> {
    let arg_value = value(next, program, option)?;
    let value = parse_mime(arg_value.as_bytes()).map_err(|e| usage_error(program, &format!("invalid mime type for option '{}' - {e}", option)))?;

    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsStr, io::ErrorKind, iter, os::unix::ffi::OsStrExt};

    fn os(arg: &[u8]) -> OsString {
        OsStr::from_bytes(arg).to_os_string()
    }

    fn parse_os(args: Vec<OsString>) -> Result<Command> {
        parse_args(iter::once(OsString::from("copy")).chain(args))
    }

    fn parse(args: &[&str]) -> Result<Command> {
        parse_os(args.iter().map(OsString::from).collect())
    }

    fn copy_args(args: &[&str]) -> CopyArgs {
        match parse(args) {
            Ok(Command::Copy(parsed)) => parsed,
            Ok(Command::Help(_)) => panic!("{args:?} parsed as --help"),
            Err(err) => panic!("{args:?} failed: {err}"),
        }
    }

    fn invalid_os(args: Vec<OsString>) -> String {
        match parse_os(args) {
            Err(err) => {
                assert_eq!(err.kind(), ErrorKind::InvalidInput, "{err}");
                err.to_string()
            }
            Ok(_) => panic!("accepted"),
        }
    }

    fn invalid(args: &[&str]) -> String {
        invalid_os(args.iter().map(OsString::from).collect())
    }

    fn words(words: &[&str]) -> Input {
        Input::Args(words.iter().map(OsString::from).collect())
    }

    #[test]
    fn no_arguments_reads_stdin_with_defaults() {
        let parsed = copy_args(&[]);
        assert_eq!(parsed.input, Input::Stdin);
        assert!(!parsed.files && !parsed.primary && !parsed.foreground);
        assert_eq!((parsed.mime, parsed.temp_dir), (None, None));
    }

    #[test]
    fn words_are_kept_separate() {
        assert_eq!(copy_args(&["a", "b c"]).input, words(&["a", "b c"]));
    }

    #[test]
    fn double_dash_ends_options() {
        let parsed = copy_args(&["--", "-p", "-f", "--type", "x"]);
        assert_eq!(parsed.input, words(&["-p", "-f", "--type", "x"]));
        assert!(!parsed.primary && !parsed.foreground);
        assert_eq!(parsed.mime, None);

        let parsed = copy_args(&["-p", "--", "--"]);
        assert!(parsed.primary);
        assert_eq!(parsed.input, words(&["--"]));
    }

    #[test]
    fn options_may_follow_words() {
        let parsed = copy_args(&["hi", "-p"]);
        assert!(parsed.primary);
        assert_eq!(parsed.input, words(&["hi"]));
    }

    #[test]
    fn short_and_long_options() {
        for flag in ["-t", "--type"] {
            assert_eq!(copy_args(&[flag, "text/x-foo"]).mime.as_deref(), Some("text/x-foo"));
        }
        for flag in ["-p", "--primary"] {
            assert!(copy_args(&[flag]).primary);
        }
        for flag in ["-f", "--foreground"] {
            assert!(copy_args(&[flag]).foreground);
        }
        let parsed = copy_args(&["--file", "a.png"]);
        assert!(parsed.files);
        assert_eq!(parsed.input, words(&["a.png"]));
        assert_eq!(copy_args(&["--temp-dir", "/var/tmp"]).temp_dir, Some(PathBuf::from("/var/tmp")));

        let parsed = copy_args(&["-p", "-t", "image/png", "--temp-dir", "d", "x"]);
        assert!(parsed.primary);
        assert_eq!(parsed.mime.as_deref(), Some("image/png"));
        assert_eq!(parsed.temp_dir, Some(PathBuf::from("d")));
        assert_eq!(parsed.input, words(&["x"]));
    }

    #[test]
    fn help_is_returned_even_with_other_arguments_but_not_after_double_dash() {
        for args in [&["-h"][..], &["--help"], &["a", "-p", "-h"]] {
            let Ok(Command::Help(usage)) = parse(args) else {
                panic!("{args:?} did not return help");
            };
            assert!(usage.contains("Usage:") && usage.contains("--foreground"));
        }
        assert_eq!(copy_args(&["--", "-h"]).input, words(&["-h"]));
    }

    #[test]
    fn a_lone_dash_is_text() {
        assert_eq!(copy_args(&["-"]).input, words(&["-"]));
    }

    #[test]
    fn non_utf8_word_is_kept_byte_for_byte() {
        let Ok(Command::Copy(parsed)) = parse_os(vec![os(b"\xff\xfe")]) else {
            panic!("non-UTF-8 word rejected");
        };
        assert_eq!(parsed.input, Input::Args(vec![os(b"\xff\xfe")]));
    }

    #[test]
    fn mime_types_are_validated() {
        let libreoffice = r#"application/x-openoffice-objectdescriptor-xml;windows_formatname="Star Object Descriptor (XML)";displayname="file:///tmp/Résumé.docx""#;
        for mime in ["text/plain;charset=utf-8", "UTF8_STRING", libreoffice, &"a".repeat(4000)] {
            assert_eq!(copy_args(&["-t", mime]).mime.as_deref(), Some(mime));
        }
        for mime in ["", "text/\tplain", "text/\u{1b}[31m", "text/\u{9b}31m", &"a".repeat(4001)] {
            assert!(invalid(&["-t", mime]).contains("'-t'"), "{mime:?}");
        }
        assert!(invalid_os(vec![os(b"--type"), os(b"text/\xff")]).contains("'--type'"));
    }

    #[test]
    fn options_that_take_a_value_need_one() {
        for flag in ["-t", "--type", "--temp-dir"] {
            assert!(invalid(&[flag]).contains(&format!("'{flag}'")), "{flag}");
        }
    }

    #[test]
    fn unknown_options_are_errors_that_name_the_option() {
        for flag in ["-x", "--bogus", "-pt"] {
            assert!(invalid(&[flag, "text"]).contains(&format!("'{flag}'")), "{flag}");
        }
    }

    #[test]
    fn file_and_type_cannot_be_combined() {
        assert!(invalid(&["--file", "-t", "text/plain", "a.png"]).contains("--file"));
    }
}
