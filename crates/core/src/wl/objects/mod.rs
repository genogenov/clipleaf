use std::fmt::Debug;

pub mod wl_data_control_device;
pub mod wl_data_managers;
pub mod wl_data_offer;
pub mod wl_data_source;
pub mod wl_display;
pub mod wl_registry;

pub trait WlObject {
    type Ops: Into<u16>;
    type Events;
}

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

pub trait WlGlobal: WlObject {
    const NAME: WlStr;
    const MAX_VERSION: u32;
    const INTERFACE: WlInterface;
}

pub enum NoEvents {}

#[derive(Debug)]
pub struct MessageHeader {
    pub object_id: u32,
    pub opcode: u16,
    pub size: u16,
}

impl MessageHeader {
    pub const WL_HEADER_SIZE: u16 = 8; // object ID word + (size << 16 | opcode) word

    pub fn parse(h: &[u8; 8]) -> Self {
        let word = u32::from_ne_bytes([h[4], h[5], h[6], h[7]]);
        Self {
            object_id: u32::from_ne_bytes([h[0], h[1], h[2], h[3]]),
            opcode: word as u16,
            size: (word >> 16) as u16,
        }
    }
}

macro_rules! wl_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($variant:ident = $val:expr),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[repr(u16)]
        $vis enum $name {
            $($variant = $val),*
        }

        impl From<$name> for u16 {
            fn from(v: $name) -> u16 {
                v as u16
            }
        }
    };
}

pub(crate) use wl_enum;

#[repr(u16)]
pub enum WlCallbackEvents {
    Done = 0,
}

pub struct WlStr {
    pub bytes: &'static [u8],
    pub str: &'static str,
    pub wl_bytes: &'static [u8],
}

impl Debug for WlStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WlStr({})", self.str)
    }
}

macro_rules! wl_str_bytes {
    ($s:expr) => {{
        const S: &str = $s;
        const LEN: usize = 4 + S.len() + 1; // 4 bytes for length prefix, string bytes, and null terminator
        const RESULT: [u8; LEN] = {
            let size = ((S.len() + 1) as u32).to_ne_bytes();
            let b = S.as_bytes();
            let mut r = [0u8; LEN];
            r[0] = size[0];
            r[1] = size[1];
            r[2] = size[2];
            r[3] = size[3];
            r[4 + S.len()] = 0; // null terminator
            let mut i = 0;
            while i < b.len() {
                r[i + 4] = b[i];
                i += 1;
            }
            r
        };
        WlStr {
            str: S,
            bytes: S.as_bytes(),
            wl_bytes: &RESULT,
        }
    }};
}
pub(crate) use wl_str_bytes;
