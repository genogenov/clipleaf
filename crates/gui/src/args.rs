use std::ffi::OsString;
use std::io::Result;

use clipleaf_core::{Colors, LOGGER};

#[derive(Debug, Eq, PartialEq)]
pub enum StartState {
    Default,
    Shown,
}

pub enum ArgCommand {
    Help(String),
    Cmd(StartState),
}

pub fn parse_args(mut args: impl ExactSizeIterator<Item = OsString>) -> Result<ArgCommand> {
    let program = args.next().unwrap_or_default();

    if args.len() > 1 {
        return Err(usage_error(&program, "too many arguments"));
    }
    if let Some(arg) = args.next() {
        match arg.as_encoded_bytes() {
            b"-h" | b"--help" => return Ok(ArgCommand::Help(usage(&program))),
            b"--show" => return Ok(ArgCommand::Cmd(StartState::Shown)),
            _ => {
                return Err(usage_error(&program, &format!("unknown option '{}'", arg.display())));
            }
        }
    }

    Ok(ArgCommand::Cmd(StartState::Default))
}

fn usage_error(program_cmd: &OsString, error: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{}\n{}", error, usage(program_cmd),))
}

pub(crate) fn usage(program_cmd: &OsString) -> String {
    let Colors { bold, reset, .. } = &LOGGER.colors;
    format!(
        concat!(
            "\n",
            "Usage:\n",
            "    {program} <options>\n",
            "Options:\n",
            "    {bold}--show{reset}                        show GUI(starts if not running)\n",
            "    {bold}-h, --help{reset}                    show this help\n",
        ),
        program = program_cmd.display(),
        bold = bold,
        reset = reset,
    )
}
