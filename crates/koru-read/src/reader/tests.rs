// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn a_picture_is_exactly_width_by_height_rgb() {
    let pixels = vec![0u8; 4 * 3 * 3];
    let picture = Picture::rgb8(4, 3, &pixels).expect("a 4x3 picture");
    assert_eq!((picture.width(), picture.height()), (4, 3));
    assert_eq!(picture.pixels().len(), 36);

    assert!(Picture::rgb8(4, 3, &pixels[..35]).is_none(), "short");
    assert!(Picture::rgb8(4, 2, &pixels).is_none(), "long");
    assert!(Picture::rgb8(0, 0, &[]).is_none(), "empty");
    assert!(
        Picture::rgb8(u32::MAX, u32::MAX, &pixels).is_none(),
        "overflow"
    );
}

/// A stand-in, to prove the trait is implementable with nothing but this crate
/// and can move to the worker's reading thread.
struct Fixed;

impl Reader for Fixed {
    fn describe(&self) -> ReaderInfo {
        ReaderInfo {
            reader: ReaderId::parse("fixed").unwrap(),
            version: "0".into(),
        }
    }

    fn read(&mut self, picture: &Picture<'_>) -> Result<Recognized, ReadError> {
        if picture.width() < 2 {
            return Err(ReadError {
                code: FailureCode::Unsupported,
                message: "too narrow".into(),
            });
        }
        Ok(Recognized {
            text: PageText::new("line one\nline two"),
            confidence: 90,
        })
    }
}

#[test]
fn a_reader_moves_to_the_reading_thread_and_answers_or_refuses_with_a_code() {
    let mut reader: Box<dyn Reader> = Box::new(Fixed);
    // Two pixels, wide or tall.
    let pixels = [0u8; 6];
    let answer = std::thread::spawn(move || {
        let wide = Picture::rgb8(2, 1, &pixels).unwrap();
        let narrow = Picture::rgb8(1, 2, &pixels).unwrap();
        (reader.read(&wide), reader.read(&narrow), reader.describe())
    })
    .join()
    .unwrap();
    assert_eq!(answer.0.unwrap().confidence, 90);
    assert_eq!(answer.1.unwrap_err().code, FailureCode::Unsupported);
    assert_eq!(answer.2.reader.as_str(), "fixed");
}
