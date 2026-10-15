use std::fmt::Debug;
use std::marker::PhantomData;

use crate::wl::objects::wl_data_managers::{ExtDataControlManagerV1, ZwlrDataControlManagerV1};
use crate::wl::objects::{MessageHeader, WlInterface, WlObject, WlStr, wl_enum, wl_str_bytes};
use crate::wl::objects::{NoEvents, WlGlobal};
use crate::wl::wl_message_reader::WlMessageReader;
use crate::wl::wl_message_router::WlMessageRouter;
use crate::wl::wl_message_writer::WlMessageWriter;

struct Known {
    name: &'static WlStr,
}
const fn known<I: WlGlobal>() -> Known {
    Known { name: &I::NAME }
}
const KNOWN: [Known; 3] = [
    known::<WlSeat>(),
    known::<ExtDataControlManagerV1>(),
    known::<ZwlrDataControlManagerV1>(),
];

#[derive(Debug, Clone, Copy)]
pub struct RegistryInterface<I>
where
    I: WlObject,
{
    pub global_name: u32,
    pub version: u32,
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
    const NAME: WlStr = wl_str_bytes!("wl_seat");
    const INTERFACE: WlInterface = WlInterface::Seat;
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

    GlobalRemove = 1,
}

pub struct WlRegistry {
    type_id: u32,
    found: [Option<(u32, u32)>; KNOWN.len()],
}

impl Debug for WlRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entry(&"type_id", &self.type_id)
            .entries(KNOWN.iter().zip(&self.found).map(|(k, a)| (k.name.str, a)))
            .finish()
    }
}

impl WlRegistry {
    pub fn new(id: u32) -> Self {
        Self {
            type_id: id,
            found: [None; KNOWN.len()],
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
        msg.pack_wl_str(&I::NAME)?;
        msg.pack_u32(negotiated_version)?;
        let binding_id = router.register_client(I::INTERFACE)?;
        msg.pack_new_object_id(&binding_id)?;
        msg.end()?;
        Ok(BoundInterface::<I>::new(
            binding_id.commit(),
            negotiated_version,
        ))
    }

    pub fn get<I: WlGlobal>(&self) -> Option<RegistryInterface<I>> {
        let known_idx = KNOWN.iter().position(|f| f.name.bytes == I::NAME.bytes)?;

        self.found[known_idx].map(|(global_name, version)| RegistryInterface::<I> {
            global_name,
            version,
            _marker: PhantomData,
        })
    }

    pub fn handle_event(
        &mut self,
        header: &MessageHeader,
        reader: &mut WlMessageReader,
    ) -> std::io::Result<()> {
        if header.opcode == RegistryEvents::Global as u16 {
            let global_name = reader.u32().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Failed to read global name",
                )
            })?;
            let interface_name_slice = reader.str().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Failed to read interface name",
                )
            })?;
            let version = reader.u32().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Failed to read version")
            })?;

            if let Some(known_idx) = KNOWN
                .iter()
                .position(|k| k.name.bytes == interface_name_slice)
            {
                self.found[known_idx] = Some((global_name, version));
            };
            return Ok(());
        } else if header.opcode == RegistryEvents::GlobalRemove as u16 {
            return Ok(());
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Unknown opcode: {}", header.opcode),
        ))
    }
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

    // Binds over a socket pair and returns the (global name, interface name, version) the
    // compositor would receive.
    fn bind_request<I: WlGlobal>(global_name: u32, version: u32) -> (u32, Vec<u8>, u32) {
        let (a, b) = UnixStream::pair().unwrap();
        let mut client = WlBufferedStream::new(a.into());
        let mut compositor = WlBufferedStream::new(b.into());
        let mut router = WlMessageRouter::new();
        let advertised = RegistryInterface::<I> {
            global_name,
            version,
            _marker: PhantomData,
        };
        let bound = WlRegistry::new(2)
            .bind(client.get_writer(), &mut router, advertised)
            .unwrap();
        client.write().unwrap();

        let (_, body, _) = compositor.read_next_message(ReadMode::Block).unwrap();
        let mut r = WlMessageReader::new(body);
        let sent_global = r.u32().unwrap();
        let sent_name = r.str().unwrap().to_vec();
        let sent_version = r.u32().unwrap();
        assert_eq!(sent_version, bound.version);
        (sent_global, sent_name, sent_version)
    }

    #[test]
    fn bind_sends_the_global_and_name_and_the_lower_of_server_and_implemented_version() {
        assert_eq!(bind_request::<WlSeat>(7, 9), (7, b"wl_seat".to_vec(), 1));
        assert_eq!(
            bind_request::<ExtDataControlManagerV1>(54, 1),
            (54, b"ext_data_control_manager_v1".to_vec(), 1)
        );
        let zwlr = |version| bind_request::<ZwlrDataControlManagerV1>(8, version).2;
        assert_eq!(zwlr(1), 1);
        assert_eq!(zwlr(2), 2);
        assert_eq!(zwlr(5), 2);
    }
}
