use std::fmt::Debug;
use std::marker::PhantomData;

use crate::wl::objects::wl_data_managers::{
    DataControlManager, DataDeviceManager, ExtDataControlManagerV1, WlDataDeviceManager,
    ZwlrDataControlManager, ZwlrDataControlManagerV1,
};
use crate::wl::objects::{MessageHeader, WlObject, WlStr, wl_enum, wl_str_bytes};
use crate::wl::objects::{NoEvents, WlGlobal};
use crate::wl::wl_message_reader::WlMessageReader;
use crate::wl::wl_message_router::{WlInterface, WlMessageRouter};
use crate::wl::wl_message_writer::WlMessageWriter;

#[derive(Debug, Clone, Copy)]
pub struct RegistryInterface<I>
where
    I: WlObject,
{
    pub global_name: u32,
    pub version: u32,
    pub interface_name: &'static WlStr,
    pub interface: WlInterface,
    _marker: PhantomData<I>,
}

pub struct BoundInterface<I>
where
    I: WlGlobal,
{
    pub local_id: u32,
    pub version: u32,
    marker: PhantomData<I>,
}

impl<I: WlGlobal> BoundInterface<I> {
    pub fn new(local_id: u32, version: u32) -> Self {
        Self {
            local_id,
            version,
            marker: PhantomData,
        }
    }
}

impl<I: WlGlobal> WlObject for BoundInterface<I> {
    type Ops = I::Ops;
    type Events = I::Events;
}

#[derive(Debug, Clone, Copy)]
pub struct WlSeat;
impl WlObject for WlSeat {
    type Ops = WlSeatOps;
    type Events = NoEvents;
}
impl WlGlobal for WlSeat {
    const MAX_VERSION: u32 = 1;
}
wl_enum! {
    pub enum WlSeatOps {
        GetPointer = 0,
        GetKeyboard = 1,
        GetTouch = 2,
        Release = 3,
    }
}

wl_enum! {
    pub enum RegistryOps {
        Bind = 0,
    }
}

#[repr(u16)]
pub enum RegistryEvents {
    Global = 0,
}

pub struct WlRegistry {
    type_id: u32,
    pub data_device_manager: Option<DataDeviceManager>,
    pub ext_data_control_manager: Option<DataControlManager>,
    pub wl_seat: Option<RegistryInterface<WlSeat>>,
    pub zwlr_data_control_manager: Option<ZwlrDataControlManager>,
}

impl Debug for WlRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WlRegistry")
            .field("type_id", &self.type_id)
            .field("data_device_manager", &self.data_device_manager)
            .field("ext_data_control_manager", &self.ext_data_control_manager)
            .field("wl_seat", &self.wl_seat)
            .field("zwlr_data_control_manager", &self.zwlr_data_control_manager)
            .finish()
    }
}

impl WlRegistry {
    pub fn new(id: u32) -> Self {
        Self {
            type_id: id,
            data_device_manager: None,
            zwlr_data_control_manager: None,
            ext_data_control_manager: None,
            wl_seat: None,
        }
    }

    pub fn bind<I>(
        &self,
        writer: WlMessageWriter,
        router: &mut WlMessageRouter,
        interface: RegistryInterface<I>,
    ) -> std::io::Result<BoundInterface<I>>
    where
        I: WlGlobal,
    {
        let mut msg = writer.begin_message::<WlRegistry>(RegistryOps::Bind, self.type_id)?;
        let negotiated_version = interface.version.min(I::MAX_VERSION);
        msg.pack_u32(interface.global_name)?;
        msg.pack_wl_str(interface.interface_name)?;
        msg.pack_u32(negotiated_version)?;
        let binding_id = router.register_client(interface.interface)?;
        msg.pack_new_object_id(&binding_id)?;
        msg.end()?;
        Ok(BoundInterface::<I>::new(
            binding_id.commit(),
            negotiated_version,
        ))
    }

    pub fn add_interface(
        &mut self,
        header: &MessageHeader,
        reader: &mut WlMessageReader,
    ) -> Option<()> {
        if header.object_id == self.type_id && header.opcode == RegistryEvents::Global as u16 {
            let global_name = reader.u32()?;
            let interface_name_slice = reader.str()?;
            let version = reader.u32()?;

            match interface_name_slice {
                val if val == WlRegistry::WL_DATA_DEVICE_MANAGER.bytes => {
                    self.data_device_manager = Some(RegistryInterface::<WlDataDeviceManager> {
                        global_name,
                        version,
                        interface_name: &WlRegistry::WL_DATA_DEVICE_MANAGER,
                        interface: WlInterface::DataDeviceManager,
                        _marker: PhantomData,
                    });
                    return Some(());
                }
                val if val == WlRegistry::ZWLR_DATA_CONTROL_MANAGER_V1.bytes => {
                    self.zwlr_data_control_manager =
                        Some(RegistryInterface::<ZwlrDataControlManagerV1> {
                            global_name,
                            version,
                            interface: WlInterface::ZwlrDataControlManager,
                            interface_name: &WlRegistry::ZWLR_DATA_CONTROL_MANAGER_V1,
                            _marker: PhantomData,
                        });
                    return Some(());
                }
                val if val == WlRegistry::EXT_DATA_CONTROL_MANAGER_V1.bytes => {
                    self.ext_data_control_manager =
                        Some(RegistryInterface::<ExtDataControlManagerV1> {
                            global_name,
                            version,
                            interface_name: &WlRegistry::EXT_DATA_CONTROL_MANAGER_V1,
                            interface: WlInterface::ExtDataControlManager,
                            _marker: PhantomData,
                        });
                    return Some(());
                }
                val if val == WlRegistry::WL_SEAT.bytes => {
                    self.wl_seat = Some(RegistryInterface::<WlSeat> {
                        global_name,
                        version,
                        interface: WlInterface::Seat,
                        interface_name: &WlRegistry::WL_SEAT,
                        _marker: PhantomData,
                    });
                    return Some(());
                }
                // Add more interfaces here as needed
                _ => return None,
            };
        }

        None
    }
}

impl WlRegistry {
    const WL_DATA_DEVICE_MANAGER: WlStr = wl_str_bytes!("wl_data_device_manager");
    const ZWLR_DATA_CONTROL_MANAGER_V1: WlStr = wl_str_bytes!("zwlr_data_control_manager_v1");
    const EXT_DATA_CONTROL_MANAGER_V1: WlStr = wl_str_bytes!("ext_data_control_manager_v1");
    const WL_SEAT: WlStr = wl_str_bytes!("wl_seat");
}

impl WlObject for WlRegistry {
    type Ops = RegistryOps;
    type Events = RegistryEvents;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{unix_fd_stream::ReadMode, wl::wl_buffered_stream::WlBufferedStream};
    use std::os::unix::net::UnixStream;

    // Binds over a socket pair and returns the version field the compositor would receive.
    fn bound_version<I: WlGlobal>(interface: RegistryInterface<I>) -> u32 {
        let (a, b) = UnixStream::pair().unwrap();
        let mut client = WlBufferedStream::new(a.into());
        let mut compositor = WlBufferedStream::new(b.into());
        let mut router = WlMessageRouter::new();
        let bound = WlRegistry::new(2)
            .bind(client.get_writer(), &mut router, interface)
            .unwrap();
        client.write().unwrap();

        let (_, body, _) = compositor.read_next_message(ReadMode::Block).unwrap();
        let mut r = WlMessageReader::new(body);
        r.u32().unwrap(); // global name
        r.str().unwrap(); // interface name
        let version = r.u32().unwrap();
        assert_eq!(version, bound.version);
        version
    }

    #[test]
    fn bind_uses_the_lower_of_server_and_implemented_version() {
        let seat = |version| RegistryInterface::<WlSeat> {
            global_name: 7,
            version,
            interface_name: &WlRegistry::WL_SEAT,
            interface: WlInterface::Seat,
            _marker: PhantomData,
        };
        let zwlr = |version| RegistryInterface::<ZwlrDataControlManagerV1> {
            global_name: 8,
            version,
            interface_name: &WlRegistry::ZWLR_DATA_CONTROL_MANAGER_V1,
            interface: WlInterface::ZwlrDataControlManager,
            _marker: PhantomData,
        };

        assert_eq!(bound_version(seat(9)), 1);
        assert_eq!(bound_version(zwlr(1)), 1);
        assert_eq!(bound_version(zwlr(2)), 2);
        assert_eq!(bound_version(zwlr(5)), 2);
    }
}
