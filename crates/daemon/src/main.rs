#![forbid(unsafe_code)]
use std::{
    io::{self, Error},
    os::fd::AsFd,
    process::ExitCode,
};

use clipleaf_core::{
    PollFd, WlBufferedStream, WlDataControlOffer, WlSessionEvent, WlSessionManager,
    ffi::{POLLIN, POLLNVAL, POLLOUT},
    init_logging, log_error, log_info, poll,
};

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
    let mut stream = WlBufferedStream::connect_from_env()?;
    stream.set_nonblocking_writes();
    let mut session = WlSessionManager::initialize(stream)?;
    loop {
        session.flush()?; // one sendmsg per iteration
        let events = POLLIN
            | if session.has_pending_writes() {
                POLLOUT
            } else {
                0
            };
        let revents = {
            let mut fds = [PollFd::new(session.as_fd(), events)];
            poll(&mut fds, None)?;
            fds[0].revents()
        }; // borrow ends before the drain
        if revents & POLLNVAL != 0 {
            return Err(Error::other("wayland socket not open"));
        } // our bug
        // POLLIN, POLLHUP, POLLERR: drain. HUP/ERR end in Err(UnexpectedEof) from the drain.
        let mut changed_offer = false;
        let mut changed_primary_offer = false;
        while let Some(event) = session.try_next_event()? {
            if let WlSessionEvent::Selection { .. } = event {
                changed_offer = true;
            } // primary: ignored for now
            if let WlSessionEvent::PrimarySelection { .. } = event {
                changed_primary_offer = true;
            }
        }
        if changed_offer {
            log_selection(session.get_offer());
        }
        if changed_primary_offer {
            log_selection(session.get_primary_offer());
        }
    }
}

fn log_selection(offer: Option<&WlDataControlOffer>) {
    match offer {
        Some(o) => {
            log_info!("Offered mime types: {}", o.offered_mime_types().join(", "));
        }
        None => log_info!("selection cleared"),
    }
}
