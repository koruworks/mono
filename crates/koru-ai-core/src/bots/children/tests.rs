// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn reply_preview_is_bounded_utf8_and_debug_never_contains_provider_words() {
    let id = MessageId::new();
    assert!(BotReplyPreview::new(id, " \n").is_none());
    let text = format!(
        "sensitive {}",
        "\u{1f642}".repeat(BotReplyPreview::MAX_BYTES)
    );
    let preview = BotReplyPreview::new(id, &text).unwrap();
    assert_eq!(preview.message, id);
    assert!(preview.markdown.len() <= BotReplyPreview::MAX_BYTES);
    assert!(preview.truncated && text.starts_with(&preview.markdown));
    assert!(!format!("{preview:?}").contains("sensitive"));
    let restored: BotReplyPreview =
        serde_json::from_value(serde_json::to_value(&preview).unwrap()).unwrap();
    assert_eq!(restored, preview);
}
