use crate::{log_debug, wl::objects::WlInterface};
use std::io::{self, Error, Result};

const SERVER_ID_START: u32 = 0xff000000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Free,
    Live(WlInterface),
    Reserved,
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

    pub fn lookup_interface(&self, object_id: u32) -> io::Result<Option<WlInterface>> {
        match self.lookup_slot(object_id) {
            Some(interface) => Ok(Some(interface)),
            None if object_id >= SERVER_ID_START => {
                log_debug!("Ignoring compositor-created object with id {}", object_id);
                Ok(None)
            }
            None => Err(Error::other(format!(
                "event for unknown client object with id {}",
                object_id
            ))),
        }
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
        if object_id > 1
            && let Some(slot) = self.client_interfaces.get_mut(object_id as usize)
        {
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
    fn lookup_returns_the_interface_of_live_client_and_server_objects() {
        let mut router = WlMessageRouter::new();
        let device = router
            .register_client(WlInterface::DataControlDevice)
            .unwrap()
            .commit();
        let offer = router
            .register_server_object(WlInterface::DataControlOffer, SERVER_ID_START + 1)
            .unwrap();

        assert_eq!(
            router.lookup_interface(1).unwrap(),
            Some(WlInterface::Display)
        );
        assert_eq!(
            router.lookup_interface(device).unwrap(),
            Some(WlInterface::DataControlDevice)
        );
        assert_eq!(
            router.lookup_interface(offer).unwrap(),
            Some(WlInterface::DataControlOffer)
        );
    }

    #[test]
    fn freed_or_unknown_server_objects_are_skipped_but_unknown_client_objects_are_errors() {
        let mut router = WlMessageRouter::new();
        let offer = router
            .register_server_object(WlInterface::DataControlOffer, SERVER_ID_START)
            .unwrap();
        router.free_server(offer).unwrap();

        // An offer we destroyed can still have events in flight: not an error.
        assert_eq!(router.lookup_interface(offer).unwrap(), None);
        // Never registered, and past the end of the table: also just skipped.
        assert_eq!(router.lookup_interface(SERVER_ID_START + 5).unwrap(), None);
        assert_eq!(router.lookup_interface(u32::MAX).unwrap(), None);

        // A client id that is free, reserved, or beyond the table means our bookkeeping is wrong.
        for id in [0, 9, MAX_REGISTERED_INTERFACES as u32, SERVER_ID_START - 1] {
            let err = router.lookup_interface(id).unwrap_err();
            assert!(
                err.to_string().contains("unknown client object"),
                "{id}: {err}"
            );
        }
    }

    #[test]
    fn a_freed_client_object_is_unknown_again() {
        let mut router = WlMessageRouter::new();
        let callback = router
            .register_client(WlInterface::Callback)
            .unwrap()
            .commit();
        assert!(router.lookup_interface(callback).unwrap().is_some());
        router.free_client(callback);
        assert!(router.lookup_interface(callback).is_err());
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
