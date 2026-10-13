//! Running the real clipleaf binaries against a [`TestCompositor`].
//!
//! Every child gets a cleared environment: nothing of the host's Wayland session can leak in.

use std::{
    io::{Read, Write},
    process::{Child, Command, Output, Stdio},
};

use crate::compositor::{Note, TestCompositor};

/// A command with an empty environment (no WAYLAND_DISPLAY, no XDG_RUNTIME_DIR), stdin closed.
pub fn bare(bin: &str, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.args(args).env_clear().stdin(Stdio::null());
    c
}

/// A command connected to the test compositor only.
pub fn command(bin: &str, comp: &TestCompositor, args: &[&str]) -> Command {
    let mut c = bare(bin, args);
    c.envs(comp.env());
    c
}

/// Runs to completion, capturing stdout and stderr.
pub fn run(bin: &str, comp: &TestCompositor, args: &[&str]) -> Output {
    command(bin, comp, args).output().expect("spawn")
}

/// A child that is killed and reaped when dropped, also when a test assertion fails.
pub struct Guarded(Option<Child>);

impl Guarded {
    pub fn new(child: Child) -> Self {
        Self(Some(child))
    }

    pub fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("child")
    }

    /// Waits for a normal exit.
    pub fn wait(mut self) -> std::process::ExitStatus {
        let mut child = self.0.take().expect("child");
        child.wait().expect("wait")
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Starts a long-running child (a foreground `clipleaf-copy`) with output discarded.
pub fn spawn(bin: &str, comp: &TestCompositor, args: &[&str]) -> Guarded {
    let child = command(bin, comp, args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    Guarded::new(child)
}

impl Guarded {
    /// Waits for the child and returns its status with whatever remains on its piped
    /// stdout and stderr.
    pub fn finish(&mut self) -> Output {
        let c = self.child();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(mut out) = c.stdout.take() {
            out.read_to_end(&mut stdout).expect("read stdout");
        }
        if let Some(mut err) = c.stderr.take() {
            err.read_to_end(&mut stderr).expect("read stderr");
        }
        let status = c.wait().expect("wait");
        Output {
            status,
            stdout,
            stderr,
        }
    }
}

/// A child whose connection to the compositor is known, so a wait for a note can fail fast.
///
/// The relay dispatches a client's buffered requests, and sends their notes, before it reports
/// that client's disconnect. So if the child exits (an error, a crash) the disconnect arrives
/// after every note it caused, and a test waiting for a note can stop waiting at the
/// disconnect and show the child's stderr. No clock is involved.
pub struct Started {
    pub child: Guarded,
    client: u32,
}

/// Spawns `command` with stderr captured (and stdin fed from `input`, if given) and returns it
/// once it has connected. Notes from earlier in the test are discarded first, so the first
/// `ClientConnected` is this child's. A child that dies before connecting is not detected here.
pub fn start(comp: &TestCompositor, mut command: Command, input: Option<&[u8]>) -> Started {
    while comp.notes.try_recv().is_ok() {}
    command.stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().expect("spawn");
    if let Some(input) = input {
        // The program reads all of stdin before it connects, so this cannot deadlock.
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(input).expect("write stdin");
    }
    let Note::ClientConnected(client) = comp.wait_for(|n| matches!(n, Note::ClientConnected(_)))
    else {
        unreachable!()
    };
    Started {
        child: Guarded::new(child),
        client,
    }
}

impl Started {
    /// The compositor's number for this child's connection.
    pub fn client(&self) -> u32 {
        self.client
    }

    /// Blocks for the first note matching `pred`; panics with the child's stderr if the child
    /// disconnects first.
    pub fn wait_for(&mut self, comp: &TestCompositor, mut pred: impl FnMut(&Note) -> bool) -> Note {
        loop {
            let note = comp.next_note();
            if pred(&note) {
                return note;
            }
            if note == Note::ClientDisconnected(self.client) {
                let output = self.child.finish();
                panic!(
                    "the child exited ({}) before the expected note; stderr:\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }

    /// Waits until the child owns the (primary) selection; returns the offered types.
    pub fn wait_selection(&mut self, comp: &TestCompositor, primary: bool) -> Vec<String> {
        match self.wait_for(
            comp,
            |n| matches!(n, Note::Selection { primary: p, mimes: Some(_) } if *p == primary),
        ) {
            Note::Selection { mimes, .. } => mimes.unwrap_or_default(),
            _ => unreachable!(),
        }
    }
}
