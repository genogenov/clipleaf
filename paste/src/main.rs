mod args;
use std::{
    borrow::Cow,
    env,
    io::{
        self,
        ErrorKind::{self, InvalidInput},
        IsTerminal, Write,
    },
    os::fd::{AsRawFd, OwnedFd},
    process::ExitCode,
};

use clipleaf_core::{
    OFFERED_TXT_MIME_TYPES, WlBufferedStream, WlSessionEvent, WlSessionManager,
    ffi::{F_SETPIPE_SZ, fcntl},
    log_debug, log_error,
};

use crate::args::{Command, PasteArgs};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log_error!("{}", err);
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let input_args = env::args_os();
    let command = args::parse_args(input_args)?;

    match command {
        Command::Help(usage) => {
            let _ = io::stdout().write_all(usage.as_bytes());
            Ok(())
        }
        Command::ListTypes { primary } => {
            let mut mgr = init()?;
            write_offered_types_to_stdout(get_offered_types(&mut mgr, primary))
        }
        Command::Paste(args) => {
            let mut mgr = init()?;
            write_all_to_stdout(&args, &mut mgr)
        }
    }
}

fn init() -> io::Result<WlSessionManager> {
    let stream = WlBufferedStream::connect_from_env()?;
    log_debug!("Successfully connected to the Wayland socket");

    let mut mgr = WlSessionManager::initialize(stream)?;

    log_debug!("Starting Paste loop");
    let callback_id = mgr.sync()?;

    while !matches!(mgr.next_event()?, WlSessionEvent::SyncDone{callback_id: done_callback_id} if done_callback_id == callback_id)
    {
    }
    Ok(mgr)
}

fn write_offered_types_to_stdout(mimes: &[Cow<'static, str>]) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out_guard = stdout.lock();

    if !mimes.is_empty() {
        writeln!(out_guard, "{}", mimes.join("\n"))?;
    } else {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "No offered MIME types available",
        ));
    }

    Ok(())
}

fn get_offered_types(mgr: &mut WlSessionManager, primary: bool) -> &[Cow<'static, str>] {
    if let Some(offer) = if primary {
        mgr.get_primary_offer()
    } else {
        mgr.get_offer()
    } {
        offer.offered_mime_types()
    } else {
        &[]
    }
}

fn write_all_to_stdout(args: &PasteArgs, mgr: &mut WlSessionManager) -> io::Result<()> {
    let (mut reader, writer) = io::pipe()?;
    let asked_mime = &args.mime;
    let fd = OwnedFd::from(writer);

    // SAFETY: fd is an open pipe we own and F_SETPIPE_SZ is a valid fcntl command for setting the pipe size, which takes one arg.
    // If this fails we are OK to ignore and proceed.
    unsafe { fcntl(fd.as_raw_fd(), F_SETPIPE_SZ, 1 << 20) };
    let Some(preferred_mime) = mgr.receive_offer(args.primary, asked_mime, fd)? else {
        return Err(io::Error::new(
            InvalidInput,
            format!(
                "The requested MIME type {:?} is not available. The avaliable mime types are:\n{}",
                asked_mime,
                get_offered_types(mgr, args.primary).join("\n")
            ),
        ));
    };

    log_debug!(
        "Asked mime: {:?}, preferred mime: {:?}",
        asked_mime,
        preferred_mime
    );

    let stdout = io::stdout();
    let is_terminal = stdout.is_terminal();
    let mut out_guard = stdout.lock();
    match io::copy(&mut reader, &mut out_guard) {
        Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
        other => other.map(drop),
    }?;

    if !args.no_newline && is_terminal && is_text_mime(preferred_mime) {
        writeln!(out_guard)?;
    }

    Ok(())
}

fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/") || OFFERED_TXT_MIME_TYPES.contains(&mime)
}
