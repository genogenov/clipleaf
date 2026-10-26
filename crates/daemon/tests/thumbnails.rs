//! Image thumbnails: every PNG or JPEG type of an entry gets a Thumb record some time after its
//! Insert: a JPEG of at most 256×256, upright, with transparency shown on a light checkerboard.
//! An image that can never get one (not decodable, over a limit) gets the "no thumbnail" marker
//! instead, and the next image still gets its thumbnail. The state is kept across restarts: an
//! image without either is queued again, one with either is not.
//!
//! The fixtures are made here (image's PNG encoder, jpeg-encoder, one hand-written PNG), so
//! there are no binary files in the repo.

mod common;

use std::{
    fs::{self, OpenOptions},
    io::Cursor,
    time::Instant,
};

use common::{wal::*, *};
use image::{ImageFormat, Rgb, RgbImage, RgbaImage};

const JPEG: &str = "image/jpeg";

const RED: [u8; 3] = [220, 30, 30];
const BLUE: [u8; 3] = [30, 30, 220];

fn png_rgb(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let img = RgbImage::from_fn(width, height, |x, y| Rgb(pixel(x, y)));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

fn png_rgba(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let img = RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y)));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

/// A baseline JPEG; with `orientation`, an EXIF APP1 segment holding only that tag.
fn jpeg(
    width: u16,
    height: u16,
    orientation: Option<u16>,
    pixel: impl Fn(u32, u32) -> [u8; 3],
) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for y in 0..height as u32 {
        for x in 0..width as u32 {
            rgb.extend(pixel(x, y));
        }
    }
    let mut out = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut out, 90);
    if let Some(o) = orientation {
        // "Exif\0\0", a little-endian TIFF header, IFD0 with one entry: 0x0112 SHORT 1 = o.
        let mut exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        exif.extend(o.to_le_bytes());
        exif.extend([0, 0, 0, 0, 0, 0]); // value padding, no next IFD
        enc.add_app_segment(1, exif).unwrap();
    }
    enc.encode(&rgb, width, height, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    out
}

/// A valid all-zero (transparent black) RGBA8 PNG of any size in under 1 MiB: one fixed-Huffman
/// deflate block of 258-byte back-references, which image's encoder would take seconds to find.
fn zero_png(width: u32, height: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend(kind);
        out.extend(data);
        let crc = crc32(&out[start..]);
        out.extend(crc.to_be_bytes());
    }
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
            }
        }
        !crc
    }
    struct Bits {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }
    impl Bits {
        fn put(&mut self, value: u32, n: u32) {
            self.acc |= (value as u64) << self.n;
            self.n += n;
            while self.n >= 8 {
                self.out.push(self.acc as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }
        /// Huffman codes go most significant bit first.
        fn code(&mut self, code: u32, n: u32) {
            self.put(code.reverse_bits() >> (32 - n), n);
        }
    }

    let raw_len = height as u64 * (1 + width as u64 * 4); // a filter byte per row, all zero
    let mut bits = Bits {
        out: vec![0x78, 0x01], // zlib: deflate, 32 KiB window
        acc: 0,
        n: 0,
    };
    bits.put(1, 1); // final block
    bits.put(1, 2); // fixed Huffman
    bits.code(0x30, 8); // literal 0
    let mut left = raw_len - 1;
    while left >= 258 {
        bits.code(0xC5, 8); // length 258 (symbol 285)
        bits.code(0, 5); // distance 1
        left -= 258;
    }
    for _ in 0..left {
        bits.code(0x30, 8);
    }
    bits.code(0, 7); // end of block
    bits.put(0, 7); // flush to a byte boundary
    let mut zlib = bits.out;
    let adler = ((raw_len % 65521) as u32) << 16 | 1; // a = 1 + Σ0, b = Σa = raw_len
    zlib.extend(adler.to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib);
    chunk(&mut png, b"IEND", &[]);
    png
}

fn decode(thumb: &Thumb) -> RgbImage {
    let img = image::load_from_memory_with_format(&thumb.jpeg, ImageFormat::Jpeg).unwrap();
    assert_eq!(
        (img.width(), img.height()),
        (thumb.width as u32, thumb.height as u32)
    );
    img.to_rgb8()
}

/// Within JPEG noise of `want` (flat areas at quality 70 stay within a few levels).
fn assert_near(img: &RgbImage, x: u32, y: u32, want: [u8; 3]) {
    let got = img.get_pixel(x, y).0;
    let close = got.iter().zip(want).all(|(&g, w)| g.abs_diff(w) <= 24);
    assert!(
        close,
        "pixel ({x}, {y}) is {got:?}, expected about {want:?}"
    );
}

fn halves(split_at: u32) -> impl Fn(u32, u32) -> [u8; 3] {
    move |x, _| if x < split_at { RED } else { BLUE }
}

#[test]
fn a_png_gets_a_thumbnail_that_fits_256_px_and_keeps_its_colours() {
    // With a text type before it: the thumbnail belongs to type 1.
    let mut daemon = Daemon::start();
    let png = png_rgb(1024, 512, halves(512));
    let entry = daemon.copy_new(&[(UTF8, b"diagram.png"), (PNG, &png)]);
    assert_eq!(entry.sizes(), [(UTF8, 11), (PNG, png.len() as u64)]);
    let thumb = daemon.wait_thumb(entry.id).expect("a thumbnail");
    assert_eq!(daemon.thumbs()[0].1, 1);
    assert_eq!((thumb.width, thumb.height), (256, 128));
    assert_eq!((thumb.src_width, thumb.src_height), (1024, 512));
    assert_eq!((thumb.has_alpha, thumb.orientation), (false, 1));
    let img = decode(&thumb);
    assert_near(&img, 64, 64, RED);
    assert_near(&img, 192, 64, BLUE);
}

#[test]
fn an_image_smaller_than_the_box_keeps_its_size() {
    let mut daemon = Daemon::start();
    let entry = daemon.copy_new(&[(PNG, &png_rgb(100, 40, halves(50)))]);
    let thumb = daemon.wait_thumb(entry.id).expect("a thumbnail");
    assert_eq!((thumb.width, thumb.height), (100, 40));
    assert_eq!((thumb.src_width, thumb.src_height), (100, 40));
}

#[test]
fn transparency_is_shown_on_a_light_checkerboard_of_8_px_squares() {
    // The left half is fully transparent red: the red must not show. The right half is opaque.
    let mut daemon = Daemon::start();
    let png = png_rgba(64, 64, |x, _| {
        if x < 32 {
            [255, 0, 0, 0]
        } else {
            [BLUE[0], BLUE[1], BLUE[2], 255]
        }
    });
    let entry = daemon.copy_new(&[(PNG, &png)]);
    let thumb = daemon.wait_thumb(entry.id).expect("a thumbnail");
    assert!(thumb.has_alpha);
    let img = decode(&thumb);
    // Square centres: (0,0) grey, (1,0) and (0,1) white, (1,1) grey again.
    assert_near(&img, 3, 3, [204; 3]);
    assert_near(&img, 11, 3, [255; 3]);
    assert_near(&img, 3, 11, [255; 3]);
    assert_near(&img, 11, 11, [204; 3]);
    assert_near(&img, 48, 32, BLUE);
}

#[test]
fn a_photo_taken_sideways_gets_an_upright_thumbnail_and_its_size_as_displayed() {
    // Stored 512×256 with EXIF orientation 6 (turn 90° clockwise to display): shown 256×512,
    // the stored left half (red) on top.
    let mut daemon = Daemon::start();
    let photo = jpeg(512, 256, Some(6), halves(256));
    let entry = daemon.copy_new(&[(JPEG, &photo)]);
    let thumb = daemon.wait_thumb(entry.id).expect("a thumbnail");
    assert_eq!((thumb.width, thumb.height), (128, 256));
    assert_eq!((thumb.src_width, thumb.src_height), (256, 512));
    assert_eq!(thumb.orientation, 6);
    let img = decode(&thumb);
    assert_near(&img, 64, 64, RED);
    assert_near(&img, 64, 192, BLUE);
}

#[test]
fn an_image_that_cannot_be_decoded_is_marked_as_having_no_thumbnail() {
    let mut daemon = Daemon::start();
    let garbage = daemon.copy_new(&[(PNG, b"not an image at all")]);
    assert_eq!(daemon.wait_thumb(garbage.id), None);

    // Cut off in the middle of its pixel data.
    let png = png_rgb(256, 256, |x, y| [x as u8, y as u8, (x ^ y) as u8]);
    let truncated = daemon.copy_new(&[(PNG, &png[..png.len() / 2])]);
    assert_eq!(daemon.wait_thumb(truncated.id), None);
}

#[test]
fn images_over_the_limits_get_no_thumbnail_and_the_next_image_still_does() {
    let mut daemon = Daemon::start();
    let started = Instant::now();

    // An 8K RGBA screenshot: 127 MiB decoded, over the 100 MiB cap.
    let screenshot = daemon.copy_new(&[(PNG, &zero_png(7680, 4320))]);
    assert_eq!(daemon.wait_thumb(screenshot.id), None);

    // A panorama wider than 8192 px.
    let panorama = daemon.copy_new(&[(JPEG, &jpeg(8193, 16, None, halves(4096)))]);
    assert_eq!(daemon.wait_thumb(panorama.id), None);

    // A JPEG file over 16 MiB (a small image followed by 16 MiB of other data, as phones
    // append their secondary images).
    let mut huge = jpeg(64, 64, None, halves(32));
    huge.resize(16 * 1024 * 1024 + 1, 0);
    let huge = daemon.copy_new(&[(JPEG, &huge)]);
    assert_eq!(daemon.wait_thumb(huge.id), None);

    // Each one is rejected from its header, before any pixel is decoded.
    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());

    let next = daemon.copy_new(&[(PNG, &png_rgb(300, 300, halves(150)))]);
    assert!(daemon.wait_thumb(next.id).is_some());
}

#[test]
fn an_image_whose_thumbnail_was_lost_in_a_crash_gets_it_after_the_restart() {
    // The daemon was killed after storing the image but before its Thumb record was written:
    // the restart queues it again. An image already marked as having no thumbnail is not.
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let no_thumb = daemon.copy_new(&[(PNG, b"not an image")]);
    assert_eq!(daemon.wait_thumb(no_thumb.id), None);
    // Noise: over the 5 KiB inline limit, so it is read back from its payload file.
    let noise = png_rgb(256, 256, |x, y| {
        let v = (x * 7919 + y * 104_729) ^ (x * y);
        [v as u8, (v >> 8) as u8, (v >> 16) as u8]
    });
    let lost = daemon.copy_new(&[(PNG, &noise)]);
    assert!(lost.types[0].in_file);
    let before = daemon.wait_thumb(lost.id).expect("a thumbnail");
    daemon.stop();

    // Cut the log just before that Thumb record (the last one).
    let wal = state.wal_bytes();
    let ends = record_ends(&wal);
    assert!(matches!(records(&wal).last(), Some(Record::Thumb { id, .. }) if *id == lost.id));
    OpenOptions::new()
        .write(true)
        .open(state.wal())
        .unwrap()
        .set_len(ends[ends.len() - 2] as u64)
        .unwrap();

    let mut daemon = Daemon::start_in(&state);
    assert_eq!(daemon.wait_thumb(lost.id), Some(before));
    // Both workers are idle at startup: a job for the marked image (a 12-byte non-image) would
    // have failed long before the 256×256 decode finished.
    assert!(
        daemon.thumbs().iter().all(|(id, ..)| *id != no_thumb.id),
        "{:?}",
        daemon.thumbs()
    );
}

#[test]
fn a_thumbnail_and_a_no_thumbnail_marker_survive_a_restart_and_a_compaction() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let a = daemon.copy_new(&[(PNG, &png_rgb(512, 512, halves(256)))]);
    let thumb = daemon.wait_thumb(a.id).expect("a thumbnail");
    let b = daemon.copy_new(&[(PNG, b"not an image")]);
    assert_eq!(daemon.wait_thumb(b.id), None);
    daemon.stop();
    state.append_dead(1024 * 1024 + 4096);
    let old_salt = salt_of(&state.wal_bytes());

    // The compacted log is written from the replayed history: both states are in its Inserts.
    let mut daemon = Daemon::start_in(&state);
    let records = daemon.wait_compacted(old_salt);
    let stored = |id: u64| {
        records
            .iter()
            .find_map(|r| match r {
                Record::Insert(i) if i.id == id => Some(i.types[0].thumb.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no Insert for {id} in {records:?}"))
    };
    assert_eq!(stored(a.id), Some(Some(thumb)));
    assert_eq!(stored(b.id), Some(None));
}

#[test]
fn both_image_workers_run_10_nice_levels_below_the_daemon() {
    let mut daemon = Daemon::start();
    let pid = daemon.pid();
    let nice = |task: &str| -> i32 {
        let stat = fs::read_to_string(format!("/proc/{pid}/task/{task}/stat")).unwrap();
        // Field 19; the name (field 2) may hold spaces, so count from its closing paren.
        let after_name = &stat[stat.rfind(')').unwrap() + 2..];
        after_name.split(' ').nth(16).unwrap().parse().unwrap()
    };
    let want = (nice(&pid.to_string()) + 10).min(19);
    // Each worker lowers its own priority when it starts: wait for both.
    let started = Instant::now();
    loop {
        let workers: Vec<i32> = fs::read_dir(format!("/proc/{pid}/task"))
            .unwrap()
            .filter_map(|t| {
                let task = t.unwrap().file_name().into_string().unwrap();
                let comm = fs::read_to_string(format!("/proc/{pid}/task/{task}/comm")).ok()?;
                comm.starts_with("image-").then(|| nice(&task))
            })
            .collect();
        if workers == [want, want] {
            break;
        }
        assert!(
            started.elapsed() < WAIT,
            "worker nice {workers:?}, expected {want}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
