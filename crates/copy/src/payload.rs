use std::{
    fs::{self, File, OpenOptions},
    hash::{BuildHasher, Hasher, RandomState},
    io::{self, ErrorKind, Read, Write},
    os::{fd::OwnedFd, unix::fs::OpenOptionsExt},
    path::Path,
    process,
};

use clipleaf_core::{ffi::O_TMPFILE, log_debug, serve_bytes, serve_file};

// The limit of a single terminal arg. the vast majority of use cases should fit within this size.
pub const MAX_INLINE_BYTES: usize = 128 << 10;

pub enum Payload {
    Bytes(Vec<u8>),
    File { file: File, len: u64 },
}

impl Payload {
    pub fn serve(&self, out: OwnedFd) -> io::Result<()> {
        match self {
            Payload::Bytes(bytes) => serve_bytes(out, bytes),
            Payload::File { file, len } => serve_file(out, file, *len),
        }
    }
}

pub fn store(input: &mut File, temp_dir: &Path) -> io::Result<Payload> {
    // Try and see if we can fit in memory within the MAX_INLINE_BYTES limit.
    let mut head = Vec::new();
    input.take(MAX_INLINE_BYTES as u64 + 1).read_to_end(&mut head)?;

    let head_len = head.len();

    if head_len <= MAX_INLINE_BYTES {
        log_debug!("Stored {} bytes in memory", head_len);
        return Ok(Payload::Bytes(head));
    }

    // If we reach here, the input is larger than MAX_INLINE_BYTES, so we store it in a temporary file.
    let mut file = open_tmpfile(temp_dir)?;
    file.write_all(&head)?;
    let tail_len = io::copy(input, &mut file)?;
    log_debug!("Stored {} bytes in temporary file", file.metadata()?.len());
    Ok(Payload::File {
        file,
        len: head_len as u64 + tail_len,
    })
}

fn open_tmpfile(dir: &Path) -> io::Result<File> {
    match OpenOptions::new().read(true).write(true).mode(0o600).custom_flags(O_TMPFILE).open(dir) {
        Err(e) if needs_fallback(&e) => {
            log_debug!("Falling back to unlinked temporary file due to O_TMPFILE not supported: {}", e);
            open_unlinked(dir)
        }
        other => {
            log_debug!("Temporary file with O_TMPFILE creation result: {:?}", other);
            other
        }
    }
    .map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("cannot create a temporary file in {}: {} (use --temp-dir)", dir.display(), e),
        )
    })
}

// EOPNOTSUPP: filesystem without O_TMPFILE. EISDIR: old kernel. EINVAL: wrong flag value for this arch.
fn needs_fallback(e: &io::Error) -> bool {
    matches!(e.kind(), ErrorKind::Unsupported | ErrorKind::IsADirectory | ErrorKind::InvalidInput)
}

fn open_unlinked(dir: &Path) -> io::Result<File> {
    for _ in 0..5 {
        let path = dir.join(format!(".clipleaf-{}-{:016x}", process::id(), random_suffix()));
        match OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => {
                // this uses unlink on UNIX, making the file nameless immediately after creation
                log_debug!("Created temporary file at {}. Unlinking it immediately.", path.display());
                fs::remove_file(&path)?;
                return Ok(file);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                log_debug!("Temporary file name already exists, retrying...");
                continue;
            }
            Err(e) => {
                log_debug!("Failed to create temporary file: {}", e);
                return Err(e);
            }
        }
    }
    Err(io::Error::new(ErrorKind::AlreadyExists, "no free temporary file name"))
}

fn random_suffix() -> u64 {
    RandomState::new().build_hasher().finish()
}

#[cfg(test)]
mod tests {
    //! What the binary-level tests (`copy/tests/scenarios.rs`) cannot see: a wrong `O_TMPFILE`
    //! value is hidden by the fallback, and the fallback itself only runs where the filesystem
    //! lacks O_TMPFILE. Sizes, offsets, spilling and temp-dir errors are covered through the binary.
    use clipleaf_core::ffi::O_TMPFILE;

    use super::*;
    use std::{
        env,
        io::{Seek, SeekFrom},
        os::fd::AsRawFd,
    };

    fn fd_target(file: &File) -> String {
        fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn o_tmpfile_flag_is_right_for_this_arch() {
        // The fallback would hide a wrong flag value, so open without it.
        let result = OpenOptions::new()
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(O_TMPFILE)
            .open(env::temp_dir());
        match result {
            Ok(file) => {
                let target = fd_target(&file);
                assert!(target.contains("/#") && target.ends_with(" (deleted)"), "{target}");
            }
            Err(e) if e.kind() == ErrorKind::Unsupported => {
                eprintln!("skipped: {} has no O_TMPFILE support", env::temp_dir().display());
            }
            Err(e) => panic!("O_TMPFILE rejected: {e}"),
        }
    }

    #[test]
    fn open_unlinked_leaves_no_name() {
        let mut file = open_unlinked(&env::temp_dir()).unwrap();
        let target = fd_target(&file);
        let name = target.strip_suffix(" (deleted)").unwrap_or_else(|| panic!("still named: {target}"));
        assert!(!fs::exists(name).unwrap(), "{name} exists");

        file.write_all(b"still readable").unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut back = Vec::new();
        file.read_to_end(&mut back).unwrap();
        assert_eq!(back, b"still readable");
    }

    #[test]
    fn only_missing_o_tmpfile_support_falls_back() {
        for kind in [ErrorKind::Unsupported, ErrorKind::IsADirectory, ErrorKind::InvalidInput] {
            assert!(needs_fallback(&kind.into()), "{kind:?}");
        }
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::PermissionDenied,
            ErrorKind::StorageFull,
            ErrorKind::ReadOnlyFilesystem,
        ] {
            assert!(!needs_fallback(&kind.into()), "{kind:?}");
        }
    }
}
