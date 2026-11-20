// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn reading_diagnostics_distinguishes_unread_from_empty_and_preserves_words() {
    let unread = format("sample.pdf".into(), None, Lang::En);
    assert!(unread.text.contains("Not read yet"));
    let mut answer: ReadingAnswer = serde_json::from_value(serde_json::json!({
        "workspace": koru_store_core::ids::WorkspaceId::new(),
        "document": koru_store_core::ids::DocumentId::new(),
        "hash": "b3:private", "generation": 1, "status": "empty", "pages": []
    }))
    .unwrap();
    assert!(
        format("sample.pdf".into(), Some(&answer), Lang::En)
            .text
            .contains("no text was found")
    );
    answer.pages.push(koru_api_client::ReadingPage {
        page: 3,
        text: "Mikaël 李\nNumber 16".into(),
    });
    answer.status = Some("partial".into());
    let text = format("sample.pdf".into(), Some(&answer), Lang::En).text;
    assert!(text.contains("Partly read"));
    assert!(text.contains("Page 3\nMikaël 李\nNumber 16"));
    assert!(!text.contains("b3:private"));
    answer.failed_here = Some("crashed".into());
    assert!(
        format("sample.pdf".into(), Some(&answer), Lang::En)
            .text
            .contains("Reading failed")
    );
    answer.pages[0].text = "李".repeat(600);
    let details = format("sample.pdf".into(), Some(&answer), Lang::En);
    assert!(
        details.text.contains(&"李".repeat(600)),
        "copy/save retain original line breaks"
    );
    assert!(details.lines.iter().all(|line| line.chars().count() <= 120));
}

#[test]
fn a_pictures_text_is_its_pages_or_the_selected_correction() {
    assert_eq!(picture_text(None), None, "no reading, no text");
    let mut answer: ReadingAnswer = serde_json::from_value(serde_json::json!({
        "workspace": koru_store_core::ids::WorkspaceId::new(),
        "document": koru_store_core::ids::DocumentId::new(),
        "hash": "b3:picture", "generation": 1, "status": "read", "pages": []
    }))
    .unwrap();
    assert_eq!(picture_text(Some(&answer)), None, "read, but nothing in it");
    answer.pages = vec![
        koru_api_client::ReadingPage {
            page: 1,
            text: "TOTAL 42\nThank you\n".into(),
        },
        koru_api_client::ReadingPage {
            page: 2,
            text: "  \n".into(),
        },
        koru_api_client::ReadingPage {
            page: 3,
            text: "Mikaël 李".into(),
        },
    ];
    assert_eq!(
        picture_text(Some(&answer)).as_deref(),
        Some("TOTAL 42\nThank you\n\nMikaël 李"),
        "pages in order, a blank line between, blank pages skipped"
    );
    answer.corrections = koru_store_wire::readings::ReadingCorrections {
        selected: Some("b".into()),
        versions: vec![
            koru_store_wire::readings::ReadingCorrectionVersion {
                id: "a".into(),
                text: Some("not this one".into()),
            },
            koru_store_wire::readings::ReadingCorrectionVersion {
                id: "b".into(),
                text: Some("TOTAL 24".into()),
            },
        ],
    };
    assert_eq!(picture_text(Some(&answer)).as_deref(), Some("TOTAL 24"));
    answer.corrections.versions[1].text = None;
    assert_eq!(
        picture_text(Some(&answer)).as_deref(),
        Some("TOTAL 42\nThank you\n\nMikaël 李"),
        "a correction reset to the generated words"
    );
    answer.corrections.versions[1].text = Some(" ".into());
    assert_eq!(picture_text(Some(&answer)), None, "corrected to blank");
}
