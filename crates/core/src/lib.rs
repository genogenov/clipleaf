#![warn(clippy::undocumented_unsafe_blocks)]
mod content_kind;
mod fd_io;
pub mod ffi;
mod file_list;
#[doc(hidden)]
mod log_writer;
mod mime;
mod unix_fd_stream;
mod wl;

pub use wl::{
    objects::{
        wl_data_control_device::{DataControlDeviceEvent, WlDataControlDevice},
        wl_data_managers::DataDeviceManagerExt,
        wl_data_managers::ExtDataControlManagerV1,
        wl_data_offer::{DataControlOfferEvent, WlDataControlOffer},
        wl_data_source::{WlDataControlSource, WlDataControlSourceEvent},
        wl_display::WlDisplay,
        wl_registry::{BoundInterface, WlSeat},
    },
    wl_buffered_stream::WlBufferedStream,
    wl_message_reader::WlMessageReader,
    wl_message_router::WlMessageRouter,
    wl_offer_tracker::WlOfferTracker,
    wl_session_manager::{WlSessionEvent, WlSessionManager},
};

pub use content_kind::{ContentKind, ContentKinds};
pub use fd_io::{serve_bytes, serve_bytes_nonblocking, serve_file, serve_file_nonblocking};
pub use file_list::{FileListFormat, FileListScan};
pub use log_writer::{Colors, LOGGER, debug_enabled, init_logging};
pub use mime::{
    Mime, MimeKind, classify_mime, parse_mime, pick_best_mime, pick_fetch_mimes,
    types as mime_types,
};
