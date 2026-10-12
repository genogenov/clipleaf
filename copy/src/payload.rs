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
    input
        .take(MAX_INLINE_BYTES as u64 + 1)
        .read_to_end(&mut head)?;

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
    match OpenOptions::new()
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(O_TMPFILE)
        .open(dir)
    {
        Err(e) if needs_fallback(&e) => {
            log_debug!(
                "Falling back to unlinked temporary file due to O_TMPFILE not supported: {}",
                e
            );
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
            format!(
                "cannot create a temporary file in {}: {} (use --temp-dir)",
                dir.display(),
                e
            ),
        )
    })
}

// EOPNOTSUPP: filesystem without O_TMPFILE. EISDIR: old kernel. EINVAL: wrong flag value for this arch.
fn needs_fallback(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::Unsupported | ErrorKind::IsADirectory | ErrorKind::InvalidInput
    )
}

fn open_unlinked(dir: &Path) -> io::Result<File> {
    for _ in 0..5 {
        let path = dir.join(format!(
            ".clipleaf-{}-{:016x}",
            process::id(),
            random_suffix()
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => {
                // this uses unlink on UNIX, making the file nameless immediately after creation
                log_debug!(
                    "Created temporary file at {}. Unlinking it immediately.",
                    path.display()
                );
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
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        "no free temporary file name",
    ))
}

fn random_suffix() -> u64 {
    RandomState::new().build_hasher().finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        io::{Seek, SeekFrom},
        os::fd::AsRawFd,
        thread,
    };

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn pipe_with(data: Vec<u8>) -> File {
        let (reader, mut writer) = io::pipe().unwrap();
        // Ignore EPIPE: some tests make store() give up before reading everything.
        thread::spawn(move || writer.write_all(&data));
        File::from(OwnedFd::from(reader))
    }

    fn file_with(data: &[u8]) -> File {
        let mut file = open_unlinked(&env::temp_dir()).unwrap();
        file.write_all(data).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file
    }

    fn served(payload: &Payload) -> Vec<u8> {
        let (mut reader, writer) = io::pipe().unwrap();
        let reading = thread::spawn(move || {
            let mut out = Vec::new();
            reader.read_to_end(&mut out).unwrap();
            out
        });
        payload.serve(OwnedFd::from(writer)).unwrap();
        reading.join().unwrap()
    }

    fn spilled(payload: &Payload) -> &File {
        match payload {
            Payload::File { file, .. } => file,
            Payload::Bytes(bytes) => panic!("{} bytes stayed in memory", bytes.len()),
        }
    }

    fn fd_target(file: &File) -> String {
        fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    // assert_eq! would print megabytes of bytes on failure.
    fn assert_same(actual: &[u8], expected: &[u8], what: &str) {
        let first_diff = actual.iter().zip(expected).position(|(a, b)| a != b);
        assert!(
            actual == expected,
            "{what}: got {} bytes, expected {}, first difference at {first_diff:?}",
            actual.len(),
            expected.len()
        );
    }

    #[test]
    fn input_up_to_the_limit_stays_in_memory() {
        for len in [0, 1, MAX_INLINE_BYTES] {
            let expected = data(len);
            let payload = store(&mut pipe_with(expected.clone()), &env::temp_dir()).unwrap();
            assert!(
                matches!(&payload, Payload::Bytes(bytes) if *bytes == expected),
                "len {len}"
            );
            assert_same(&served(&payload), &expected, &format!("len {len}"));
        }
    }

    #[test]
    fn larger_input_spills_to_a_nameless_file() {
        for len in [MAX_INLINE_BYTES + 1, 3 << 20] {
            let expected = data(len);
            let payload = store(&mut pipe_with(expected.clone()), &env::temp_dir()).unwrap();
            let file = spilled(&payload);
            assert_eq!(file.metadata().unwrap().len(), len as u64);
            assert!(
                fd_target(file).ends_with(" (deleted)"),
                "{}",
                fd_target(file)
            );
            assert_same(&served(&payload), &expected, &format!("len {len}, paste 1"));
            assert_same(&served(&payload), &expected, &format!("len {len}, paste 2"));
        }
    }

    #[test]
    fn regular_file_input_is_read_from_its_current_offset() {
        for len in [100, 3 << 20] {
            let expected = data(len);
            let mut input = file_with(&expected);
            input.seek(SeekFrom::Start(10)).unwrap();
            let payload = store(&mut input, &env::temp_dir()).unwrap();
            assert_same(&served(&payload), &expected[10..], &format!("len {len}"));
        }
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
                assert!(
                    target.contains("/#") && target.ends_with(" (deleted)"),
                    "{target}"
                );
            }
            Err(e) if e.kind() == ErrorKind::Unsupported => {
                eprintln!(
                    "skipped: {} has no O_TMPFILE support",
                    env::temp_dir().display()
                );
            }
            Err(e) => panic!("O_TMPFILE rejected: {e}"),
        }
    }

    #[test]
    fn open_unlinked_leaves_no_name() {
        let mut file = open_unlinked(&env::temp_dir()).unwrap();
        let target = fd_target(&file);
        let name = target
            .strip_suffix(" (deleted)")
            .unwrap_or_else(|| panic!("still named: {target}"));
        assert!(!fs::exists(name).unwrap(), "{name} exists");

        file.write_all(b"still readable").unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut back = Vec::new();
        file.read_to_end(&mut back).unwrap();
        assert_eq!(back, b"still readable");
    }

    #[test]
    fn only_missing_o_tmpfile_support_falls_back() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::IsADirectory,
            ErrorKind::InvalidInput,
        ] {
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

    #[test]
    fn missing_temp_dir_fails_only_when_spilling() {
        let dir = env::temp_dir().join("clipleaf-test-missing-dir");
        assert!(!fs::exists(&dir).unwrap());

        let payload = store(&mut pipe_with(data(100 << 10)), &dir).unwrap();
        assert!(matches!(payload, Payload::Bytes(_)));

        let Err(err) = store(&mut pipe_with(data(3 << 20)), &dir) else {
            panic!("spilled into a missing directory");
        };
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(err.to_string().contains("--temp-dir"), "{err}");
    }
}
