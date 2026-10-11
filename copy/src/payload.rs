use std::{
    fs::{self, File, OpenOptions},
    hash::{BuildHasher, Hasher, RandomState},
    io::{self, ErrorKind, Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::{FileTypeExt, OpenOptionsExt},
    },
    path::Path,
    process, ptr,
};

use clipleaf_core::{
    ffi::{F_GETPIPE_SZ, F_SETFL, F_SETPIPE_SZ, fcntl, splice},
    log_debug,
};

// The limit of a single terminal arg. the vast majority of use cases should fit within this size.
pub const MAX_INLINE_BYTES: usize = 128 << 10;

// O_TMPFILE = __O_TMPFILE | O_DIRECTORY; differ between architectures.
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "arm",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "m68k"
))]
const O_TMPFILE: i32 = (1 << 22) | (1 << 14);
#[cfg(any(target_arch = "sparc", target_arch = "sparc64"))]
const O_TMPFILE: i32 = (1 << 25) | (1 << 16);
#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "arm",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "m68k",
    target_arch = "sparc",
    target_arch = "sparc64"
)))]
const O_TMPFILE: i32 = (1 << 22) | (1 << 16); // kernel default: x86, riscv, loongarch, s390x, mips

pub enum Payload {
    Bytes(Vec<u8>),
    File { file: File, len: u64 },
}

impl Payload {
    pub fn serve(&self, out: OwnedFd) -> io::Result<()> {
        // SAFETY: F_SETFL clears only the flags that we have access to.
        // We do this to clear O_NONBLOCK if set to ensure blocking writes.
        let fcntl_result = unsafe { fcntl(out.as_raw_fd(), F_SETFL, 0) };
        if fcntl_result == -1 {
            log_debug!(
                "Failed to clear O_NONBLOCK on output FD: {}",
                io::Error::last_os_error()
            );
        }

        let mut out = File::from(out);
        let is_pipe = out.metadata()?.file_type().is_fifo();

        // anything larger than 4 KiB going into a pipe, we try to increase the pipe size for better performance.
        if is_pipe && self.len() > 4096 {
            const PASTE_PIPE_SIZE: i32 = 1 << 20;
            // SAFETY: out is an open pipe; F_GETPIPE_SZ takes no argument, F_SETPIPE_SZ one int.
            if unsafe { fcntl(out.as_raw_fd(), F_GETPIPE_SZ) } < PASTE_PIPE_SIZE {
                unsafe { fcntl(out.as_raw_fd(), F_SETPIPE_SZ, PASTE_PIPE_SIZE) }; // errors ignored: only speed is lost
            }
        }

        match self {
            Payload::Bytes(bytes) => out.write_all(bytes),
            Payload::File { file, len } => {
                if is_pipe {
                    log_debug!("Using two-stage splice for FIFO(pipe) output");
                    return write_all_two_stage_splice(out.into(), file, *len);
                }
                let mut file: &File = file;
                file.seek(SeekFrom::Start(0))?;
                io::copy(&mut file, &mut out).map(drop)
            }
        }
    }

    fn len(&self) -> u64 {
        match self {
            Payload::Bytes(bytes) => bytes.len() as u64,
            Payload::File { len, .. } => *len,
        }
    }
}

// Use a 2 stage splice to do zero(user space) copy from the payload file to the output pipe.
// Two stages overlap the page-cache lookup of our file with the paster draining its pipe: ~20% faster on tmpfs for 100 MiB.
// A direct splice keeps the paster's pipe locked during that lookup, so we and the paster take turns (~100 waits per 100 MiB assuming 1MB pipe).
fn write_all_two_stage_splice(out: OwnedFd, payload: &File, len: u64) -> io::Result<()> {
    let payload_fd = payload.as_raw_fd();
    let mut temp_in_offset = 0i64;
    let out_fd = out.as_raw_fd();

    let (reader, writer) = io::pipe()?;
    let temp_fd_writer = writer.as_raw_fd();
    let temp_fd_reader = reader.as_raw_fd();

    // SAFETY: fd is an open pipe we own and F_SETPIPE_SZ is a valid fcntl command for setting the pipe size, which takes one arg.
    // If this fails we are OK to ignore and proceed.
    const ONE_MB: i32 = 1 << 20;
    unsafe { fcntl(temp_fd_writer, F_SETPIPE_SZ, ONE_MB) };

    while temp_in_offset < len as i64 {
        // SAFETY: we are splicing from a temp, unlinked file that never changes once written and nobody else uses.
        let mut pending_bytes = unsafe {
            splice(
                payload_fd,
                &mut temp_in_offset,
                temp_fd_writer,
                ptr::null_mut(),
                ONE_MB as usize,
                0,
            )
        };

        if pending_bytes == 0 {
            // should not happen as being in the loop means there are more bytes to move.
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "Unexpected EOF of payload while splicing",
            ));
        }

        if pending_bytes == -1 {
            let err = io::Error::last_os_error();
            match err.kind() {
                ErrorKind::Interrupted => continue,
                _ => return Err(err),
            }
        }

        while pending_bytes > 0 {
            // SAFETY: both pipe fds are open for the whole call.
            let temp_written = unsafe {
                splice(
                    temp_fd_reader,
                    ptr::null_mut(),
                    out_fd,
                    ptr::null_mut(),
                    pending_bytes as usize,
                    0,
                )
            };

            if temp_written == -1 {
                let err = io::Error::last_os_error();
                match err.kind() {
                    ErrorKind::Interrupted => continue,
                    _ => return Err(err),
                }
            }

            if temp_written == 0 {
                return Err(io::Error::new(
                    ErrorKind::WriteZero,
                    "paste pipe accepted no data",
                ));
            }

            pending_bytes -= temp_written;
        }
    }

    debug_assert!(
        temp_in_offset == len as i64,
        "Not all input bytes were spliced"
    );

    Ok(())
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
    use std::{env, os::fd::AsRawFd, thread};

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

    // One in-memory payload and one spilled payload, both larger than a default 64 KiB pipe.
    fn payloads() -> [(&'static str, Vec<u8>, Payload); 2] {
        let small = data(MAX_INLINE_BYTES);
        let large = data(3 << 20);
        let inline = store(&mut pipe_with(small.clone()), &env::temp_dir()).unwrap();
        let spilled = store(&mut pipe_with(large.clone()), &env::temp_dir()).unwrap();
        [("in memory", small, inline), ("spilled", large, spilled)]
    }

    #[test]
    fn spilled_payload_into_a_regular_file_uses_the_fallback() {
        let (_, expected, payload) = payloads().into_iter().nth(1).unwrap();
        let mut out = open_unlinked(&env::temp_dir()).unwrap();
        payload
            .serve(OwnedFd::from(out.try_clone().unwrap()))
            .unwrap();
        out.seek(SeekFrom::Start(0)).unwrap();
        let mut back = Vec::new();
        out.read_to_end(&mut back).unwrap();
        assert_same(&back, &expected, "regular file");
    }

    // O_NONBLOCK is 0o4000 except on mips and sparc.
    #[cfg(not(any(
        target_arch = "mips",
        target_arch = "mips32r6",
        target_arch = "mips64",
        target_arch = "mips64r6",
        target_arch = "sparc",
        target_arch = "sparc64"
    )))]
    #[test]
    fn a_non_blocking_paste_pipe_with_a_slow_reader_still_gets_everything() {
        const O_NONBLOCK: i32 = 0o4000;
        for (what, expected, payload) in payloads() {
            let (mut reader, writer) = io::pipe().unwrap();
            // SAFETY: writer is an open pipe; F_SETFL takes one int.
            assert_eq!(unsafe { fcntl(writer.as_raw_fd(), F_SETFL, O_NONBLOCK) }, 0);
            let reading = thread::spawn(move || {
                // Let the pipe fill up first, so a non-blocking write would hit EAGAIN.
                thread::sleep(std::time::Duration::from_millis(50));
                let mut out = Vec::new();
                reader.read_to_end(&mut out).unwrap();
                out
            });
            payload.serve(OwnedFd::from(writer)).unwrap();
            assert_same(&reading.join().unwrap(), &expected, what);
        }
    }

    #[test]
    fn a_reader_that_closes_early_ends_the_paste_with_broken_pipe() {
        // The in-memory payload now fits in the grown paste pipe, so only a spilled one can hit EPIPE.
        let (what, _, payload) = payloads().into_iter().nth(1).unwrap();
        let (mut reader, writer) = io::pipe().unwrap();
        let reading = thread::spawn(move || {
            let mut first = [0u8; 1024];
            reader.read_exact(&mut first).unwrap();
        });
        let err = payload.serve(OwnedFd::from(writer)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::BrokenPipe, "{what}: {err}");
        reading.join().unwrap();
    }

    #[test]
    fn the_paste_pipe_is_grown_to_1_mib_only_for_payloads_over_a_page() {
        // Unprivileged pipes can't exceed 1 MiB, so the never-shrink branch can't be exercised here.
        for (len, start, expected) in [
            (1, 64 << 10, 64 << 10),
            (100 << 10, 64 << 10, 1 << 20),
            (100 << 10, 1 << 20, 1 << 20),
        ] {
            let (mut reader, writer) = io::pipe().unwrap();
            let probe = reader.try_clone().unwrap();
            // SAFETY: writer is an open pipe; F_SETPIPE_SZ takes one int.
            unsafe { fcntl(writer.as_raw_fd(), F_SETPIPE_SZ, start) };
            let reading = thread::spawn(move || {
                let mut out = Vec::new();
                reader.read_to_end(&mut out).unwrap();
                out.len()
            });
            Payload::Bytes(data(len))
                .serve(OwnedFd::from(writer))
                .unwrap();
            assert_eq!(reading.join().unwrap(), len);
            // SAFETY: probe is an open read end of the same pipe; F_GETPIPE_SZ takes no argument.
            let size = unsafe { fcntl(probe.as_raw_fd(), F_GETPIPE_SZ) };
            assert_eq!(size, expected, "len {len}, started at {start}");
        }
    }

    #[test]
    fn serving_leaves_no_file_descriptors_open() {
        let open_fds = || fs::read_dir("/proc/self/fd").unwrap().count();
        let payload = store(&mut pipe_with(data(MAX_INLINE_BYTES + 1)), &env::temp_dir()).unwrap();
        let before = open_fds();
        for _ in 0..200 {
            served(&payload);
        }
        // Other tests run in parallel and open a few fds; a leak would add 400.
        let after = open_fds();
        assert!(after < before + 50, "{before} fds before, {after} after");
    }
}
