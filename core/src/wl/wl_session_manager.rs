use std::{
    borrow::Cow,
    io::{self, Error, ErrorKind},
    os::fd::OwnedFd,
};

use crate::{
    BoundInterface, DataControlDeviceEvent, DataControlOfferEvent, DataDeviceManagerExt,
    ExtDataControlManagerV1, WlBufferedStream, WlDataControlDevice, WlDataControlOffer,
    WlDataControlSourceEvent, WlDisplay, WlMessageReader, WlMessageRouter, WlOfferTracker,
    log_debug,
    unix_fd_stream::ReadMode,
    wl::{
        objects::{
            WlCallbackEvents, wl_data_source::WlDataControlSource, wl_display::DisplayEvent,
            wl_registry::WlRegistry,
        },
        wl_buffered_stream::{WlStreamWriter, WlWireMessage},
        wl_message_router::WlInterface::{self},
        wl_message_writer::WlMessageWriter,
    },
};

#[derive(Debug)]
pub enum WlSessionEvent {
    Selection {
        offer_id: Option<u32>,
    },
    PrimarySelection {
        offer_id: Option<u32>,
    },
    DataControlSend {
        source: u32,
        mime: Box<[u8]>,
        fd: OwnedFd,
    },
    DataControlCancelled {
        source: u32,
    },
    SyncDone,
}

pub struct WlSessionManager {
    display: WlDisplay,
    stream: WlBufferedStream,
    registry: WlRegistry,
    router: WlMessageRouter,
    offer_tracker: WlOfferTracker,
    local_data_device: WlDataControlDevice,
    ext_data_control_manager: BoundInterface<ExtDataControlManagerV1>,
}

impl WlSessionManager {
    pub fn initialize(mut stream: WlBufferedStream) -> Result<WlSessionManager, std::io::Error> {
        let mut display = WlDisplay::new();
        let mut router = WlMessageRouter::new();
        let mut registry = display.get_registry(stream.get_writer(), &mut router)?;
        let mut offer_tracker = WlOfferTracker::new();
        display.schedule_sync(stream.get_writer(), &mut router)?;
        stream.write()?;
        while !matches!(
            Self::read_event(
                &mut stream,
                &mut router,
                &mut registry,
                &mut offer_tracker,
                ReadMode::Block
            )?,
            WlSessionEvent::SyncDone
        ) {}

        log_debug!("Got registry: {:?}", registry);

        if let Some(ext_data_control_manager) = registry.ext_data_control_manager {
            log_debug!(
                "Found ExtDataControlManagerV1({}) with id {} and version {}",
                ext_data_control_manager.interface_name.str,
                ext_data_control_manager.global_name,
                ext_data_control_manager.version
            );

            let mgr_local =
                registry.bind(stream.get_writer(), &mut router, ext_data_control_manager)?;
            let seat_local = registry.bind(
                stream.get_writer(),
                &mut router,
                registry.wl_seat.ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "wl_seat not found")
                })?,
            )?;

            let local_data_device =
                mgr_local.get_data_device(stream.get_writer(), &mut router, seat_local.local_id)?;

            log_debug!(
                "Bound ExtDataControlManagerV1 to local id {}, and WlSeat to local id {} and got DataDevice with local id {}",
                mgr_local.local_id,
                seat_local.local_id,
                local_data_device.local_id
            );

            Ok(WlSessionManager {
                display,
                stream,
                router,
                offer_tracker,
                ext_data_control_manager: mgr_local,
                local_data_device,
                registry,
            })
        } else {
            Err(std::io::Error::other(
                "this compositor does not support ext_data_control_manager_v1",
            ))
        }
    }

    pub fn create_data_source(&mut self) -> Result<WlDataControlSource, std::io::Error> {
        self.ext_data_control_manager
            .create_data_source(self.stream.get_writer(), &mut self.router)
    }

    pub fn set_selection(&mut self, source_id: u32) -> std::io::Result<()> {
        self.local_data_device
            .set_selection(self.stream.get_writer(), source_id)
    }

    pub fn set_primary_selection(&mut self, source_id: u32) -> std::io::Result<()> {
        self.local_data_device
            .set_primary_selection(self.stream.get_writer(), source_id)
    }

    pub fn sync(&mut self) -> Result<(), std::io::Error> {
        self.display
            .schedule_sync(self.stream.get_writer(), &mut self.router)?;
        self.stream.write()
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

    fn read_event(
        stream: &mut WlBufferedStream,
        router: &mut WlMessageRouter,
        registry: &mut WlRegistry,
        tracker: &mut WlOfferTracker,
        mode: ReadMode,
    ) -> io::Result<WlSessionEvent> {
        let (mut reader, mut writer) = stream.split();
        loop {
            // flush queued messages before reading.
            writer.write()?;

            let msg = reader.read_next_message(mode)?;
            let Some(interface) = router.lookup_interface(msg.0.object_id)? else {
                continue;
            };
            if let Some(event) =
                Self::handle_event(router, registry, tracker, &mut writer, interface, msg)?
            {
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
                    return Ok(Some(WlSessionEvent::SyncDone));
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
                let data_control_device_event =
                    WlDataControlDevice::parse_message(header.opcode, body, fds)?;
                match data_control_device_event {
                    DataControlDeviceEvent::DataOffer { new_id } => {
                        router.register_server_object(WlInterface::DataControlOffer, new_id)?;
                        tracker.data_device_offer(new_id)?;
                        Ok(None)
                    }
                    DataControlDeviceEvent::Selection { offer_id } => {
                        if let Some(prior_selection) = tracker.selection(&offer_id)? {
                            log_debug!("Destroying prior selection: {:?}", prior_selection);
                            Self::drop_offer(router, tracker, writer, prior_selection)?;
                        }

                        Ok(Some(WlSessionEvent::Selection { offer_id }))
                    }
                    DataControlDeviceEvent::PrimarySelection { offer_id } => {
                        if let Some(prior_primary_selection) =
                            tracker.primary_selection(&offer_id)?
                        {
                            log_debug!(
                                "Destroying prior primary selection: {:?}",
                                prior_primary_selection
                            );
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
                    WlDataControlSourceEvent::Send { mime_type, fd } => {
                        Ok(Some(WlSessionEvent::DataControlSend {
                            source: header.object_id,
                            mime: mime_type.into(),
                            fd,
                        }))
                    }
                    WlDataControlSourceEvent::Cancelled => {
                        Ok(Some(WlSessionEvent::DataControlCancelled {
                            source: header.object_id,
                        }))
                    }
                }
            }
            WlInterface::DataControlOffer => {
                match WlDataControlOffer::parse_message(header.opcode, body, fds)? {
                    DataControlOfferEvent::Offer { mime } => {
                        tracker.data_control_offer(header.object_id, mime)?;
                        Ok(None)
                    }
                }
            }
            WlInterface::Registry => {
                registry.add_interface(&header, &mut WlMessageReader::new(body));
                Ok(None)
            }
        }
    }

    fn drop_offer(
        router: &mut WlMessageRouter,
        tracker: &mut WlOfferTracker,
        writer: &mut WlStreamWriter<'_>,
        id: u32,
    ) -> std::io::Result<()> {
        WlDataControlOffer::destroy(id, writer.get_writer())?;
        tracker.remove(id);
        router.free_server(id)
    }

    // fn dispatch<F>(
    //     stream: &mut WlBufferedStream,
    //     router: &mut WlMessageRouter,
    //     registry: &mut WlRegistry,
    //     tracker: &mut WlOfferTracker,
    //     fnhandler: &mut F,
    // ) -> Result<(), std::io::Error>
    // where
    //     F: WlMessageHandler,
    // {
    //     log_debug!("Starting message dispatch loop");
    //     let (mut reader, mut writer) = stream.split();
    //     loop {
    //         // Requests queued by handlers must reach the compositor before blocking on a read.
    //         writer.write()?;

    //         let Some(event) = router.next_event(&mut reader, ReadMode::Block)? else {
    //             break;
    //         };
    //         let Some(event_result) =
    //             (match Self::handle_event(router, registry, tracker, &mut writer, event)
    //                 .continue_ok()
    //             {
    //                 Ok(it) => it,
    //                 Err(err) => return err,
    //             })
    //         else {
    //             continue;
    //         };
    //         if let ControlFlow::Break(result) = fnhandler(event_result) {
    //             return result;
    //         }
    //     }

    //     Err(std::io::Error::new(
    //         ErrorKind::UnexpectedEof,
    //         "Unexpected end of stream",
    //     ))
    // }

    pub fn get_message_writer(&mut self) -> WlMessageWriter<'_> {
        self.stream.get_writer()
    }

    pub fn send_messages(&mut self) -> Result<(), std::io::Error> {
        self.stream.write()
    }

    pub fn receive_offer(
        &mut self,
        primary: bool,
        preferred_mime: &Option<String>,
        fd: OwnedFd,
    ) -> Result<Option<&Cow<'_, str>>, std::io::Error> {
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
            offer.preferred_mime_type()
        );

        let Some(preferred_mime) = preferred_mime
            .as_ref()
            .map_or_else(|| offer.preferred_mime_type(), |s| offer.asked_mime_type(s))
        else {
            return Ok(None);
        };

        WlDataControlOffer::receive(offer.id(), self.stream.get_writer(), preferred_mime, fd)?;
        self.stream.write()?;
        Ok(Some(preferred_mime))
    }

    pub fn get_offer(&self) -> Option<&WlDataControlOffer> {
        self.offer_tracker.get_selected_slot()
    }

    pub fn get_primary_offer(&self) -> Option<&WlDataControlOffer> {
        self.offer_tracker.get_primary_selected_slot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wl::objects::{
        WlCallbackEvents,
        wl_data_control_device::WlDataControlDeviceEvents,
        wl_data_managers::ExtDataControlManagerOps,
        wl_data_offer::{WlDataControlOfferEvents, WlDataControlOfferOps},
        wl_display::{DisplayEvents, DisplayOps},
        wl_registry::{RegistryEvents, RegistryOps},
    };
    use std::{
        io::{ErrorKind, Write},
        net::Shutdown,
        os::unix::net::UnixStream,
    };

    fn msg(object_id: u32, opcode: u16, body: &[u8]) -> Vec<u8> {
        let mut m = Vec::with_capacity(8 + body.len());
        m.extend_from_slice(&object_id.to_ne_bytes());
        m.extend_from_slice(&(((8 + body.len()) as u32) << 16 | u32::from(opcode)).to_ne_bytes());
        m.extend_from_slice(body);
        m
    }

    fn wl_str(s: &str) -> Vec<u8> {
        let mut out = ((s.len() + 1) as u32).to_ne_bytes().to_vec();
        out.extend_from_slice(s.as_bytes());
        out.push(0);
        out.resize(out.len().next_multiple_of(4), 0);
        out
    }

    // First ids the client hands out: 0 is reserved, 1 is wl_display.
    const REGISTRY_ID: u32 = 2;
    const CALLBACK_ID: u32 = 3;

    // Queues the compositor's replies to get_registry + sync before the client reads them.
    fn compositor_with_globals(globals: &[(u32, &str, u32)]) -> (WlBufferedStream, UnixStream) {
        let (client, mut compositor) = UnixStream::pair().unwrap();
        let mut events = Vec::new();
        for &(name, interface, version) in globals {
            let mut body = name.to_ne_bytes().to_vec();
            body.extend(wl_str(interface));
            body.extend(version.to_ne_bytes());
            events.extend(msg(REGISTRY_ID, RegistryEvents::Global as u16, &body));
        }
        events.extend(msg(
            CALLBACK_ID,
            WlCallbackEvents::Done as u16,
            &0u32.to_ne_bytes(),
        ));
        compositor.write_all(&events).unwrap();
        (WlBufferedStream::new(client.into()), compositor)
    }

    // #[test]
    // fn initialize_binds_the_manager_and_the_seat_and_creates_a_device() {
    //     let (stream, compositor) = compositor_with_globals(&[
    //         (1, "wl_seat", 9),
    //         (7, "wl_output", 4),
    //         (54, "ext_data_control_manager_v1", 1),
    //     ]);
    //     let mut session = WlSessionManager::initialize(stream).unwrap();
    //     session.send_messages().unwrap();

    //     let mut compositor = WlBufferedStream::new(compositor.into());
    //     let mut next = || {
    //         let (header, body, _) = compositor.read_next_message().unwrap().unwrap();
    //         (header.object_id, header.opcode, body.to_vec())
    //     };

    //     let (id, op, body) = next();
    //     assert_eq!(
    //         (id, op),
    //         (WlDisplay::TYPE_ID, DisplayOps::GetRegistry.into())
    //     );
    //     assert_eq!(WlMessageReader::new(&body).u32(), Some(REGISTRY_ID));

    //     let (id, op, body) = next();
    //     assert_eq!((id, op), (WlDisplay::TYPE_ID, DisplayOps::Sync.into()));
    //     assert_eq!(WlMessageReader::new(&body).u32(), Some(CALLBACK_ID));

    //     // Server versions 1 and 9 are clamped to what is implemented (1 for both).
    //     for (name, interface, new_id) in [
    //         (54, &b"ext_data_control_manager_v1"[..], 4),
    //         (1, b"wl_seat", 5),
    //     ] {
    //         let (id, op, body) = next();
    //         assert_eq!((id, op), (REGISTRY_ID, RegistryOps::Bind.into()));
    //         let mut r = WlMessageReader::new(&body);
    //         assert_eq!(
    //             (r.u32(), r.str(), r.u32(), r.u32()),
    //             (Some(name), Some(interface), Some(1), Some(new_id))
    //         );
    //     }

    //     let (id, op, body) = next();
    //     assert_eq!(
    //         (id, op),
    //         (4, ExtDataControlManagerOps::GetDataDevice.into())
    //     );
    //     let mut r = WlMessageReader::new(&body);
    //     assert_eq!(
    //         (r.u32(), r.u32()),
    //         (Some(6), Some(5)),
    //         "new device id, seat id"
    //     );
    // }

    #[test]
    fn initialize_fails_without_ext_data_control() {
        let (stream, _compositor) = compositor_with_globals(&[(1, "wl_seat", 9)]);
        let Err(err) = WlSessionManager::initialize(stream) else {
            panic!("initialized without ext_data_control_manager_v1");
        };
        assert!(err.to_string().contains("ext_data_control_manager_v1"));
    }

    #[test]
    fn initialize_fails_without_a_seat() {
        let (stream, _compositor) =
            compositor_with_globals(&[(54, "ext_data_control_manager_v1", 1)]);
        let Err(err) = WlSessionManager::initialize(stream) else {
            panic!("initialized without wl_seat");
        };
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    const SEAT_ID: u32 = 5;
    const DEVICE_ID: u32 = 6;

    fn session_with_device() -> (WlSessionManager, UnixStream) {
        let (stream, compositor) =
            compositor_with_globals(&[(1, "wl_seat", 1), (54, "ext_data_control_manager_v1", 1)]);
        (WlSessionManager::initialize(stream).unwrap(), compositor)
    }

    // Only the compositor-to-client direction is closed, so the client can still send requests.
    fn send_then_eof(compositor: &mut UnixStream, events: &[u8]) {
        compositor.write_all(events).unwrap();
        compositor.shutdown(Shutdown::Write).unwrap();
    }

    // fn dispatch_to_eof(session: &mut WlSessionManager) {
    //     let err = session
    //         .dispatch_messages(&mut |_| ControlFlow::Continue(()))
    //         .unwrap_err();
    //     assert_eq!(err.kind(), ErrorKind::UnexpectedEof, "{err}");
    // }

    // Closes the client and returns every offer it destroyed, in order.
    // fn destroyed_offers(session: WlSessionManager, compositor: UnixStream) -> Vec<u32> {
    //     drop(session);
    //     let mut requests = WlBufferedStream::new(compositor.into());
    //     let mut destroyed = Vec::new();
    //     while let Some((header, ..)) = requests.read_next_message().unwrap() {
    //         if header.object_id >= 0xff00_0000
    //             && header.opcode == u16::from(WlDataControlOfferOps::Destroy)
    //         {
    //             destroyed.push(header.object_id);
    //         }
    //     }
    //     destroyed
    // }

    fn device_event(event: WlDataControlDeviceEvents, arg: u32) -> Vec<u8> {
        msg(DEVICE_ID, event.into(), &arg.to_ne_bytes())
    }

    fn mime_offer(offer: u32, mime: &str) -> Vec<u8> {
        msg(offer, WlDataControlOfferEvents::Offer.into(), &wl_str(mime))
    }

    // #[test]
    // fn dispatch_returns_on_break_and_the_next_call_sees_the_rest() {
    //     let (mut session, mut compositor) = session_with_device();
    //     let mut events = msg(
    //         CALLBACK_ID,
    //         WlCallbackEvents::Done as u16,
    //         &0u32.to_ne_bytes(),
    //     );
    //     events.extend(msg(
    //         WlDisplay::TYPE_ID,
    //         DisplayEvents::DeleteId.into(),
    //         &CALLBACK_ID.to_ne_bytes(),
    //     ));
    //     send_then_eof(&mut compositor, &events);

    //     session
    //         .dispatch_messages(&mut |ev| match ev {
    //             WlEvent::SyncDone => ControlFlow::Break(Ok(())),
    //             _ => ControlFlow::Continue(()),
    //         })
    //         .unwrap();
    //     // delete_id arrived in the same read as done: it must still be handled.
    //     dispatch_to_eof(&mut session);
    //     assert_eq!(session.create_data_source().unwrap().local_id, CALLBACK_ID);
    // }

    // #[test]
    // fn a_display_error_ends_dispatch_with_its_message() {
    //     let (mut session, mut compositor) = session_with_device();
    //     let mut body = DEVICE_ID.to_ne_bytes().to_vec();
    //     body.extend(1u32.to_ne_bytes());
    //     body.extend(wl_str("invalid arguments"));
    //     send_then_eof(
    //         &mut compositor,
    //         &msg(WlDisplay::TYPE_ID, DisplayEvents::Error.into(), &body),
    //     );

    //     let err = session
    //         .dispatch_messages(&mut |_| ControlFlow::Continue(()))
    //         .unwrap_err();
    //     assert!(err.to_string().contains("invalid arguments"), "{err}");
    // }

    // #[test]
    // fn malformed_or_unexpected_device_events_are_errors() {
    //     let (mut session, mut compositor) = session_with_device();
    //     let cases = [
    //         ("unknown opcode", msg(DEVICE_ID, 9, &[])),
    //         (
    //             "missing argument",
    //             msg(DEVICE_ID, WlDataControlDeviceEvents::Selection.into(), &[]),
    //         ),
    //         (
    //             "new_id outside the server range",
    //             device_event(WlDataControlDeviceEvents::DataOffer, 5),
    //         ),
    //         (
    //             "finished",
    //             msg(DEVICE_ID, WlDataControlDeviceEvents::Finished.into(), &[]),
    //         ),
    //     ];
    //     let events: Vec<u8> = cases.iter().flat_map(|(_, m)| m.clone()).collect();
    //     send_then_eof(&mut compositor, &events);

    //     // Each error consumes only its own message, so the next dispatch reports the next case.
    //     for (case, _) in cases {
    //         let err = session
    //             .dispatch_messages(&mut |_| ControlFlow::Continue(()))
    //             .unwrap_err();
    //         assert_ne!(err.kind(), ErrorKind::UnexpectedEof, "{case}");
    //     }
    //     dispatch_to_eof(&mut session);
    // }

    // #[test]
    // fn paste_burst_is_routed_per_offer_and_tracks_both_selections() {
    //     let (mut session, mut compositor) = session_with_device();
    //     let (clipboard, primary) = (0xff00_0000u32, 0xff00_0001u32);

    //     // What a compositor sends right after bind + get_data_device + sync, in one write.
    //     let mut burst = msg(SEAT_ID, 0, &3u32.to_ne_bytes());
    //     burst.extend(device_event(
    //         WlDataControlDeviceEvents::DataOffer,
    //         clipboard,
    //     ));
    //     burst.extend(mime_offer(clipboard, "text/html"));
    //     burst.extend(mime_offer(clipboard, "text/plain;charset=utf-8"));
    //     burst.extend(device_event(
    //         WlDataControlDeviceEvents::Selection,
    //         clipboard,
    //     ));
    //     burst.extend(device_event(WlDataControlDeviceEvents::DataOffer, primary));
    //     burst.extend(mime_offer(primary, "text/plain"));
    //     burst.extend(device_event(
    //         WlDataControlDeviceEvents::PrimarySelection,
    //         primary,
    //     ));
    //     burst.extend(msg(
    //         CALLBACK_ID,
    //         WlCallbackEvents::Done as u16,
    //         &0u32.to_ne_bytes(),
    //     ));
    //     send_then_eof(&mut compositor, &burst);

    //     let mut mimes = Vec::new();
    //     session
    //         .dispatch_messages(&mut |ev| match ev {
    //             WlEvent::DataControlOffer {
    //                 id,
    //                 event: DataControlOfferEvent::Offer { mime },
    //             } => {
    //                 mimes.push((id, mime.to_vec()));
    //                 ControlFlow::Continue(())
    //             }
    //             WlEvent::SyncDone => ControlFlow::Break(Ok(())),
    //             _ => ControlFlow::Continue(()),
    //         })
    //         .unwrap();

    //     assert_eq!(
    //         mimes,
    //         [
    //             (clipboard, b"text/html".to_vec()),
    //             (clipboard, b"text/plain;charset=utf-8".to_vec()),
    //             (primary, b"text/plain".to_vec()),
    //         ]
    //     );
    //     fn summary(offer: Option<&WlDataControlOffer>) -> (u32, Vec<&str>, Option<&str>) {
    //         let offer = offer.unwrap();
    //         let types = offer
    //             .offered_mime_types()
    //             .iter()
    //             .map(|m| m.as_ref())
    //             .collect();
    //         (
    //             offer.id(),
    //             types,
    //             offer.preferred_mime_type().map(|m| m.as_ref()),
    //         )
    //     }
    //     assert_eq!(
    //         summary(session.get_offer()),
    //         (
    //             clipboard,
    //             vec!["text/html", "text/plain;charset=utf-8"],
    //             Some("text/plain;charset=utf-8")
    //         )
    //     );
    //     assert_eq!(
    //         summary(session.get_primary_offer()),
    //         (primary, vec!["text/plain"], Some("text/plain"))
    //     );
    // }

    // #[test]
    // fn clearing_the_selection_destroys_the_previous_offer() {
    //     let (mut session, mut compositor) = session_with_device();
    //     let offer = 0xff00_0000;
    //     let mut events = device_event(WlDataControlDeviceEvents::DataOffer, offer);
    //     events.extend(device_event(WlDataControlDeviceEvents::Selection, offer));
    //     events.extend(device_event(WlDataControlDeviceEvents::Selection, 0));
    //     send_then_eof(&mut compositor, &events);

    //     dispatch_to_eof(&mut session);
    //     assert!(session.get_offer().is_none());
    //     assert_eq!(destroyed_offers(session, compositor), [offer]);
    // }

    // #[test]
    // fn selection_churn_destroys_each_previous_offer_and_never_runs_out_of_slots() {
    //     let (mut session, mut compositor) = session_with_device();
    //     // The compositor reuses an id once it is destroyed, so two ids are enough.
    //     let ids = [0xff00_0000u32, 0xff00_0001];
    //     // More rounds than the router (16) and the tracker (8) have slots.
    //     let rounds = 40;
    //     let mut events = Vec::new();
    //     for round in 0..rounds {
    //         let id = ids[round % 2];
    //         events.extend(device_event(WlDataControlDeviceEvents::DataOffer, id));
    //         events.extend(mime_offer(id, "text/plain"));
    //         events.extend(device_event(
    //             WlDataControlDeviceEvents::PrimarySelection,
    //             id,
    //         ));
    //     }
    //     send_then_eof(&mut compositor, &events);

    //     dispatch_to_eof(&mut session);
    //     let expected: Vec<u32> = (0..rounds - 1).map(|round| ids[round % 2]).collect();
    //     assert_eq!(destroyed_offers(session, compositor), expected);
    // }
}
