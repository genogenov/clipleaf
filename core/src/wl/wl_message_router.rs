use crate::{
    fd_io::InFdBuffer,
    log_debug,
    wl::{
        objects::{
            MessageHeader, wl_data_control_device::DataControlDeviceEvent,
            wl_data_offer::DataControlOfferEvent, wl_data_source::WlDataControlSourceEvent,
        },
        wl_buffered_stream::WlStreamReader,
    },
};
use std::{
    io::{Error, Result},
    ops::ControlFlow,
};

const SERVER_ID_START: u32 = 0xff000000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WlInterface {
    Display,
    Callback,
    Registry,
    Seat,
    ExtDataControlManager,
    ZwlrDataControlManager,
    DataDeviceManager,
    DataControlDevice,
    DataControlSource,
    DataControlOffer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Free,
    Live(WlInterface),
    Reserved,
}

pub enum WlEvent<'a> {
    DataControlSource {
        id: u32,
        event: WlDataControlSourceEvent<'a>,
    },

    DataControlDevice(DataControlDeviceEvent),

    DataControlOffer {
        id: u32,
        event: DataControlOfferEvent<'a>,
    },

    SyncDone,

    Ignored,
}

const MAX_REGISTERED_INTERFACES: usize = 16;

pub struct WlMessageRouter {
    client_interfaces: [Slot; MAX_REGISTERED_INTERFACES],
    server_interfaces: [Slot; MAX_REGISTERED_INTERFACES],
}

impl Default for WlMessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct WlPendingId<'a> {
    id: u32,
    slot: &'a mut Slot,
}

impl<'a> WlPendingId<'a> {
    pub(crate) fn new(id: u32, slot: &'a mut Slot) -> Self {
        Self { id, slot }
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn commit(self) -> u32 {
        let id = self.id;
        std::mem::forget(self);
        id
    }
}

impl<'a> Drop for WlPendingId<'a> {
    fn drop(&mut self) {
        *self.slot = Slot::Free;
    }
}

pub trait WlMessageHandler: FnMut(WlEvent<'_>) -> ControlFlow<Result<()>> {}

impl<T> WlMessageHandler for T where T: FnMut(WlEvent<'_>) -> ControlFlow<Result<()>> {}

pub enum WlEventReadResult<'a> {
    Ignored,
    Event(WlInterface, MessageHeader, &'a [u8], &'a mut InFdBuffer),
    Error(Error),
}

impl WlMessageRouter {
    pub fn new() -> Self {
        let mut instance = Self {
            client_interfaces: [Slot::Free; MAX_REGISTERED_INTERFACES],
            server_interfaces: [Slot::Free; MAX_REGISTERED_INTERFACES],
        };
        instance.client_interfaces[0] = Slot::Reserved;
        instance.client_interfaces[1] = Slot::Live(WlInterface::Display);
        instance
    }

    pub fn register_client(&mut self, interface: WlInterface) -> Result<WlPendingId<'_>> {
        if let Some(free_index) = self.find_free_client() {
            let slot = &mut self.client_interfaces[free_index];
            *slot = Slot::Live(interface);
            log_debug!(
                "Registered interface {:?} at client slot {}",
                interface,
                free_index
            );
            Ok(WlPendingId::new(free_index as u32, slot))
        } else {
            Err(Error::other("No free client slot available"))
        }
    }

    pub fn register_server_object(&mut self, interface: WlInterface, id: u32) -> Result<u32> {
        if let Some(slot) = self.server_interfaces.get_mut(
            id.checked_sub(SERVER_ID_START)
                .ok_or_else(|| Error::other(format!("Invalid server slot id {}", id)))?
                as usize,
        ) {
            *slot = Slot::Live(interface);
            log_debug!("Registered interface {:?} at server slot {}", interface, id);
            Ok(id)
        } else {
            Err(Error::other(format!(
                "Invalid server slot id {}(local array id: {})",
                id,
                (id - SERVER_ID_START)
            )))
        }
    }

    pub fn next_event<'a>(
        &mut self,
        reader: &'a mut WlStreamReader<'_>,
    ) -> std::io::Result<Option<WlEventReadResult<'a>>> {
        let Some((header, buffer, fds)) = reader.read_next_message()? else {
            return Ok(None);
        };
        // log_debug!("Received message with header: {:?}", header);
        let result = match self.lookup_slot(header.object_id) {
            Some(interface) => WlEventReadResult::Event(interface, header, buffer, fds),
            None if header.object_id >= 0xff00_0000 => {
                log_debug!(
                    "Ignoring compositor-created object with id {}",
                    header.object_id
                );
                WlEventReadResult::Ignored
            }
            None => WlEventReadResult::Error(Error::other("No slot found")),
        };
        Ok(Some(result))
    }

    pub fn free_server(&mut self, object_id: u32) -> Result<()> {
        if let Some(slot) = self.server_interfaces.get_mut(
            object_id
                .checked_sub(SERVER_ID_START)
                .ok_or_else(|| Error::other(format!("Invalid server slot id {}", object_id)))?
                as usize,
        ) {
            *slot = Slot::Free;
        }
        Ok(())
    }

    pub fn free_client(&mut self, object_id: u32) -> Option<()> {
        if let Some(slot) = self.client_interfaces.get_mut(object_id as usize) {
            *slot = Slot::Free;
            return Some(());
        }
        None
    }

    fn find_free_client(&self) -> Option<usize> {
        self.client_interfaces
            .iter()
            .position(|slot| *slot == Slot::Free)
    }

    fn lookup_slot(&self, object_id: u32) -> Option<WlInterface> {
        let slot = if object_id >= SERVER_ID_START {
            self.server_interfaces
                .get((object_id - SERVER_ID_START) as usize)
        } else {
            self.client_interfaces.get(object_id as usize)
        };
        match slot {
            Some(Slot::Live(interface)) => Some(*interface),
            Some(Slot::Free) => None,
            Some(Slot::Reserved) => None,
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wl::wl_buffered_stream::WlBufferedStream;
    use std::{io::Write, os::unix::net::UnixStream};

    fn msg(object_id: u32, opcode: u16, body: &[u8]) -> Vec<u8> {
        let mut m = Vec::with_capacity(8 + body.len());
        m.extend_from_slice(&object_id.to_ne_bytes());
        m.extend_from_slice(&(((8 + body.len()) as u32) << 16 | u32::from(opcode)).to_ne_bytes());
        m.extend_from_slice(body);
        m
    }

    // The peer end plays the compositor.
    fn setup() -> (WlMessageRouter, WlBufferedStream, UnixStream) {
        let (a, peer) = UnixStream::pair().unwrap();
        (
            WlMessageRouter::new(),
            WlBufferedStream::new(a.into()),
            peer,
        )
    }

    #[test]
    fn ids_are_dense_and_reused_after_free_client() {
        let mut router = WlMessageRouter::new();
        for (interface, id) in [
            (WlInterface::Registry, 2),
            (WlInterface::Callback, 3),
            (WlInterface::Seat, 4),
        ] {
            assert_eq!(router.register_client(interface).unwrap().commit(), id);
        }
        router.free_client(3);
        assert_eq!(
            router
                .register_client(WlInterface::DataControlSource)
                .unwrap()
                .commit(),
            3
        );
        assert_eq!(
            router
                .register_client(WlInterface::DataControlDevice)
                .unwrap()
                .commit(),
            5
        );
    }

    #[test]
    fn a_dropped_pending_id_frees_its_slot() {
        let mut router = WlMessageRouter::new();
        let pending = router.register_client(WlInterface::Registry).unwrap();
        assert_eq!(pending.id(), 2);
        drop(pending);
        assert_eq!(router.lookup_slot(2), None);
        assert_eq!(
            router.register_client(WlInterface::Seat).unwrap().commit(),
            2
        );
        assert_eq!(router.lookup_slot(2), Some(WlInterface::Seat));
    }

    #[test]
    fn next_event_returns_the_interface_header_and_body_of_registered_objects() {
        let (mut router, mut stream, mut peer) = setup();
        let (mut reader, _) = stream.split();
        let device = router
            .register_client(WlInterface::DataControlDevice)
            .unwrap()
            .commit();
        let offer = router
            .register_server_object(WlInterface::DataControlOffer, 0xff00_0001)
            .unwrap();

        let mut bytes = msg(device, 1, &7u32.to_ne_bytes());
        bytes.extend(msg(offer, 0, &[]));
        peer.write_all(&bytes).unwrap();

        assert!(matches!(
            router.next_event(&mut reader).unwrap(),
            Some(WlEventReadResult::Event(WlInterface::DataControlDevice, h, body, _))
                if h.object_id == device && h.opcode == 1 && body == 7u32.to_ne_bytes()
        ));
        assert!(matches!(
            router.next_event(&mut reader).unwrap(),
            Some(WlEventReadResult::Event(WlInterface::DataControlOffer, h, body, _))
                if h.object_id == offer && body.is_empty()
        ));

        drop(peer);
        assert!(router.next_event(&mut reader).unwrap().is_none(), "EOF");
    }

    #[test]
    fn freed_or_unknown_server_objects_are_ignored_but_unknown_client_objects_are_errors() {
        let (mut router, mut stream, mut peer) = setup();
        let (mut reader, _) = stream.split();
        let offer = router
            .register_server_object(WlInterface::DataControlOffer, 0xff00_0000)
            .unwrap();
        router.free_server(offer).unwrap();

        let mut bytes = msg(offer, 0, &[]);
        bytes.extend(msg(0xff00_0005, 0, &[]));
        bytes.extend(msg(9, 0, &[]));
        peer.write_all(&bytes).unwrap();

        for _ in 0..2 {
            assert!(matches!(
                router.next_event(&mut reader).unwrap(),
                Some(WlEventReadResult::Ignored)
            ));
        }
        assert!(matches!(
            router.next_event(&mut reader).unwrap(),
            Some(WlEventReadResult::Error(_))
        ));
    }

    #[test]
    fn server_ids_outside_the_table_are_rejected() {
        let mut router = WlMessageRouter::new();
        let past_the_end = SERVER_ID_START + MAX_REGISTERED_INTERFACES as u32;
        assert!(
            router
                .register_server_object(WlInterface::DataControlOffer, past_the_end)
                .is_err()
        );
        assert!(
            router
                .register_server_object(WlInterface::DataControlOffer, 5)
                .is_err()
        );
        assert!(router.free_server(5).is_err());
    }
}
