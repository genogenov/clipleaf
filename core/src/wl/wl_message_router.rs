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
}

const MAX_CLIENT_INTERFACES: usize = 16;
const MAX_SERVER_OBJECTS: usize = 1 << 16;

pub struct WlMessageRouter {
    client_interfaces: [Slot; MAX_CLIENT_INTERFACES],
    server_interfaces: Vec<Slot>,
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
            client_interfaces: [Slot::Free; MAX_CLIENT_INTERFACES],
            server_interfaces: vec![Slot::Free; 16],
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
        let index = id
            .checked_sub(SERVER_ID_START)
            .ok_or_else(|| Error::other(format!("Invalid server slot id {}", id)))?
            as usize;
        if index >= MAX_SERVER_OBJECTS {
            return Err(Error::other(format!(
                "Invalid server slot id {} exceeds the max server ID allowed",
                id
            )));
        }
        if self.server_interfaces.len() <= index {
            self.server_interfaces.resize(index + 1, Slot::Free);
        }
        let slot = &mut self.server_interfaces[index];
        {
            // Server objects are only destroyed by us, no delete_id comes, so reusing taken ID would mean a misbehaving compositor.
            if let Slot::Live(_) = *slot {
                return Err(Error::other(format!(
                    "The requested server slod id {} is already taken",
                    index
                )));
            }
            *slot = Slot::Live(interface);
            log_debug!("Registered interface {:?} at server slot {}", interface, id);
            Ok(id)
        }
    }

    pub fn mark_destroyed(&mut self, object_id: u32) -> Result<()> {
        let slot = self.get_slot_mut(object_id)?;
        let Slot::Live(interface) = *slot else {
            return Err(Error::other(format!(
                "Invalid slot id {} for marking destroyed",
                object_id
            )));
        };
        {
            *slot = Slot::Destroyed(interface);
            log_debug!(
                "Marked interface {:?} at client slot {} as destroyed",
                interface,
                object_id
            );
            Ok(())
        }
    }

    pub fn lookup_interface(&self, object_id: u32) -> io::Result<Route> {
        match self.get_slot(object_id)? {
            Slot::Live(interface) => Ok(Route::Live(*interface)),
            Slot::Destroyed(interface) => {
                log_debug!(
                    "Ignoring event for destroyed client object with id {} and interface {:?}",
                    object_id,
                    interface
                );
                Ok(Route::Destroyed(*interface))
            }
            Slot::Free if object_id >= SERVER_ID_START => Err(Error::other(format!(
                "Event for unanounced server object with id {}",
                object_id
            ))),
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

    fn get_slot(&self, object_id: u32) -> Result<&Slot> {
        let slot = if object_id >= SERVER_ID_START {
            self.server_interfaces
                .get((object_id - SERVER_ID_START) as usize)
        } else {
            self.client_interfaces.get(object_id as usize)
        };
        slot.ok_or_else(|| Error::other(format!("object with id {} out of bounds", object_id)))
    }

    fn get_slot_mut(&mut self, object_id: u32) -> Result<&mut Slot> {
        let slot = if object_id >= SERVER_ID_START {
            self.server_interfaces
                .get_mut((object_id - SERVER_ID_START) as usize)
        } else {
            self.client_interfaces.get_mut(object_id as usize)
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

    fn lookup_error(router: &mut WlMessageRouter, id: u32) -> String {
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
        assert!(lookup_error(&mut router, 2).contains("Unknown client object"));
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
    fn a_freed_client_object_is_unknown_again() {
        let mut router = WlMessageRouter::new();
        let callback = router
            .register_client(WlInterface::Callback)
            .unwrap()
            .commit();
        assert!(router.lookup_interface(callback).is_ok());
        router.free_client(callback);
        assert!(lookup_error(&mut router, callback).contains("Unknown client object"));
    }

    #[test]
    fn a_destroyed_client_object_routes_as_destroyed_until_it_is_freed() {
        let mut router = WlMessageRouter::new();
        let source = router
            .register_client(WlInterface::DataControlSource)
            .unwrap()
            .commit();
        router.mark_destroyed(source).unwrap();
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
        assert!(lookup_error(&mut router, source).contains("Unknown client object"));
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
        router.mark_destroyed(source).unwrap();
        // Free, reserved, already destroyed, and outside the table.
        for id in [9, 0, source, 99] {
            assert!(router.mark_destroyed(id).is_err(), "{id}");
        }
    }
}
