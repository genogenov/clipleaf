use std::{
    borrow::Cow,
    io::{self, Error, ErrorKind},
    os::fd::{AsFd, OwnedFd},
};

use crate::{
    BoundInterface, DataControlDeviceEvent, DataControlOfferEvent, DataDeviceManagerExt, ExtDataControlManagerV1, WlBufferedStream,
    WlDataControlDevice, WlDataControlOffer, WlDataControlSourceEvent, WlDisplay, WlMessageReader, WlMessageRouter, WlOfferTracker, WlSeat,
    log_debug, pick_best_mime,
    unix_fd_stream::ReadMode,
    wl::{
        objects::{WlCallbackEvents, WlGlobal, WlInterface, wl_data_source::WlDataControlSource, wl_display::DisplayEvent, wl_registry::WlRegistry},
        wl_buffered_stream::{WlStreamWriter, WlWireMessage},
        wl_message_router::Route,
        wl_message_writer::WlMessageWriter,
    },
};

#[derive(Debug)]
pub enum WlSessionEvent {
    Selection { offer_id: Option<u32> },
    PrimarySelection { offer_id: Option<u32> },
    DataControlSend { source: u32, mime: Box<[u8]>, fd: OwnedFd },
    DataControlCancelled { source: u32 },
    SyncDone { callback_id: u32 },
}

pub struct WlSessionManager {
    stream: WlBufferedStream,
    registry: WlRegistry,
    router: WlMessageRouter,
    offer_tracker: WlOfferTracker,
    local_data_device: WlDataControlDevice,
    ext_data_control_manager: BoundInterface<ExtDataControlManagerV1>,
}

impl AsFd for WlSessionManager {
    fn as_fd(&self) -> std::os::unix::prelude::BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

impl WlSessionManager {
    pub fn initialize(mut stream: WlBufferedStream) -> Result<WlSessionManager, std::io::Error> {
        let mut router = WlMessageRouter::new();
        let mut registry = WlDisplay::get_registry(stream.get_writer(), &mut router)?;
        let mut offer_tracker = WlOfferTracker::new();
        let callback = WlDisplay::schedule_sync(stream.get_writer(), &mut router)?;
        stream.write()?;
        while !matches!(
            Self::read_event(
                &mut stream,
                &mut router,
                &mut registry,
                &mut offer_tracker,
                ReadMode::Block
            )?,
            WlSessionEvent::SyncDone { callback_id } if callback_id == callback
        ) {}

        log_debug!("Got registry: {:?}", registry);

        if let Some(ext_data_control_manager) = registry.get::<ExtDataControlManagerV1>() {
            log_debug!(
                "Found ExtDataControlManagerV1({}) with id {} and version {}",
                ExtDataControlManagerV1::NAME.str,
                ext_data_control_manager.global_name,
                ext_data_control_manager.version
            );

            let mgr_local = registry.bind(stream.get_writer(), &mut router, ext_data_control_manager)?;
            let seat_local = registry.bind(
                stream.get_writer(),
                &mut router,
                registry
                    .get::<WlSeat>()
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "wl_seat not found"))?,
            )?;

            let local_data_device = mgr_local.get_data_device(stream.get_writer(), &mut router, seat_local.local_id)?;

            log_debug!(
                "Bound ExtDataControlManagerV1 to local id {}, and WlSeat to local id {} and got DataDevice with local id {}",
                mgr_local.local_id,
                seat_local.local_id,
                local_data_device.local_id
            );

            Ok(WlSessionManager {
                stream,
                router,
                offer_tracker,
                ext_data_control_manager: mgr_local,
                local_data_device,
                registry,
            })
        } else {
            Err(std::io::Error::other("this compositor does not support ext_data_control_manager_v1"))
        }
    }

    pub fn create_data_source(&mut self) -> Result<WlDataControlSource, std::io::Error> {
        self.ext_data_control_manager
            .create_data_source(self.stream.get_writer(), &mut self.router)
    }

    pub fn set_selection(&mut self, source_id: u32) -> std::io::Result<()> {
        self.local_data_device.set_selection(self.stream.get_writer(), source_id)
    }

    pub fn set_primary_selection(&mut self, source_id: u32) -> std::io::Result<()> {
        self.local_data_device.set_primary_selection(self.stream.get_writer(), source_id)
    }

    pub fn schedule_sync(&mut self) -> Result<u32, std::io::Error> {
        let callback_id = WlDisplay::schedule_sync(self.stream.get_writer(), &mut self.router)?;
        Ok(callback_id)
    }

    pub fn next_event(&mut self) -> io::Result<WlSessionEvent> {
        Self::read_event(
            &mut self.stream,
            &mut self.router,
            &mut self.registry,
            &mut self.offer_tracker,
            ReadMode::Block,
        )
    }

    /// Returns the next event, or `Ok(None)` once no complete event is buffered
    /// and the socket has nothing more to read. One read can buffer several
    /// events, and buffered events don't make the socket readable: a caller that
    /// waits for readiness on `as_fd()` must call this until `Ok(None)` first.
    pub fn try_next_event(&mut self) -> io::Result<Option<WlSessionEvent>> {
        match Self::read_event(
            &mut self.stream,
            &mut self.router,
            &mut self.registry,
            &mut self.offer_tracker,
            ReadMode::NonBlock,
        ) {
            Ok(ev) => Ok(Some(ev)),
            Err(err) if err.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub fn destroy_source(&mut self, source: WlDataControlSource) -> std::io::Result<()> {
        let local_id = source.local_id;
        source.destroy(self.stream.get_writer())?;
        self.router.mark_destroyed(local_id)?;
        Ok(())
    }

    fn read_event(
        stream: &mut WlBufferedStream,
        router: &mut WlMessageRouter,
        registry: &mut WlRegistry,
        tracker: &mut WlOfferTracker,
        mode: ReadMode,
    ) -> io::Result<WlSessionEvent> {
        let (mut reader, mut writer) = stream.split();
        loop {
            if mode == ReadMode::Block {
                // flush queued messages before reading.
                writer.write_blocking()?;
            }

            let msg = reader.read_next_message(mode)?;
            if let Some(event) = match router.lookup_interface(msg.0.object_id)? {
                Route::Destroyed(WlInterface::DataControlSource) => {
                    log_debug!("Received message for destroyed DataControlSource object_id: {:?}", msg.0.object_id);
                    drop(WlDataControlSource::parse_message(msg.0.opcode, msg.1, msg.2)?);
                    None
                }
                Route::Destroyed(WlInterface::DataControlOffer) => {
                    WlDataControlOffer::parse_message(msg.0.opcode, msg.1, msg.2)?;
                    None
                }
                Route::Destroyed(other) => {
                    return Err(io::Error::other(format!("destroyed {other:?} not supported")));
                }
                Route::Live(interface) => Self::handle_event(router, registry, tracker, &mut writer, interface, msg)?,
            } {
                return Ok(event);
            }
        }
    }

    fn handle_event(
        router: &mut WlMessageRouter,
        registry: &mut WlRegistry,
        tracker: &mut WlOfferTracker,
        writer: &mut WlStreamWriter<'_>,
        interface: WlInterface,
        msg: WlWireMessage,
    ) -> io::Result<Option<WlSessionEvent>> {
        let (header, body, fds) = msg;
        match interface {
            WlInterface::Callback => {
                if header.opcode == WlCallbackEvents::Done as u16 {
                    log_debug!("Received done event from callback");
                    return Ok(Some(WlSessionEvent::SyncDone {
                        callback_id: header.object_id,
                    }));
                }
                Err(Error::other("Unknown opcode"))
            }
            WlInterface::Display => {
                let Some(display_event) = WlDisplay::parse_message(header.opcode, body) else {
                    return Ok(None);
                };
                match display_event {
                    DisplayEvent::Error {
                        target_object_id,
                        error_code,
                        error_msg,
                    } => Err(std::io::Error::other(format!(
                        "Received error message from Wayland socket: target_object_id={}, error_code={}, message={}",
                        target_object_id, error_code, error_msg
                    ))),
                    DisplayEvent::DeleteId { id } => {
                        log_debug!("Deleting WL client object id: {}", id);
                        router.free_client(id);
                        Ok(None)
                    }
                }
            }
            WlInterface::Seat => Ok(None),
            WlInterface::ExtDataControlManager => Ok(None),
            WlInterface::ZwlrDataControlManager => Ok(None),
            WlInterface::DataDeviceManager => Ok(None),
            WlInterface::DataControlDevice => {
                let data_control_device_event = WlDataControlDevice::parse_message(header.opcode, body, fds)?;
                match data_control_device_event {
                    DataControlDeviceEvent::DataOffer { new_id } => {
                        router.register_server_object(WlInterface::DataControlOffer, new_id)?;
                        tracker.data_device_offer(new_id)?;
                        Ok(None)
                    }
                    DataControlDeviceEvent::Selection { offer_id } => {
                        if let Some(prior_selection) = tracker.selection(&offer_id)? {
                            // log_debug!("Destroying prior selection: {:?}", prior_selection);
                            Self::drop_offer(router, tracker, writer, prior_selection)?;
                        }

                        Ok(Some(WlSessionEvent::Selection { offer_id }))
                    }
                    DataControlDeviceEvent::PrimarySelection { offer_id } => {
                        if let Some(prior_primary_selection) = tracker.primary_selection(&offer_id)? {
                            // log_debug!(
                            //     "Destroying prior primary selection: {:?}",
                            //     prior_primary_selection
                            // );
                            Self::drop_offer(router, tracker, writer, prior_primary_selection)?;
                        }
                        Ok(Some(WlSessionEvent::PrimarySelection { offer_id }))
                    }
                    DataControlDeviceEvent::Finished => Err(std::io::Error::other(format!(
                        "Received unexpected Finished event for DataControlDevice {}",
                        header.object_id
                    ))),
                }
            }
            WlInterface::DataControlSource => {
                let event = WlDataControlSource::parse_message(header.opcode, body, fds)?;
                match event {
                    WlDataControlSourceEvent::Send { mime_type, fd } => Ok(Some(WlSessionEvent::DataControlSend {
                        source: header.object_id,
                        mime: mime_type.into(),
                        fd,
                    })),
                    WlDataControlSourceEvent::Cancelled => Ok(Some(WlSessionEvent::DataControlCancelled { source: header.object_id })),
                }
            }
            WlInterface::DataControlOffer => match WlDataControlOffer::parse_message(header.opcode, body, fds)? {
                DataControlOfferEvent::Offer { mime } => {
                    tracker.data_control_offer(header.object_id, mime)?;
                    Ok(None)
                }
            },
            WlInterface::Registry => {
                registry.handle_event(&header, &mut WlMessageReader::new(body))?;
                Ok(None)
            }
        }
    }

    fn drop_offer(router: &mut WlMessageRouter, tracker: &mut WlOfferTracker, writer: &mut WlStreamWriter<'_>, id: u32) -> std::io::Result<()> {
        WlDataControlOffer::destroy(id, writer.get_writer())?;
        tracker.remove(id);
        router.mark_destroyed(id)
    }

    pub fn get_message_writer(&mut self) -> WlMessageWriter<'_> {
        self.stream.get_writer()
    }

    pub fn flush(&mut self) -> Result<usize, std::io::Error> {
        self.stream.write()
    }

    pub fn has_pending_writes(&self) -> bool {
        self.stream.has_pending_writes()
    }

    pub fn receive_offer(&mut self, primary: bool, preferred_mime: Option<&str>, fd: OwnedFd) -> Result<Option<&Cow<'_, str>>, std::io::Error> {
        let Some(offer) = (if primary {
            self.offer_tracker.get_primary_selected_slot()
        } else {
            self.offer_tracker.get_selected_slot()
        }) else {
            return Err(io::Error::other("nothing found to paste"));
        };

        log_debug!(
            "Selected offer: id = {}, mime = {:?}",
            offer.id(),
            pick_best_mime(offer.offered_mime_types())
        );

        let Some(preferred_mime) = preferred_mime.as_ref().map_or_else(
            || pick_best_mime(offer.offered_mime_types()).map(|m| *m.mime()),
            |s| offer.asked_mime_type(s),
        ) else {
            return Ok(None);
        };

        WlDataControlOffer::receive(offer.id(), self.stream.get_writer(), preferred_mime, fd)?;
        Ok(Some(preferred_mime))
    }

    pub fn get_offer(&self) -> Option<&WlDataControlOffer> {
        self.offer_tracker.get_selected_slot()
    }

    pub fn get_primary_offer(&self) -> Option<&WlDataControlOffer> {
        self.offer_tracker.get_primary_selected_slot()
    }
}
