//! The test compositor: a wayland-server (pure-Rust backend) relay that speaks just enough
//! ext-data-control-v1 for clipleaf, run on its own thread. [`compositor`] is the one place
//! tests get a compositor from.
//!
//! Tests wait on [`Note`]s (blocking channel receives), never on a clock. Dropping the
//! [`TestCompositor`] closes every client socket, so a detached `clipleaf-copy` sees EOF and exits.

use std::{
    ffi::OsStr,
    io::{self, PipeWriter},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::JoinHandle,
};

use rustix::event::{PollFd, PollFlags, poll};
use wayland_server::{Display, ListeningSocket};

pub(crate) mod relay {
    use std::os::fd::AsFd;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, mpsc::Sender};

    use wayland_protocols::ext::data_control::v1::server::{
        ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
        ext_data_control_manager_v1::{self, ExtDataControlManagerV1},
        ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
        ext_data_control_source_v1::{self, ExtDataControlSourceV1},
    };
    use wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        backend::ClientId,
        protocol::wl_seat::{self, WlSeat},
    };

    /// What the compositor reports to the test thread.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Note {
        /// A client connected; the number is its accept order (1-based).
        ClientConnected(u32),
        /// That client's socket closed (all its processes exited or closed it).
        ClientDisconnected(u32),
        /// The (primary) selection changed; `None` = cleared.
        Selection { primary: bool, mimes: Option<Vec<String>> },
        /// `cancelled` was sent to a replaced source.
        Cancelled { primary: bool },
        /// A `receive` was forwarded to the owning source as `send`.
        Forwarded { mime: String },
        /// A client sent `destroy` for one of its offers.
        OfferDestroyed,
    }

    #[derive(Default)]
    pub struct SourceData {
        mimes: Vec<String>,
        used: bool,
    }

    pub struct OfferData {
        source: ExtDataControlSourceV1,
        /// Set when the device was given a newer offer. Like wlroots, a `receive` on such an
        /// offer only closes the fd: it is not forwarded to the (old) owner.
        inert: AtomicBool,
    }

    /// The offers a device currently holds, so a replaced one can be made inert.
    #[derive(Default)]
    pub struct DeviceData {
        selection: Mutex<Option<ExtDataControlOfferV1>>,
        primary: Mutex<Option<ExtDataControlOfferV1>>,
    }

    struct Selection {
        source: ExtDataControlSourceV1,
        mimes: Vec<String>,
    }

    pub struct State {
        pub dh: DisplayHandle,
        pub notes: Sender<Note>,
        devices: Vec<ExtDataControlDeviceV1>,
        selection: Option<Selection>,
        primary: Option<Selection>,
    }

    impl State {
        pub fn new(dh: DisplayHandle, notes: Sender<Note>) -> Self {
            Self {
                dh,
                notes,
                devices: Vec::new(),
                selection: None,
                primary: None,
            }
        }

        fn slot(&mut self, primary: bool) -> &mut Option<Selection> {
            if primary { &mut self.primary } else { &mut self.selection }
        }

        /// data_offer + offer* + (primary_)selection to one device.
        fn advertise(&self, device: &ExtDataControlDeviceV1, primary: bool) {
            let current = if primary { &self.primary } else { &self.selection };
            let offer = current.as_ref().and_then(|sel| {
                let client = device.client()?;
                let offer = client
                    .create_resource::<ExtDataControlOfferV1, _, State>(
                        &self.dh,
                        device.version(),
                        OfferData {
                            source: sel.source.clone(),
                            inert: AtomicBool::new(false),
                        },
                    )
                    .ok()?;
                device.data_offer(&offer);
                for mime in &sel.mimes {
                    offer.offer(mime.clone());
                }
                Some(offer)
            });
            if let Some(held) = device.data::<DeviceData>() {
                let slot = if primary { &held.primary } else { &held.selection };
                let previous = std::mem::replace(&mut *slot.lock().unwrap(), offer.clone());
                if let Some(data) = previous.as_ref().and_then(|p| p.data::<OfferData>()) {
                    data.inert.store(true, Ordering::Relaxed);
                }
            }
            if primary {
                device.primary_selection(offer.as_ref());
            } else {
                device.selection(offer.as_ref());
            }
        }

        fn set(&mut self, primary: bool, new: Option<Selection>) {
            let mimes = new.as_ref().map(|s| s.mimes.clone());
            let old = std::mem::replace(self.slot(primary), new);
            if let Some(old) = old {
                old.source.cancelled();
                let _ = self.notes.send(Note::Cancelled { primary });
            }
            self.devices.retain(|d| d.is_alive());
            for device in &self.devices {
                self.advertise(device, primary);
            }
            let _ = self.notes.send(Note::Selection { primary, mimes });
        }
    }

    impl GlobalDispatch<WlSeat, ()> for State {
        fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<WlSeat>, _: &(), data_init: &mut DataInit<'_, Self>) {
            let seat = data_init.init(resource, ());
            seat.capabilities(wl_seat::Capability::empty());
            if seat.version() >= 2 {
                seat.name("seat0".into());
            }
        }
    }

    impl Dispatch<WlSeat, ()> for State {
        fn request(_: &mut Self, _: &Client, _: &WlSeat, _: wl_seat::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
            // get_pointer/keyboard/touch are never used by clipleaf; release is a destructor.
        }
    }

    impl GlobalDispatch<ExtDataControlManagerV1, ()> for State {
        fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<ExtDataControlManagerV1>, _: &(), data_init: &mut DataInit<'_, Self>) {
            data_init.init(resource, ());
        }
    }

    impl Dispatch<ExtDataControlManagerV1, ()> for State {
        fn request(
            state: &mut Self,
            _: &Client,
            _: &ExtDataControlManagerV1,
            request: ext_data_control_manager_v1::Request,
            _: &(),
            _: &DisplayHandle,
            data_init: &mut DataInit<'_, Self>,
        ) {
            use ext_data_control_manager_v1::Request;
            match request {
                Request::CreateDataSource { id } => {
                    data_init.init(id, Mutex::new(SourceData::default()));
                }
                Request::GetDataDevice { id, seat: _ } => {
                    let device = data_init.init(id, DeviceData::default());
                    // "The first selection event is sent upon binding the device."
                    state.advertise(&device, false);
                    state.advertise(&device, true);
                    state.devices.push(device);
                }
                Request::Destroy => {}
                _ => {}
            }
        }
    }

    impl Dispatch<ExtDataControlDeviceV1, DeviceData> for State {
        fn request(
            state: &mut Self,
            _: &Client,
            device: &ExtDataControlDeviceV1,
            request: ext_data_control_device_v1::Request,
            _: &DeviceData,
            _: &DisplayHandle,
            _: &mut DataInit<'_, Self>,
        ) {
            use ext_data_control_device_v1::Request;
            let (primary, source) = match request {
                Request::SetSelection { source } => (false, source),
                Request::SetPrimarySelection { source } => (true, source),
                Request::Destroy => return,
                _ => return,
            };
            let new = match source {
                None => None,
                Some(source) => {
                    let data = source.data::<Mutex<SourceData>>().expect("source data");
                    let mut data = data.lock().unwrap();
                    if data.used {
                        device.post_error(ext_data_control_device_v1::Error::UsedSource, "source already used");
                        return;
                    }
                    data.used = true;
                    let mimes = data.mimes.clone();
                    drop(data);
                    Some(Selection { source, mimes })
                }
            };
            state.set(primary, new);
        }
    }

    impl Dispatch<ExtDataControlSourceV1, Mutex<SourceData>> for State {
        fn request(
            _: &mut Self,
            _: &Client,
            source: &ExtDataControlSourceV1,
            request: ext_data_control_source_v1::Request,
            data: &Mutex<SourceData>,
            _: &DisplayHandle,
            _: &mut DataInit<'_, Self>,
        ) {
            if let ext_data_control_source_v1::Request::Offer { mime_type } = request {
                let mut data = data.lock().unwrap();
                if data.used {
                    source.post_error(ext_data_control_source_v1::Error::InvalidOffer, "offer after set_selection");
                    return;
                }
                data.mimes.push(mime_type);
            }
        }

        fn destroyed(state: &mut Self, _: ClientId, source: &ExtDataControlSourceV1, _: &Mutex<SourceData>) {
            // A destroyed (or disconnected) selection owner clears the selection,
            // as wlroots does. No `cancelled`: the object is gone.
            for primary in [false, true] {
                if state.slot(primary).as_ref().is_some_and(|s| &s.source == source) {
                    *state.slot(primary) = None;
                    state.devices.retain(|d| d.is_alive());
                    for device in &state.devices {
                        state.advertise(device, primary);
                    }
                    let _ = state.notes.send(Note::Selection { primary, mimes: None });
                }
            }
        }
    }

    impl Dispatch<ExtDataControlOfferV1, OfferData> for State {
        fn request(
            state: &mut Self,
            _: &Client,
            _: &ExtDataControlOfferV1,
            request: ext_data_control_offer_v1::Request,
            data: &OfferData,
            _: &DisplayHandle,
            _: &mut DataInit<'_, Self>,
        ) {
            match request {
                ext_data_control_offer_v1::Request::Receive { mime_type, fd } => {
                    // Relay to the owner. The backend dups `fd` into its out queue;
                    // our OwnedFd closes when `request` drops at the end of this call.
                    if data.source.is_alive() && !data.inert.load(Ordering::Relaxed) {
                        data.source.send(mime_type.clone(), fd.as_fd());
                        let _ = state.notes.send(Note::Forwarded { mime: mime_type });
                    }
                }
                ext_data_control_offer_v1::Request::Destroy => {
                    let _ = state.notes.send(Note::OfferDestroyed);
                }
                _ => {}
            }
        }
    }
}

pub use relay::Note;

struct ClientState {
    n: u32,
    notes: Sender<Note>,
}

impl wayland_server::backend::ClientData for ClientState {
    fn disconnected(&self, _: wayland_server::backend::ClientId, _: wayland_server::backend::DisconnectReason) {
        let _ = self.notes.send(Note::ClientDisconnected(self.n));
    }
}

/// Which globals the compositor advertises.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub seat: bool,
    pub data_control: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            seat: true,
            data_control: true,
        }
    }
}

/// A running compositor. Dropping it stops the thread, closes every client and removes the dir.
pub struct TestCompositor {
    runtime_dir: PathBuf,
    display_name: String,
    /// Notes in the order the compositor produced them.
    pub notes: Receiver<Note>,
    stop: Option<PipeWriter>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// The one bootstrap: a compositor with a seat and ext_data_control_manager_v1.
pub fn compositor() -> TestCompositor {
    compositor_with(Config::default())
}

/// Same bootstrap with some globals left out (the "compositor lacks X" scenarios).
pub fn compositor_with(config: Config) -> TestCompositor {
    TestCompositor::start(config).expect("cannot start the test compositor")
}

impl TestCompositor {
    fn start(config: Config) -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        // Under temp_dir(), not the target dir: a socket path must fit in SUN_LEN (~107 bytes).
        let parent = std::env::temp_dir();
        let runtime_dir = loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = parent.join(format!("clipleaf-wl-{}-{n}", std::process::id()));
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => break dir,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        let display_name = "wayland-test".to_owned();
        let listener = ListeningSocket::bind_absolute(runtime_dir.join(&display_name)).map_err(io::Error::other)?;

        let (tx, rx) = mpsc::channel();
        let (stop_rx, stop_tx) = io::pipe()?;
        let thread = std::thread::spawn(move || run(listener, tx, stop_rx, config));
        Ok(Self {
            runtime_dir,
            display_name,
            notes: rx,
            stop: Some(stop_tx),
            thread: Some(thread),
        })
    }

    /// The environment a client needs. Pass it to a child after clearing the inherited one.
    pub fn env(&self) -> [(&'static str, &OsStr); 2] {
        [
            ("XDG_RUNTIME_DIR", self.runtime_dir.as_os_str()),
            ("WAYLAND_DISPLAY", self.display_name.as_ref()),
        ]
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join(&self.display_name)
    }

    /// Blocks for the next note. No timeout: a hang is a bug the CI job timeout catches.
    pub fn next_note(&self) -> Note {
        self.notes.recv().expect("compositor thread ended")
    }

    /// Blocks until a note matching `pred` arrives, skipping others.
    pub fn wait_for(&self, mut pred: impl FnMut(&Note) -> bool) -> Note {
        loop {
            let note = self.next_note();
            if pred(&note) {
                return note;
            }
        }
    }

    /// Blocks until a (primary) selection with types is set; returns the types.
    pub fn wait_selection(&self, primary: bool) -> Vec<String> {
        match self.wait_for(|n| matches!(n, Note::Selection { primary: p, mimes: Some(_) } if *p == primary)) {
            Note::Selection { mimes, .. } => mimes.unwrap_or_default(),
            _ => unreachable!(),
        }
    }

    /// Blocks until the (primary) selection is cleared.
    pub fn wait_cleared(&self, primary: bool) {
        self.wait_for(|n| matches!(n, Note::Selection { primary: p, mimes: None } if *p == primary));
    }
}

impl Drop for TestCompositor {
    fn drop(&mut self) {
        drop(self.stop.take()); // EOF on the stop pipe wakes the poll
        let result = self.thread.take().map(|t| t.join());
        crate::procs::kill_leftovers(&self.runtime_dir);
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
        // A relay failure must not pass silently: a test that only checked a binary's failure
        // could otherwise pass for the wrong reason. (Never panic while already unwinding.)
        if !std::thread::panicking() {
            match result {
                Some(Err(panic)) => std::panic::resume_unwind(panic),
                Some(Ok(Err(e))) => panic!("the test compositor failed: {e}"),
                _ => {}
            }
        }
    }
}

fn run(listener: ListeningSocket, tx: Sender<Note>, stop: io::PipeReader, config: Config) -> io::Result<()> {
    let mut display: Display<relay::State> = Display::new().map_err(io::Error::other)?;
    let mut dh = display.handle();
    if config.seat {
        dh.create_global::<relay::State, wayland_server::protocol::wl_seat::WlSeat, ()>(9, ());
    }
    if config.data_control {
        dh.create_global::<relay::State, wayland_protocols::ext::data_control::v1::server::ext_data_control_manager_v1::ExtDataControlManagerV1, ()>(
            1,
            (),
        );
    }
    let mut state = relay::State::new(dh.clone(), tx);
    let mut clients = 0;

    loop {
        let (listener_ready, stop_ready) = {
            let poll_fd = display.backend().poll_fd();
            let mut fds = [
                PollFd::new(&listener, PollFlags::IN),
                PollFd::from_borrowed_fd(poll_fd, PollFlags::IN),
                PollFd::new(&stop, PollFlags::IN),
            ];
            poll(&mut fds, None)?;
            (!fds[0].revents().is_empty(), !fds[2].revents().is_empty())
        };
        if stop_ready {
            return Ok(());
        }
        if listener_ready {
            while let Some(stream) = listener.accept()? {
                clients += 1;
                let data = ClientState {
                    n: clients,
                    notes: state.notes.clone(),
                };
                dh.insert_client(stream, Arc::new(data))?;
                let _ = state.notes.send(Note::ClientConnected(clients));
            }
        }
        display.dispatch_clients(&mut state)?;
        display.flush_clients()?;
    }
}
