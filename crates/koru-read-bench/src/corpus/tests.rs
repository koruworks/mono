// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn files_are_read_by_extension_whatever_its_case() {
    assert_eq!(
        kind_of(Path::new("a/scan.PDF")),
        Some((Kind::Pdf, "application/pdf"))
    );
    assert_eq!(
        kind_of(Path::new("photo.jpeg")),
        Some((Kind::Picture, "image/jpeg"))
    );
    assert_eq!(
        kind_of(Path::new("photo.JPG")),
        Some((Kind::Picture, "image/jpeg"))
    );
    assert_eq!(
        kind_of(Path::new("page.tif")),
        Some((Kind::Picture, "image/tiff"))
    );
    assert_eq!(
        kind_of(Path::new("notes.md")),
        Some((Kind::Plain, "text/markdown"))
    );
    assert_eq!(kind_of(Path::new("truth.json")), None);
    assert_eq!(kind_of(Path::new("photo.heic")), None);
    assert_eq!(kind_of(Path::new("README")), None);
}
