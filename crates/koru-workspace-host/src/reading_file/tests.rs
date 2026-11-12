// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_api_client::ReadingPage;

fn answer() -> ReadingAnswer {
    ReadingAnswer {
        workspace: koru_store_core::ids::WorkspaceId::new(),
        hash: "b3:0123456789abcdef0123".into(),
        document: koru_store_core::ids::DocumentId::new(),
        reader: Some("text".into()),
        generation: 1,
        status: Some("read".into()),
        reason: None,
        source: Some("text-layer".into()),
        confidence: None,
        pages_done: None,
        pages: vec![
            ReadingPage {
                page: 1,
                text: "first line\nsecond line".into(),
            },
            ReadingPage {
                page: 3,
                text: "third page\n".into(),
            },
        ],
        offers: Default::default(),
        failed_here: None,
    }
}

/// The header names everything §9 lists, then each page under its own line,
/// in page order, each page's words exactly as read.
#[test]
fn a_reading_is_a_header_then_its_pages() {
    let text = text("b3:0123", Some(&answer()));
    assert_eq!(
        text,
        "Koru reading (KORU_READ_DEBUG)\n\
         blob: b3:0123\n\
         reader: text\n\
         generation: 1\n\
         status: read\n\
         reason: -\n\
         source: text-layer\n\
         confidence: -\n\
         pages read: 2\n\
         \n--- Page 1 ---\nfirst line\nsecond line\n\
         \n--- Page 3 ---\nthird page\n"
    );
}

/// A failed reading still says why, and a partial one how far it got.
#[test]
fn a_failure_says_why_and_a_partial_says_how_far() {
    let mut failed = answer();
    failed.status = Some("failed".into());
    failed.reason = Some("password".into());
    failed.pages.clear();
    let text = text("b3:aa", Some(&failed));
    assert!(text.contains("status: failed\n"), "{text}");
    assert!(text.contains("reason: password\n"), "{text}");
    assert!(text.contains("pages read: 0\n"), "{text}");
    assert!(!text.contains("--- Page"), "{text}");

    let mut partial = answer();
    partial.status = Some("partial".into());
    partial.pages_done = Some(200);
    partial.confidence = Some(91);
    let text = super::text("b3:aa", Some(&partial));
    assert!(text.contains("pages read: 200\n"), "{text}");
    assert!(text.contains("confidence: 91\n"), "{text}");
}

/// Offered whatever the status, so nothing read yet is a file that says so.
#[test]
fn nothing_read_yet_is_said_in_the_file() {
    assert_eq!(
        text("b3:aa", None),
        "Koru reading (KORU_READ_DEBUG)\nblob: b3:aa\nstatus: not read yet\n"
    );
}

/// `<name>.ocr.txt`: the suffix is added, never swapped for the extension; a
/// picture with no name takes its address.
#[test]
fn the_file_is_named_after_the_item() {
    let file = ReadingTarget::File(koru_core::DocumentId::new());
    assert_eq!(file_name(&file, Some("Lease.pdf")), "Lease.pdf.ocr.txt");
    assert_eq!(file_name(&file, Some("  ")), "file.ocr.txt");
    let picture = ReadingTarget::Picture {
        hash: "b3:0123456789abcdef0123".into(),
    };
    assert_eq!(file_name(&picture, None), "picture-0123456789ab.ocr.txt");
}
