// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn actual_body_is_bounded_even_without_a_length_hint() {
    let mut reader = std::io::Cursor::new(vec![0; 20]);
    assert_eq!(
        read_bounded(&mut reader, 8).unwrap_err().code,
        "payload_too_large"
    );
    assert_eq!(reader.position(), 9);
    assert_eq!(read_bounded(&b"exact"[..], 5).unwrap(), b"exact");
}
