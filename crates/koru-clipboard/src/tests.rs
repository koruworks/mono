// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The bytes `png` writes decode back to the same size and pixels: what a
/// saved diagram is, and what the clipboard's own normalization relies on.
#[test]
fn png_round_trips_its_pixels() {
    let (width, height) = (3_u32, 2_u32);
    let rgba: Vec<u8> = (0..width * height * 4).map(|i| (i * 7) as u8).collect();
    let bytes = png(width, height, &rgba).expect("encodes");
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "a PNG signature");
    let decoded = image::load_from_memory(&bytes).expect("decodes").to_rgba8();
    assert_eq!(decoded.dimensions(), (width, height));
    assert_eq!(decoded.into_raw(), rgba);
}

/// A byte count that is not `width × height × 4` is refused rather than
/// encoded as a picture of the wrong rows.
#[test]
fn png_refuses_a_byte_count_that_does_not_match() {
    assert_eq!(
        png(2, 2, &[0; 15]).unwrap_err().code,
        ErrorCode::CorruptImage
    );
    assert_eq!(png(0, 2, &[]).unwrap_err().code, ErrorCode::CorruptImage);
}

struct Fixture {
    paths: Vec<PathBuf>,
    bitmap: Option<RgbaBitmap>,
}

impl Backend for Fixture {
    fn bitmap(&self) -> Result<Option<RgbaBitmap>, Error> {
        Ok(self.bitmap.clone())
    }

    fn image_paths(&self) -> Result<Vec<PathBuf>, Error> {
        Ok(self.paths.clone())
    }
}

fn pixel() -> RgbaBitmap {
    RgbaBitmap {
        width: 1,
        height: 1,
        bytes: vec![1, 2, 3, 255],
    }
}

/// A paste offers every copied file, not only pictures; the allowlist is
/// the caller's. Files win over a bitmap that came with them.
#[test]
fn attachments_offer_every_file_before_a_bitmap() {
    let paths = vec![PathBuf::from("report.pdf"), PathBuf::from("shot.png")];
    let clipboard = Clipboard::new(Fixture {
        paths: paths.clone(),
        bitmap: Some(pixel()),
    });
    assert_eq!(
        clipboard.attachments(ImageLimits::default()).unwrap(),
        AttachmentPayload::Files(paths)
    );
}

#[test]
fn attachments_fall_back_to_a_png_bitmap_then_to_nothing() {
    let clipboard = Clipboard::new(Fixture {
        paths: Vec::new(),
        bitmap: Some(pixel()),
    });
    match clipboard.attachments(ImageLimits::default()).unwrap() {
        AttachmentPayload::Bitmap(bitmap) => {
            assert_eq!((bitmap.width, bitmap.height), (1, 1));
            assert!(bitmap.bytes.starts_with(b"\x89PNG"));
        }
        other => panic!("expected a bitmap, got {other:?}"),
    }
    let empty = Clipboard::new(Fixture {
        paths: Vec::new(),
        bitmap: None,
    });
    assert_eq!(
        empty.attachments(ImageLimits::default()).unwrap(),
        AttachmentPayload::Empty
    );
}

#[test]
fn the_attachment_read_runs_on_its_own_thread() {
    let (sender, receiver) = std::sync::mpsc::channel();
    spawn_attachment_read_with(
        Fixture {
            paths: Vec::new(),
            bitmap: Some(pixel()),
        },
        ImageLimits::default(),
        move |result| {
            let _ = sender.send((std::thread::current().name().map(str::to_owned), result));
        },
    )
    .unwrap()
    .join()
    .unwrap();
    let (name, result) = receiver.recv().unwrap();
    assert_eq!(name.as_deref(), Some("koru-clipboard attachment read"));
    assert!(matches!(result, Ok(AttachmentPayload::Bitmap(_))));
}
