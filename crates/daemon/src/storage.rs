mod entry;
mod fs_store;
mod payload;

pub const MAX_INLINE_TYPE_BYTES: usize = 5 * 1024; // config in step 5

pub use entry::*;
pub use fs_store::*;
pub use payload::*;
