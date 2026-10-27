mod args;
use std::{
    borrow::Cow,
    env,
    io::{
        self,
        ErrorKind::{self, InvalidInput},
        IsTerminal, Write,
    },
    os::fd::{AsFd, OwnedFd},
    process::ExitCode,
};

use clipleaf_core::{
    WlBufferedStream, WlSessionEvent, WlSessionManager, ffi::fd::resize_pipe, init_logging, log_debug, log_error, mime_types::TEXT_MIME_TYPES,
};

use crate::args::{Command, PasteArgs};

fn main() -> ExitCode {
    init_logging();
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
    let callback_id = mgr.schedule_sync()?;
    mgr.flush()?;

    while !matches!(mgr.next_event()?, WlSessionEvent::SyncDone{callback_id: done_callback_id} if done_callback_id == callback_id) {}
    Ok(mgr)
}

fn write_offered_types_to_stdout(mimes: &[Cow<'static, str>]) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out_guard = stdout.lock();

    if !mimes.is_empty() {
        writeln!(out_guard, "{}", mimes.join("\n"))?;
    } else {
        return Err(io::Error::new(ErrorKind::NotFound, "No offered MIME types available"));
    }

    Ok(())
}

fn get_offered_types(mgr: &mut WlSessionManager, primary: bool) -> &[Cow<'static, str>] {
    if let Some(offer) = if primary { mgr.get_primary_offer() } else { mgr.get_offer() } {
        offer.offered_mime_types()
    } else {
        &[]
    }
}

fn write_all_to_stdout(args: &PasteArgs, mgr: &mut WlSessionManager) -> io::Result<()> {
    let (mut reader, writer) = io::pipe()?;
    let fd = OwnedFd::from(writer);

    // Best effort: a smaller pipe only costs speed.
    _ = resize_pipe(fd.as_fd(), 1 << 20);
    let Some(preferred_mime) = mgr.receive_offer(args.primary, args.mime.as_deref(), fd)? else {
        return Err(io::Error::new(
            InvalidInput,
            format!(
                "The requested MIME type {:?} is not available. The avaliable mime types are:\n{}",
                args.mime,
                get_offered_types(mgr, args.primary).join("\n")
            ),
        ));
    };

    log_debug!("Asked mime: {:?}, preferred mime: {:?}", args.mime, preferred_mime);

    let stdout = io::stdout();
    let is_terminal = stdout.is_terminal();
    let add_newline = !args.no_newline && is_terminal && is_text_mime(preferred_mime);

    mgr.flush()?;

    let mut out_guard = stdout.lock();
    match io::copy(&mut reader, &mut out_guard) {
        Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
        other => other.map(drop),
    }?;

    if add_newline {
        writeln!(out_guard)?;
    }

    Ok(())
}

fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/") || TEXT_MIME_TYPES.contains(&mime)
}
