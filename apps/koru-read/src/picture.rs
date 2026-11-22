// SPDX-License-Identifier: MPL-2.0

//! **Pixels for the reader**: a picture file decoded, turned upright and
//! scaled down, or a scanned page rendered — and the bounds that keep a
//! hostile file from making the worker hold more than it can.
//!
//! The reader is handed one thing, tightly packed 8-bit RGB, already upright
//! ([`koru_read::Picture`]). Everything that turns a file into that happens
//! here, in the worker, so the reader never sees a format:
//!
//! - **pictures**: PNG, JPEG, GIF, WebP, BMP and TIFF, by their bytes, never
//!   by the recorded type; the first frame of an animation; the photograph's
//!   EXIF orientation applied (a phone stores a portrait photo on its side and
//!   says so); transparency flattened onto white, because dark text on a
//!   transparent background would otherwise become dark on black; and a
//!   picture longer than [`MAX_SIDE`] scaled down to it;
//! - **scanned pages**: rendered with `hayro` at [`RENDER_SCALE`] (144 dpi),
//!   with the longer side capped at [`MAX_SIDE`] — Plan 0.1's settings
//!   ([`page_scale`]).
//!
//! ## The bounds, and why they come before decoding
//!
//! `hayro` decodes every image a page draws **at its full size**, whatever
//! the output scale: Plan 0.1's synthetic 20,000 x 20,000 image, 380 KiB on
//! disk, took about 760 MiB to render. The worker's 1 GiB memory ceiling is
//! the backstop, not the defence. So a page is rendered only when every image
//! it draws declares a size within [`MAX_PAGE_IMAGE_PIXELS`], and all of them
//! together within [`MAX_PAGE_IMAGES_PIXELS`] ([`page_images_allowed`]). The
//! declared sizes come from the text pass, which reads each image's dictionary
//! and decodes nothing. A picture file is held to [`MAX_PICTURE_PIXELS`] by its
//! header before its pixels are decoded, and the decoder to
//! [`MAX_DECODE_BYTES`].
//!
//! None of these four is measured: each is a few times what a real document
//! needs (an A3 page scanned at 600 dpi is 70 megapixels; a 48-megapixel phone
//! photograph is the largest common picture) and a fraction of the bomb.

use std::io::Cursor;

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, RgbImage, RgbaImage};
use koru_read::Picture;

use crate::accept::Layer;

/// Pages render at this many pixels per PDF point: 144 dpi. *Measured, Plan
/// 0.1*.
pub const RENDER_SCALE: f32 = 2.0;

/// No picture handed to the reader is longer than this on either side: a
/// rendered page is capped at it, a picture is scaled down to it. *Measured,
/// Plan 0.1*.
pub const MAX_SIDE: u32 = 4_000;

/// A picture file whose header declares more pixels than this is not decoded.
pub const MAX_PICTURE_PIXELS: u64 = 64_000_000;

/// The most the decoder may allocate for one picture.
pub const MAX_DECODE_BYTES: u64 = 384 * 1024 * 1024;

/// A picture file larger than this is not read at all.
pub const MAX_PICTURE_FILE_BYTES: u64 = 128 * 1024 * 1024;

/// A page is not rendered when one image it draws declares more pixels than
/// this...
pub const MAX_PAGE_IMAGE_PIXELS: u64 = 80_000_000;

/// ...or when all of them together do.
pub const MAX_PAGE_IMAGES_PIXELS: u64 = 200_000_000;

/// Pixels ready for the reader: tightly packed RGB, top row first, upright.
#[derive(Clone, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl std::fmt::Debug for Pixels {
    /// The size only: pixels are a person's picture.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pixels({}x{})", self.width, self.height)
    }
}

impl Pixels {
    /// The contract's view of these pixels.
    pub fn picture(&self) -> Option<Picture<'_>> {
        Picture::rgb8(self.width, self.height, &self.rgb)
    }

    fn from_rgb(image: RgbImage) -> Pixels {
        Pixels {
            width: image.width(),
            height: image.height(),
            rgb: image.into_raw(),
        }
    }
}

/// Why a picture file gives the reader nothing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Not one of the six formats this worker reads (HEIC, SVG, a format this
    /// build does not know). A local fact: another device may read it.
    Unsupported,
    /// One of the six, and its bytes do not decode. True on every device.
    Damaged,
    /// It declares more than the bounds above allow.
    TooLarge,
}

/// The formats a picture is read in. Fixed here rather than taken from
/// whatever the decoder was compiled with, so two builds agree on what is
/// readable.
const FORMATS: [ImageFormat; 6] = [
    ImageFormat::Png,
    ImageFormat::Jpeg,
    ImageFormat::Gif,
    ImageFormat::WebP,
    ImageFormat::Bmp,
    ImageFormat::Tiff,
];

/// **Decode a picture file for the reader.**
pub fn decode(bytes: &[u8]) -> Result<Pixels, DecodeError> {
    if bytes.len() as u64 > MAX_PICTURE_FILE_BYTES {
        return Err(DecodeError::TooLarge);
    }
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| DecodeError::Damaged)?;
    match reader.format() {
        Some(format) if FORMATS.contains(&format) => {}
        _ => return Err(DecodeError::Unsupported),
    }
    let mut reader = reader;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(classify)?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PICTURE_PIXELS {
        return Err(DecodeError::TooLarge);
    }
    // A missing or unreadable EXIF block is an upright picture, not a
    // failure.
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(classify)?;
    image.apply_orientation(orientation);
    let rgb = if image.color().has_alpha() {
        flatten_on_white(&image.to_rgba8())
    } else {
        image.to_rgb8()
    };
    drop(image);
    Ok(Pixels::from_rgb(fit(rgb, MAX_SIDE)))
}

fn classify(error: image::ImageError) -> DecodeError {
    match error {
        image::ImageError::Limits(_) => DecodeError::TooLarge,
        image::ImageError::Unsupported(_) => DecodeError::Unsupported,
        _ => DecodeError::Damaged,
    }
}

/// Composite onto white, so text drawn on a transparent background keeps its
/// contrast.
pub fn flatten_on_white(rgba: &RgbaImage) -> RgbImage {
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let alpha = u32::from(a);
        let over = |channel: u8| -> u8 {
            ((u32::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
        };
        image::Rgb([over(r), over(g), over(b)])
    })
}

/// Scale `image` down so neither side is longer than `max_side`, keeping its
/// proportions. A picture that already fits is returned as it is.
pub fn fit(image: RgbImage, max_side: u32) -> RgbImage {
    let (width, height) = image.dimensions();
    let longer = width.max(height);
    if longer <= max_side {
        return image;
    }
    let scale = f64::from(max_side) / f64::from(longer);
    let scaled = |side: u32| ((f64::from(side) * scale).round() as u32).clamp(1, max_side);
    image::imageops::resize(
        &image,
        scaled(width),
        scaled(height),
        image::imageops::FilterType::Triangle,
    )
}

/// The scale a page of `width` x `height` points renders at: 144 dpi, unless
/// that would make its longer side exceed [`MAX_SIDE`] pixels.
pub fn page_scale(width: f32, height: f32) -> f32 {
    let longer = width.max(height);
    if !longer.is_finite() || longer <= 0.0 {
        return RENDER_SCALE;
    }
    RENDER_SCALE.min(MAX_SIDE as f32 / longer)
}

/// **Whether a page may be rendered**, by the sizes its images declare.
pub fn page_images_allowed(layer: &Layer) -> bool {
    layer.largest_image <= MAX_PAGE_IMAGE_PIXELS && layer.image_pixels <= MAX_PAGE_IMAGES_PIXELS
}

/// A rendered page's RGBA, on an opaque white background, as RGB.
pub fn rgba_to_rgb(width: u32, height: u32, rgba: &[u8]) -> Option<Pixels> {
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if rgba.len() != expected || width == 0 || height == 0 {
        return None;
    }
    let mut rgb = Vec::with_capacity(expected / 4 * 3);
    for pixel in rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&pixel[..3]);
    }
    Some(Pixels { width, height, rgb })
}

#[cfg(test)]
mod tests;
