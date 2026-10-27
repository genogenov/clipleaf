mod entry;
mod fs_store;
mod payload;

pub mod wal;

pub const MAX_INLINE_TYPE_BYTES: usize = 5 * 1024;
pub const RESTORE_ID_LIMIT: u64 = 1 << 52;
pub const MAX_ENTRY_ID: u64 = (1 << 53) - 1;

pub use entry::*;
pub use fs_store::*;
pub use payload::*;
