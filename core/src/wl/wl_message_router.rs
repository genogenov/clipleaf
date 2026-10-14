use crate::{log_debug, wl::objects::WlInterface};
use std::io::{self, Error, Result};

const SERVER_ID_START: u32 = 0xff000000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Free,
    Live(WlInterface),
    Destroyed(WlInterface),
    Reserved,
}

pub enum Route {
    Live(WlInterface),
    Destroyed(WlInterface),
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

    pub fn mark_destroyed_client(&mut self, object_id: u32) -> Result<()> {
        if let Some(slot) = self.client_interfaces.get_mut(object_id as usize)
            && let Slot::Live(interface) = *slot
        {
            *slot = Slot::Destroyed(interface);
            log_debug!(
                "Marked interface {:?} at client slot {} as destroyed",
                interface,
                object_id
            );
            return Ok(());
        }
        Err(Error::other(format!(
            "Invalid client slot id {} for marking destroyed",
            object_id
        )))
    }

    pub fn lookup_interface(&self, object_id: u32) -> io::Result<Route> {
        match self.lookup_slot(object_id)? {
            Slot::Live(interface) => Ok(Route::Live(*interface)),
            Slot::Destroyed(interface) => {
                log_debug!(
                    "Ignoring event for destroyed client object with id {} and interface {:?}",
                    object_id,
                    interface
                );
                Ok(Route::Destroyed(*interface))
            }
            Slot::Free if object_id >= SERVER_ID_START => {
                log_debug!("Ignoring compositor-created object with id {}", object_id);
                Ok(Route::Ignored)
            }
            Slot::Reserved => Err(Error::other(format!(
                "event for reserved client object with id {}",
                object_id
            ))),
            Slot::Free => Err(Error::other(format!(
                "Unknown client object with id {}",
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

    fn lookup_slot(&self, object_id: u32) -> Result<&Slot> {
        let slot = if object_id >= SERVER_ID_START {
            self.server_interfaces
                .get((object_id - SERVER_ID_START) as usize)
        } else {
            self.client_interfaces.get(object_id as usize)
        };
        slot.ok_or_else(|| Error::other(format!("object with id {} out of bounds", object_id)))
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

    fn lookup_error(router: &WlMessageRouter, id: u32) -> String {
        router
            .lookup_interface(id)
            .err()
            .expect("an error")
            .to_string()
    }

    #[test]
    fn a_dropped_pending_id_frees_its_slot() {
        let mut router = WlMessageRouter::new();
        let pending = router.register_client(WlInterface::Registry).unwrap();
        assert_eq!(pending.id(), 2);
        drop(pending);
        assert!(lookup_error(&router, 2).contains("Unknown client object"));
        assert_eq!(
            router.register_client(WlInterface::Seat).unwrap().commit(),
            2
        );
        assert!(matches!(
            router.lookup_interface(2),
            Ok(Route::Live(WlInterface::Seat))
        ));
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

        assert!(matches!(
            router.lookup_interface(1),
            Ok(Route::Live(WlInterface::Display))
        ));
        assert!(matches!(
            router.lookup_interface(device),
            Ok(Route::Live(WlInterface::DataControlDevice))
        ));
        assert!(matches!(
            router.lookup_interface(offer),
            Ok(Route::Live(WlInterface::DataControlOffer))
        ));
    }

    #[test]
    fn freed_server_objects_are_ignored_but_bad_client_ids_and_ids_past_the_tables_are_errors() {
        let mut router = WlMessageRouter::new();
        let offer = router
            .register_server_object(WlInterface::DataControlOffer, SERVER_ID_START)
            .unwrap();
        router.free_server(offer).unwrap();

        // An offer we destroyed can still have events in flight: not an error.
        assert!(matches!(router.lookup_interface(offer), Ok(Route::Ignored)));
        // Never registered, but inside the table: the same.
        assert!(matches!(
            router.lookup_interface(SERVER_ID_START + 5),
            Ok(Route::Ignored)
        ));

        // A client id that is free or reserved means our bookkeeping is wrong.
        assert!(lookup_error(&router, 9).contains("Unknown client object"));
        assert!(lookup_error(&router, 0).contains("reserved"));
        // Outside the tables on either side.
        for id in [
            MAX_REGISTERED_INTERFACES as u32,
            SERVER_ID_START - 1,
            SERVER_ID_START + MAX_REGISTERED_INTERFACES as u32,
            u32::MAX,
        ] {
            assert!(lookup_error(&router, id).contains("out of bounds"), "{id}");
        }
    }

    #[test]
    fn a_freed_client_object_is_unknown_again() {
        let mut router = WlMessageRouter::new();
        let callback = router
            .register_client(WlInterface::Callback)
            .unwrap()
            .commit();
        assert!(router.lookup_interface(callback).is_ok());
        router.free_client(callback);
        assert!(lookup_error(&router, callback).contains("Unknown client object"));
    }

    #[test]
    fn a_destroyed_client_object_routes_as_destroyed_until_it_is_freed() {
        let mut router = WlMessageRouter::new();
        let source = router
            .register_client(WlInterface::DataControlSource)
            .unwrap()
            .commit();
        router.mark_destroyed_client(source).unwrap();
        assert!(matches!(
            router.lookup_interface(source),
            Ok(Route::Destroyed(WlInterface::DataControlSource))
        ));
        // Not reusable yet: the compositor has not confirmed the delete.
        assert_ne!(
            router
                .register_client(WlInterface::DataControlSource)
                .unwrap()
                .commit(),
            source
        );

        router.free_client(source);
        assert!(lookup_error(&router, source).contains("Unknown client object"));
        assert_eq!(
            router
                .register_client(WlInterface::Callback)
                .unwrap()
                .commit(),
            source
        );
    }

    #[test]
    fn only_a_live_client_object_can_be_marked_destroyed() {
        let mut router = WlMessageRouter::new();
        let source = router
            .register_client(WlInterface::DataControlSource)
            .unwrap()
            .commit();
        router.mark_destroyed_client(source).unwrap();
        // Free, reserved, already destroyed, and outside the table.
        for id in [9, 0, source, 99] {
            assert!(router.mark_destroyed_client(id).is_err(), "{id}");
        }
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
