use crate::{
    WlMessageRouter,
    wl::{
        objects::{
            NoEvents, WlGlobal, WlInterface, WlObject, WlStr, wl_data_control_device::WlDataControlDevice, wl_data_source::WlDataControlSource,
            wl_enum, wl_registry::BoundInterface, wl_str_bytes,
        },
        wl_message_writer::WlMessageWriter,
    },
};

pub trait DataDeviceManagerExt {
    fn manager_id(&self) -> u32;

    fn get_data_device(&self, writer: WlMessageWriter, router: &mut WlMessageRouter, seat_id: u32) -> std::io::Result<WlDataControlDevice> {
        let mut msg = writer.begin_message::<WlDataDeviceManager>(WlDataDeviceManagerOps::GetDataDevice, self.manager_id())?;
        let data_device_id = router.register_client(WlInterface::DataControlDevice)?;
        msg.pack_new_object_id(&data_device_id)?;
        msg.pack_u32(seat_id)?;
        msg.end()?;
        let id = data_device_id.commit();
        Ok(WlDataControlDevice { local_id: id })
    }

    fn create_data_source(&self, writer: WlMessageWriter, router: &mut WlMessageRouter) -> std::io::Result<WlDataControlSource> {
        let mut msg = writer.begin_message::<WlDataDeviceManager>(WlDataDeviceManagerOps::CreateDataSource, self.manager_id())?;
        let data_source_id = router.register_client(WlInterface::DataControlSource)?;

        msg.pack_new_object_id(&data_source_id)?;
        msg.end()?;

        Ok(WlDataControlSource {
            local_id: data_source_id.commit(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExtDataControlManagerV1;
impl WlObject for ExtDataControlManagerV1 {
    type Ops = ExtDataControlManagerOps;
    type Events = NoEvents;
}

impl WlGlobal for ExtDataControlManagerV1 {
    const MAX_VERSION: u32 = 1;
    const NAME: WlStr = wl_str_bytes!("ext_data_control_manager_v1");
    const INTERFACE: WlInterface = WlInterface::ExtDataControlManager;
}

wl_enum! {
    pub enum ExtDataControlManagerOps {
        CreateDataSource = 0,
        GetDataDevice = 1,
        Destroy = 2,
    }
}

#[derive(Debug)]
pub struct ZwlrDataControlManagerV1;
impl WlObject for ZwlrDataControlManagerV1 {
    type Ops = ZwlrDataControlManagerOps;
    type Events = NoEvents;
}

impl WlGlobal for ZwlrDataControlManagerV1 {
    const MAX_VERSION: u32 = 2;
    const NAME: WlStr = wl_str_bytes!("zwlr_data_control_manager_v1");
    const INTERFACE: WlInterface = WlInterface::ZwlrDataControlManager;
}
wl_enum! {
    pub enum ZwlrDataControlManagerOps {
        CreateDataSource = 0,
        GetDataDevice = 1,
        Destroy = 2,
    }
}

#[derive(Debug)]
pub struct WlDataDeviceManager;
impl WlObject for WlDataDeviceManager {
    type Ops = WlDataDeviceManagerOps;
    type Events = WlDataDeviceManagerEvents;
}
wl_enum! {
    pub enum WlDataDeviceManagerOps {
        CreateDataSource = 0,
        GetDataDevice = 1,
    }
}

// #[repr(u8)]
// pub enum DragAndDrop {
//     None = 0,
//     Copy = 1,
//     Move = 2,
//     Ask = 4,
// }
pub enum WlDataDeviceManagerEvents {
    // DragAndDrop(DragAndDrop),
}

impl DataDeviceManagerExt for BoundInterface<ZwlrDataControlManagerV1> {
    fn manager_id(&self) -> u32 {
        self.local_id
    }
}

impl DataDeviceManagerExt for BoundInterface<ExtDataControlManagerV1> {
    fn manager_id(&self) -> u32 {
        self.local_id
    }
}
