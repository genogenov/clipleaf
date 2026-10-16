#![warn(clippy::undocumented_unsafe_blocks)]
mod event_fd;
mod fd_io;
pub mod ffi;
#[doc(hidden)]
mod log_writer;
mod mime;
mod poll;
mod unix_fd_stream;
mod wl;

pub use wl::{
    objects::{
        wl_data_control_device::{DataControlDeviceEvent, WlDataControlDevice},
        wl_data_managers::DataDeviceManagerExt,
        wl_data_managers::ExtDataControlManagerV1,
        wl_data_offer::{DataControlOfferEvent, OFFERED_TXT_MIME_TYPES, WlDataControlOffer},
        wl_data_source::WlDataControlSourceEvent,
        wl_display::WlDisplay,
        wl_registry::{BoundInterface, WlSeat},
    },
    wl_buffered_stream::WlBufferedStream,
    wl_message_reader::WlMessageReader,
    wl_message_router::WlMessageRouter,
    wl_offer_tracker::WlOfferTracker,
    wl_session_manager::{WlSessionEvent, WlSessionManager},
};

pub use event_fd::EventFd;
pub use fd_io::{serve_bytes, serve_file};
pub use log_writer::{Colors, LOGGER, debug_enabled, init_logging};
pub use mime::parse_mime;
pub use poll::{PollFd, PollResult, poll};
