mod entry;
mod fs_store;
mod payload;

pub mod wal;

pub const MAX_INLINE_TYPE_BYTES: usize = 5 * 1024; // config in step 5

/// Upper bound on the types fetched per copy; the default pick yields at most 6 (text + image + 4 file lists).
pub const MAX_FETCHED_TYPES: usize = 10;

pub use entry::*;
pub use fs_store::*;
pub use payload::*;
