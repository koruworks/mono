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
