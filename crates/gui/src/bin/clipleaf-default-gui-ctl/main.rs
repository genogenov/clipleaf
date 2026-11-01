mod args;

use std::{
    env::{self, current_exe},
    io::{self, ErrorKind, Read, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    process::{Command, ExitCode, Stdio},
    time::Duration,
};

use clipleaf_core::{init_logging, log_debug, log_error};
use clipleaf_default_gui::{EXE_NAME, GuiCommand, GuiControlError, GuiResponse, default_socket_path};

use crate::args::{ArgCommand, CtlArgs};

const MAX_LINE_LEN: usize = 64;

const EXIT_USAGE_ERROR: u8 = 2;

pub fn main() -> ExitCode {
    init_logging();

    let args = match args::parse_args(env::args_os()) {
        Ok(ArgCommand::Help(usage)) => {
            let _ = io::stdout().write_all(usage.as_bytes());
            return ExitCode::SUCCESS;
        }
        Ok(ArgCommand::Cmd(args)) => args,
        Err(e) => {
            log_error!("{e}");
            return ExitCode::from(EXIT_USAGE_ERROR);
        }
    };
    log_debug!("Args: {args:?}");

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log_error!("{}", err);
            ExitCode::FAILURE
        }
    }
}

fn run(args: &CtlArgs) -> io::Result<()> {
    match send(&args.cmd, Duration::from_secs(1)) {
        Ok(_) => Ok(()),
        Err(e) => match e {
            GuiControlError::NotRunning => {
                if args.cmd == GuiCommand::Hide {
                    log_debug!("Not running and hiding - done");
                    return Ok(());
                }
                // start...
                log_debug!("Not running - starting..");
                spawn_gui()?;
                return Ok(());
            }
            _ => return Err(io::Error::other(format!("An error occurred :{e}"))),
        },
    }
}

fn spawn_gui() -> io::Result<()> {
    let path = current_exe()?.with_file_name(EXE_NAME);

    Command::new(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .arg("--show")
        .process_group(0)
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("Error while starting gui from {} : {e}", path.display())))?;
    Ok(())
}

fn send(cmd: &GuiCommand, timeout: Duration) -> Result<(), GuiControlError> {
    let mut socket = match UnixStream::connect(default_socket_path()?) {
        Ok(s) => s,
        Err(e) => match e.kind() {
            ErrorKind::NotFound | ErrorKind::ConnectionRefused => return Err(GuiControlError::NotRunning),
            _ => {
                return Err(GuiControlError::Io(io::Error::new(
                    e.kind(),
                    format!("GUI Control: Failed to connect to socket: {e}"),
                )));
            }
        },
    };
    socket.set_read_timeout(Some(timeout))?;
    socket.set_write_timeout(Some(timeout))?;

    if let Err(e) = socket.write_all(cmd.as_sock_line()) {
        return Err(GuiControlError::Io(io::Error::new(
            e.kind(),
            format!("GUI Control: Failed to send command to socket: {e}"),
        )));
    }

    let mut buf = [0u8; MAX_LINE_LEN];
    let mut pos = 0;
    loop {
        if pos >= MAX_LINE_LEN {
            return Err(GuiControlError::InvalidData);
        }

        match socket.read(&mut buf[pos..]) {
            Ok(0) => return Err(GuiControlError::InvalidData),
            Ok(n) => pos += n,
            Err(e) => match e.kind() {
                ErrorKind::Interrupted => {}
                ErrorKind::UnexpectedEof => return Err(GuiControlError::InvalidData),
                ErrorKind::TimedOut => {
                    return Err(GuiControlError::Io(io::Error::new(
                        e.kind(),
                        "GUI Control: Timeout while waiting for response",
                    )));
                }
                _ => {
                    return Err(GuiControlError::Io(io::Error::new(
                        e.kind(),
                        format!("GUI Control: Failed to read response from socket: {e}"),
                    )));
                }
            },
        }

        if let Some(res) = GuiResponse::parse_line(&buf[..pos]) {
            return match res {
                GuiResponse::CommandOk => Ok(()),
                GuiResponse::Rejected => Err(GuiControlError::Rejected),
            };
        }

        if buf[..pos].contains(&b'\n') {
            return Err(GuiControlError::InvalidData);
        }
    }
}
