use std::{
    borrow::Cow,
    fs::File,
    io::{self, BufReader, Cursor, ErrorKind, Read, Seek},
};

use clipleaf_core::log_debug;
use image::{
    DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits, Pixel, Rgba,
    RgbaImage,
    metadata::Orientation::{Rotate90, Rotate90FlipH, Rotate270, Rotate270FlipH},
};

use crate::{
    image::{
        JPEG_MAX_DECODED_SIZE, JPEG_MAX_FILE_SIZE, JPEG_MAX_WIDTH, MAX_INPUT_IMG_HEIGHT,
        MAX_TOTAL_ALLOC, PNG_MAX_DECODED_SIZE, PNG_MAX_WIDTH, THUMBNAIL_BOX_SIZE,
        THUMBNAIL_QUALITY,
    },
    storage::{ThumbNail, wal::encoder::MAX_THUMB_BYTES},
};

pub struct ImageSourceReader<R> {
    inner: BufReader<R>,
    len: u64,
}

impl<T: AsRef<[u8]>> ImageSourceReader<Cursor<T>> {
    pub fn from_bytes(bytes: T) -> Self {
        ImageSourceReader {
            len: bytes.as_ref().len() as u64,
            inner: BufReader::with_capacity(8 << 10, Cursor::new(bytes)),
        }
    }
}

impl ImageSourceReader<File> {
    pub fn from_file(f: File) -> io::Result<Self> {
        let len = f.metadata()?.len();
        Ok(ImageSourceReader {
            len,
            inner: BufReader::with_capacity(8 << 10, f),
        })
    }
}

impl<R> ImageSourceReader<R>
where
    R: Read + Seek,
{
    pub fn into_reader(self) -> BufReader<R> {
        self.inner
    }
}

pub enum ImageProcessingFail {
    Permanent(Cow<'static, str>),
    TransientIO(io::Error),
    PermanentIO(io::Error),
    Unsupported(Cow<'static, str>),
}

impl ImageProcessingFail {
    pub fn permanent_err(err: &'static str) -> ImageProcessingFail {
        ImageProcessingFail::Permanent(std::borrow::Cow::Borrowed(err))
    }
}

impl From<io::Error> for ImageProcessingFail {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            ErrorKind::Interrupted | ErrorKind::OutOfMemory | ErrorKind::TimedOut => {
                ImageProcessingFail::TransientIO(e)
            }
            _ => ImageProcessingFail::PermanentIO(e),
        }
    }
}

impl From<ImageError> for ImageProcessingFail {
    fn from(value: ImageError) -> Self {
        match value {
            ImageError::IoError(e) => e.into(), // reuse the io rule
            e => ImageProcessingFail::Permanent(e.to_string().into()),
        }
    }
}

pub fn make_thumbnail<R>(src: ImageSourceReader<R>) -> Result<ThumbNail, ImageProcessingFail>
where
    R: Read + Seek,
{
    let len = src.len;
    let mut reader = ImageReader::new(src.into_reader()).with_guessed_format()?;
    let (max_width, max_decoded, format) = match reader.format() {
        Some(ImageFormat::Png) => (PNG_MAX_WIDTH, PNG_MAX_DECODED_SIZE, ImageFormat::Png),
        Some(ImageFormat::Jpeg) => {
            if len > JPEG_MAX_FILE_SIZE {
                return Err(ImageProcessingFail::Permanent(
                    format!("jpeg file cap exceeded. Len: {len}").into(),
                ));
            }
            (JPEG_MAX_WIDTH, JPEG_MAX_DECODED_SIZE, ImageFormat::Jpeg)
        }
        _ => {
            return Err(ImageProcessingFail::Unsupported(
                "Skipping image thumbnail processing - not png/jpeg".into(),
            ));
        }
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_width);
    limits.max_image_height = Some(MAX_INPUT_IMG_HEIGHT);
    limits.max_alloc = Some(MAX_TOTAL_ALLOC);

    log_debug!(
        "Img worker: processing input img format: {:?} with max width {}",
        format,
        max_width
    );

    reader.limits(limits);
    let mut dec = reader.into_decoder()?;

    if dec.total_bytes() > max_decoded {
        return Err(ImageProcessingFail::Permanent(
            format!("decoded cap exceeded: {}", dec.total_bytes()).into(),
        ));
    }

    let (w, h) = dec.dimensions();

    log_debug!(
        "Img worker: Decoded total bytes {} (input len: {}), width: {}, height: {}",
        dec.total_bytes(),
        len,
        w,
        h
    );

    let orientation = dec.orientation()?;
    let img = DynamicImage::from_decoder(dec)?; // decoder + JPEG input copy dropped here
    let mut downsized = if w > THUMBNAIL_BOX_SIZE || h > THUMBNAIL_BOX_SIZE {
        img.thumbnail(THUMBNAIL_BOX_SIZE, THUMBNAIL_BOX_SIZE)
    } else {
        img
    };
    downsized.apply_orientation(orientation);
    let rgba = downsized.into_rgba8();
    let has_alpha = rgba.pixels().any(|p| p[3] < 255);
    let (width, height) = rgba.dimensions();
    let jpeg = encode_thumbnail_jpeg(rgba, has_alpha)?;
    if jpeg.len() > MAX_THUMB_BYTES as usize {
        return Err(ImageProcessingFail::permanent_err("thumb too big"));
    }
    let swap = matches!(
        orientation,
        Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH
    );
    let (src_width, src_height) = if swap { (h, w) } else { (w, h) };
    log_debug!(
        "Img worker: produced thumbnail: W:{width}, H:{height}, size: {}",
        jpeg.len()
    );
    Ok(ThumbNail {
        width: width as u16,
        height: height as u16,
        has_alpha,
        src_width,
        src_height,
        orientation: orientation.to_exif(),
        jpeg: jpeg.into(),
    })
}

fn encode_thumbnail_jpeg(
    mut rgba: RgbaImage,
    has_alpha: bool,
) -> Result<Vec<u8>, ImageProcessingFail> {
    if has_alpha {
        log_debug!("Img: worker: Img has alpha, blending into background");
        blend_into_bg(&mut rgba);
    }
    let (width, height) = rgba.dimensions();
    let mut out = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut out, THUMBNAIL_QUALITY);
    enc.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0);
    // enc.set_optimized_huffman_tables(true);
    enc.encode(
        &rgba,
        width as u16,
        height as u16,
        jpeg_encoder::ColorType::Rgba,
    )
    .map_err(|e| {
        ImageProcessingFail::Permanent(format!("Error during thumbnail encode: {e}").into())
    })?;

    // let test = current_dir()?.join("test_img.jpg");
    // OpenOptions::new()
    //     .create(true)
    //     .write(true)
    //     .open(&test)?
    //     .write_all(&out)?;
    // log_debug!("Written {}", &test.display());
    Ok(out)
}

fn blend_into_bg(rgba: &mut RgbaImage) {
    // light checkerboard 8 px squares from the top-left
    const BG_TILE_SIZE: u32 = 8;
    const BG_TILE_DARK: Rgba<u8> = Rgba([204, 204, 204, 255]);
    const BG_TILE_LIGHT: Rgba<u8> = Rgba([255, 255, 255, 255]);
    for (x, y, pixel) in rgba.enumerate_pixels_mut() {
        // the BG being aligned to 8 byte blocks produces smaller images.
        let bg_color = if (x / BG_TILE_SIZE + y / BG_TILE_SIZE).is_multiple_of(2) {
            BG_TILE_DARK
        } else {
            BG_TILE_LIGHT
        };
        *pixel = {
            let mut p = bg_color;
            p.blend(pixel);
            p
        };
    }
}
