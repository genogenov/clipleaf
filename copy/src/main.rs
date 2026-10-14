mod args;
mod payload;
mod process_util;
use std::{
    env,
    fs::File,
    io::{self, ErrorKind, Write, stdin},
    os::{fd::AsFd, unix::ffi::OsStringExt},
    process::ExitCode,
};

use clipleaf_core::{
    OFFERED_TXT_MIME_TYPES, WlBufferedStream, WlSessionEvent, WlSessionManager, log_debug,
    log_error,
};

use crate::{
    args::{Command, Input},
    payload::Payload,
};

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
    let args = match args::parse_args(input_args)? {
        Command::Help(usage) => {
            let _ = io::stdout().write_all(usage.as_bytes());
            return Ok(());
        }
        Command::Copy(args) => args,
    };
    if args.files {
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "--file is not implemented yet",
        ));
    }
    let input = match args.input {
        Input::Args(words) => {
            let total = words
                .iter()
                .map(|w| w.len() + 1)
                .sum::<usize>()
                .saturating_sub(1);
            let mut words = words.into_iter();
            let mut text = words.next().unwrap_or_default().into_vec();
            text.reserve_exact(total - text.len());
            for word in words {
                text.push(b' ');
                text.extend_from_slice(word.as_encoded_bytes());
            }
            Payload::Bytes(text)
        }
        Input::Stdin => payload::store(
            &mut File::from(stdin().as_fd().try_clone_to_owned()?),
            &args.temp_dir.unwrap_or_else(args::fallback_temp_dir),
        )?,
    };

    let stream = WlBufferedStream::connect_from_env()?;
    log_debug!("Successfully connected to the Wayland socket");

    let mut mgr = WlSessionManager::initialize(stream)?;

    let data_source = mgr.create_data_source()?;
    if let Some(mime) = args.mime {
        data_source.offer(mgr.get_message_writer(), &mime)?;
    } else {
        for mime in OFFERED_TXT_MIME_TYPES {
            data_source.offer(mgr.get_message_writer(), mime)?;
        }
    }

    if args.primary {
        mgr.set_primary_selection(data_source.local_id)?;
    } else {
        mgr.set_selection(data_source.local_id)?;
    }

    let callback_id = mgr.sync()?;

    let mut detached = args.foreground;

    loop {
        match mgr.next_event()? {
            WlSessionEvent::DataControlSend { fd, .. } => {
                log_debug!("Received send event with fd {:?}", fd);

                // Serve to pasting client via fd it sent.
                if let Err(e) = input.serve(fd) {
                    log_error!("Failed to write to fd: {}", e);
                }
            }
            WlSessionEvent::DataControlCancelled { .. } => {
                log_debug!("Received cancelled event. Exiting...");
                return Ok(());
            }
            WlSessionEvent::SyncDone {
                callback_id: sync_callback_id,
            } if sync_callback_id == callback_id && !detached => {
                log_debug!("Received SyncDone - detaching from terminal");
                detached = true;
                match process_util::fork_process() {
                    Ok(process_util::Forked::Parent) => {
                        log_debug!("Parent process exiting after fork");
                        return Ok(());
                    }
                    Ok(process_util::Forked::Child) => {
                        if let Err(e) = process_util::detach() {
                            log_error!("Failed to detach child process: {}", e);
                        } else {
                            log_debug!("Successfully detached child process from terminal");
                        }
                    }
                    Err(e) => {
                        return Err(io::Error::new(
                            e.kind(),
                            format!("Failed to fork process: {e}"),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}
