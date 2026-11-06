// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn a_duration_is_sixty_to_nine_hundred_seconds_and_five_minutes_by_default() {
    assert_eq!(claim_duration_millis(None), Some(300_000));
    assert_eq!(claim_duration_millis(Some(60)), Some(60_000));
    assert_eq!(claim_duration_millis(Some(900)), Some(900_000));
    assert_eq!(claim_duration_millis(Some(59)), None);
    assert_eq!(claim_duration_millis(Some(901)), None);
    assert_eq!(claim_duration_millis(Some(0)), None);
    assert_eq!(claim_duration_millis(Some(u32::MAX)), None);
}

#[test]
fn a_self_declared_text_is_trimmed_and_bounded_in_characters() {
    assert_eq!(
        claim_text("  tidy the outline ", MAX_PURPOSE_CHARS).as_deref(),
        Some("tidy the outline")
    );
    assert_eq!(claim_text("   ", MAX_PURPOSE_CHARS), None);
    assert_eq!(claim_text("", MAX_AGENT_LABEL_CHARS), None);
    assert_eq!(claim_text("line\nbreak", MAX_AGENT_LABEL_CHARS), None);
    // Characters, not bytes: 64 four-byte characters fit a label.
    let wide = "\u{1F600}".repeat(MAX_AGENT_LABEL_CHARS);
    assert_eq!(
        claim_text(&wide, MAX_AGENT_LABEL_CHARS).as_deref(),
        Some(&*wide)
    );
    assert_eq!(claim_text(&format!("{wide}x"), MAX_AGENT_LABEL_CHARS), None);
}

#[test]
fn retry_after_rounds_up_and_is_never_zero() {
    assert_eq!(retry_after_seconds(10_000, 0), 10);
    assert_eq!(retry_after_seconds(10_001, 0), 11);
    assert_eq!(retry_after_seconds(1, 0), 1);
    assert_eq!(retry_after_seconds(0, 5), 1);
}

#[test]
fn requests_never_print_a_claim_digest() {
    let request = ClaimRequest {
        connection_id: "11111111-1111-4111-8111-111111111111".parse().unwrap(),
        koru_identity_id: "22222222-2222-4222-8222-222222222222".parse().unwrap(),
        workspace_id: WorkspaceId::new(),
        document_id: DocumentId::new(),
        claim_hash: [0xab; 32],
        renew: false,
        purpose: Some("secret plan".into()),
        agent_label: None,
        duration_millis: 300_000,
    };
    let printed = format!("{request:?}");
    assert!(!printed.contains("171"), "{printed}");
    assert!(!printed.contains("secret plan"), "{printed}");
    let check = ClaimCheckRequest {
        connection_id: request.connection_id,
        koru_identity_id: request.koru_identity_id,
        workspace_id: request.workspace_id,
        presented: vec![[0xab; 32]],
    };
    assert!(!format!("{check:?}").contains("171"));
}
