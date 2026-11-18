// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::io::Cursor;

#[test]
fn workspace_image_viewer_decodes_pictures_and_leaves_other_files_external() {
    for format in [
        image::ImageFormat::Png,
        image::ImageFormat::Jpeg,
        image::ImageFormat::Gif,
    ] {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::new(12, 8));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        let picture = decode(bytes.get_ref()).unwrap().expect("picture");
        assert_eq!(picture.natural_size(), (12, 8));
    }
    assert!(decode(b"%PDF-1.7").unwrap().is_none());
    assert!(decode(b"plain text").unwrap().is_none());
    assert!(decode(b"\x89PNG\r\n\x1a\ninvalid").is_err());
}
