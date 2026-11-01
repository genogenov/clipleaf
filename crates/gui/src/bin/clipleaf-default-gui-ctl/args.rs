use std::ffi::OsString;
use std::io::Result;

use clipleaf_core::{Colors, LOGGER};
use clipleaf_default_gui::GuiCommand;

#[derive(Debug)]
pub struct CtlArgs {
    pub cmd: GuiCommand,
}

pub enum ArgCommand {
    Help(String),
    Cmd(CtlArgs),
}

pub fn parse_args(mut args: impl ExactSizeIterator<Item = OsString>) -> Result<ArgCommand> {
    let program = args.next().unwrap_or_default();

    if args.len() > 1 {
        return Err(usage_error(&program, "too many arguments"));
    }
    if let Some(arg) = args.next() {
        match arg.as_encoded_bytes() {
            b"-h" | b"--help" => return Ok(ArgCommand::Help(usage(&program))),
            b"toggle" => return Ok(ArgCommand::Cmd(CtlArgs { cmd: GuiCommand::Toggle })),
            b"show" => return Ok(ArgCommand::Cmd(CtlArgs { cmd: GuiCommand::Show })),
            b"hide" => return Ok(ArgCommand::Cmd(CtlArgs { cmd: GuiCommand::Hide })),
            _ => {
                return Err(usage_error(&program, &format!("unknown option '{}'", arg.display())));
            }
        }
    }

    Ok(ArgCommand::Cmd(CtlArgs { cmd: GuiCommand::Toggle }))
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
            "    {program} {bold}[command]{reset} <options>\n",
            "Commands:\n",
            "    {bold}toggle{reset}                      toggle GUI visibility (starts if not running)\n",
            "    {bold}show{reset}                        show GUI(starts if not running)\n",
            "    {bold}hide{reset}                        hide GUI(if running)\n",
            "Options:\n",
            "    {bold}-h, --help{reset}                  show this help\n",
        ),
        program = program_cmd.display(),
        bold = bold,
        reset = reset,
    )
}
