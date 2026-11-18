// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::fixtures::pictures;

fn at(pixels: &Pixels, x: u32, y: u32) -> [u8; 3] {
    let at = ((y * pixels.width + x) * 3) as usize;
    [pixels.rgb[at], pixels.rgb[at + 1], pixels.rgb[at + 2]]
}

#[test]
fn a_png_decodes_to_its_pixels() {
    let grid = pictures::grid(3, 2);
    let pixels = decode(&pictures::png(&grid)).unwrap();
    assert_eq!((pixels.width, pixels.height), (3, 2));
    assert_eq!(pixels.rgb, grid.into_raw());
    assert!(pixels.picture().is_some());
}

/// **A photograph's EXIF orientation is applied**: a phone stores a portrait
/// photo on its side and says "turn me a quarter clockwise" (6).
#[test]
fn the_exif_orientation_turns_the_picture_upright() {
    let grid = pictures::grid(3, 2);
    let pixels = decode(&pictures::png_with_exif(&grid, Some(6))).unwrap();
    assert_eq!((pixels.width, pixels.height), (2, 3), "the sides swap");
    // Turned a quarter clockwise, the top-left is what was bottom-left.
    assert_eq!(at(&pixels, 0, 0), grid.get_pixel(0, 1).0);
    assert_eq!(at(&pixels, 1, 0), grid.get_pixel(0, 0).0);
    // Upside down (3): the top-left is what was bottom-right.
    let pixels = decode(&pictures::png_with_exif(&grid, Some(3))).unwrap();
    assert_eq!((pixels.width, pixels.height), (3, 2));
    assert_eq!(at(&pixels, 0, 0), grid.get_pixel(2, 1).0);
    // No orientation, or "as stored" (1): untouched.
    let pixels = decode(&pictures::png_with_exif(&grid, Some(1))).unwrap();
    assert_eq!(pixels.rgb, grid.into_raw());
}

/// Dark text on a transparent background stays dark on white, not on black.
#[test]
fn transparency_is_flattened_onto_white() {
    let mut rgba = image::RgbaImage::new(2, 1);
    rgba.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
    rgba.put_pixel(1, 0, image::Rgba([20, 30, 40, 255]));
    let pixels = decode(&pictures::png_rgba(&rgba)).unwrap();
    assert_eq!(at(&pixels, 0, 0), [255, 255, 255], "transparent is white");
    assert_eq!(at(&pixels, 1, 0), [20, 30, 40], "opaque is itself");
    let half = flatten_on_white(&image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([0, 0, 0, 128]),
    ));
    assert_eq!(half.get_pixel(0, 0).0, [127, 127, 127]);
}

#[test]
fn an_animation_is_read_by_its_first_frame() {
    let pixels = decode(&pictures::gif([200, 10, 10], [10, 10, 200])).unwrap();
    assert_eq!((pixels.width, pixels.height), (4, 4));
    let [r, _, b] = at(&pixels, 0, 0);
    assert!(r > 150 && b < 60, "the first frame, not the second");
}

/// **A very large picture is scaled down** to the longest side the reader is
/// handed, keeping its proportions.
#[test]
fn a_picture_longer_than_the_limit_is_scaled_down() {
    let wide = image::RgbImage::from_pixel(5_000, 10, image::Rgb([9, 9, 9]));
    let pixels = decode(&pictures::png(&wide)).unwrap();
    assert_eq!((pixels.width, pixels.height), (4_000, 8));
    let tall = fit(image::RgbImage::new(30, 12_000), MAX_SIDE);
    assert_eq!(tall.dimensions(), (10, 4_000));
    let small = fit(image::RgbImage::new(30, 40), MAX_SIDE);
    assert_eq!(
        small.dimensions(),
        (30, 40),
        "a picture that fits is left alone"
    );
    assert_eq!(
        fit(image::RgbImage::new(8_001, 1), MAX_SIDE).dimensions(),
        (4_000, 1)
    );
}

/// **A bomb is refused by its header**, before a pixel is decoded.
#[test]
fn a_picture_declaring_too_many_pixels_is_refused_before_decoding() {
    assert_eq!(
        decode(&pictures::png_declaring(20_000, 20_000)),
        Err(DecodeError::TooLarge)
    );
}

#[test]
fn an_unknown_format_is_unsupported_and_broken_bytes_are_damaged() {
    // HEIC's box header: not one of the six.
    let heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec();
    assert_eq!(decode(&heic), Err(DecodeError::Unsupported));
    assert_eq!(
        decode(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
        Err(DecodeError::Unsupported)
    );
    let mut png = pictures::png(&pictures::grid(3, 2));
    png.truncate(png.len() / 2);
    assert_eq!(decode(&png), Err(DecodeError::Damaged));
}

/// Plan 0.1's render settings: 144 dpi, the longer side at most 4,000 px.
#[test]
fn a_page_renders_at_twice_its_size_unless_that_is_too_long() {
    assert_eq!(page_scale(612.0, 792.0), 2.0, "US letter");
    let a0 = page_scale(2_384.0, 3_370.0);
    assert!(
        (3_370.0 * a0 - 4_000.0).abs() < 0.5,
        "A0 is capped at 4,000 px"
    );
    assert_eq!(page_scale(0.0, 0.0), 2.0);
    assert_eq!(page_scale(f32::NAN, 10.0), 2.0);
}

/// **The bound on what rendering would decode**, from the sizes the text pass
/// read.
#[test]
fn a_page_whose_images_declare_too_much_is_not_rendered() {
    let page = |largest_image, image_pixels| Layer {
        largest_image,
        image_pixels,
        ..Layer::default()
    };
    assert!(page_images_allowed(&page(0, 0)));
    // An A4 page scanned at 600 dpi.
    assert!(page_images_allowed(&page(4_960 * 7_016, 4_960 * 7_016)));
    // Plan 0.1's synthetic bomb.
    assert!(!page_images_allowed(&page(
        20_000 * 20_000,
        20_000 * 20_000
    )));
    assert!(!page_images_allowed(&page(
        MAX_PAGE_IMAGE_PIXELS + 1,
        MAX_PAGE_IMAGE_PIXELS + 1
    )));
    assert!(!page_images_allowed(&page(
        MAX_PAGE_IMAGE_PIXELS,
        MAX_PAGE_IMAGES_PIXELS + 1
    )));
}

#[test]
fn a_rendered_page_drops_its_alpha_and_a_short_buffer_is_refused() {
    let rgba = [1, 2, 3, 255, 4, 5, 6, 255];
    let pixels = rgba_to_rgb(2, 1, &rgba).unwrap();
    assert_eq!(pixels.rgb, [1, 2, 3, 4, 5, 6]);
    assert!(rgba_to_rgb(2, 1, &rgba[..4]).is_none());
    assert!(rgba_to_rgb(0, 0, &[]).is_none());
}

/// Pixels are a person's picture: `Debug` says only the size.
#[test]
fn pixels_print_their_size_only() {
    let pixels = Pixels {
        width: 2,
        height: 1,
        rgb: vec![9; 6],
    };
    assert_eq!(format!("{pixels:?}"), "Pixels(2x1)");
}
