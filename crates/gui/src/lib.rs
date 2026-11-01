use std::{env, error, ffi::OsString, fmt::Display, io, path::PathBuf};

pub const EXE_NAME: &str = "clipleaf-default-gui";
pub const RELATIVE_SOCKET_PATH: &str = "clipleaf-default-gui/gui.sock";

pub const GUI_CONTROL_MAX_LINE_LEN: usize = 64;

pub fn default_socket_path() -> io::Result<PathBuf> {
    socket_path(env::var_os("XDG_RUNTIME_DIR"))
}

pub fn socket_path(dir: Option<OsString>) -> io::Result<PathBuf> {
    let dir = runtime_dir(dir)?;
    Ok(dir.join(RELATIVE_SOCKET_PATH))
}

fn runtime_dir(runtime_dir: Option<OsString>) -> io::Result<PathBuf> {
    runtime_dir
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set or not an absolute path"))
}

#[derive(Debug, Eq, PartialEq)]
pub enum GuiCommand {
    Show,
    Hide,
    Toggle,
}

impl GuiCommand {
    pub const fn as_sock_line(&self) -> &'static [u8] {
        match self {
            GuiCommand::Show => "show\n".as_bytes(),
            GuiCommand::Hide => "hide\n".as_bytes(),
            GuiCommand::Toggle => "toggle\n".as_bytes(),
        }
    }

    pub const fn parse_line(line: &[u8]) -> Option<GuiCommand> {
        match line {
            b"show\n" => Some(GuiCommand::Show),
            b"hide\n" => Some(GuiCommand::Hide),
            b"toggle\n" => Some(GuiCommand::Toggle),
            _ => None,
        }
    }
}

pub enum GuiResponse {
    CommandOk,
    Rejected,
}

impl GuiResponse {
    pub fn as_sock_line(&self) -> &'static [u8] {
        match self {
            GuiResponse::CommandOk => "ok\n".as_bytes(),
            GuiResponse::Rejected => "rejected\n".as_bytes(),
        }
    }

    pub fn parse_line(line: &[u8]) -> Option<GuiResponse> {
        match line {
            b"ok\n" => Some(GuiResponse::CommandOk),
            b"rejected\n" => Some(GuiResponse::Rejected),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum GuiControlError {
    NotRunning,
    Rejected,
    Io(io::Error),
    InvalidData,
}

impl From<io::Error> for GuiControlError {
    fn from(e: io::Error) -> Self {
        GuiControlError::Io(e)
    }
}

impl Display for GuiControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuiControlError::NotRunning => write!(f, "GUI is not running"),
            GuiControlError::Rejected => write!(f, "Gui rejected the command."),
            GuiControlError::Io(error) => write!(f, "An I/O error occurred: {error}"),
            GuiControlError::InvalidData => write!(f, "GUI socket sent invalid data"),
        }
    }
}

impl error::Error for GuiControlError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            GuiControlError::Io(error) => Some(error),
            GuiControlError::NotRunning => None,
            GuiControlError::Rejected => None,
            GuiControlError::InvalidData => None,
        }
    }
}
