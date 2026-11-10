// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn every_refusal_code_is_a_distinct_bot_code_that_round_trips() {
    let mut codes = std::collections::BTreeSet::new();
    for refusal in BotInputRefusal::ALL {
        // The owner reports any `bot_*` port code as non-retryable.
        assert!(refusal.code().starts_with("bot_input_"));
        assert!(codes.insert(refusal.code()));
        assert_eq!(BotInputRefusal::from_code(refusal.code()), Some(refusal));
    }
    assert_eq!(BotInputRefusal::from_code("port"), None);
    assert_eq!(BotInputRefusal::from_code("bot_fenced"), None);
}

#[test]
fn only_a_definite_answer_about_the_input_is_a_refusal() {
    use ErrorCode as E;
    let refusals = [
        (E::InputTooLarge, BotInputRefusal::TooLarge),
        (E::UnsupportedMedia, BotInputRefusal::Pictures),
        (E::InvalidImage, BotInputRefusal::Pictures),
        (E::AttachmentLimit, BotInputRefusal::Pictures),
        (E::AttachmentNotFound, BotInputRefusal::Pictures),
        (E::CapabilityNotNegotiated, BotInputRefusal::Capability),
        (E::DeliveryUnsupported, BotInputRefusal::Capability),
        (E::ProviderModelUnavailable, BotInputRefusal::Model),
        (E::InvalidRequest, BotInputRefusal::Invalid),
        (E::InvalidTransition, BotInputRefusal::Invalid),
        (E::IdempotencyConflict, BotInputRefusal::Invalid),
    ];
    for (code, refusal) in &refusals {
        assert_eq!(BotInputRefusal::of_session(code), Some(*refusal), "{code}");
        assert_eq!(
            BotInputRefusal::of_session_code(code.as_str()),
            Some(*refusal)
        );
    }
    // No answer, or an answer about the session rather than the input, may
    // have begun the turn or may pass: it is never classified as a refusal.
    for code in [
        E::Backpressure,
        E::StaleLease,
        E::ControllerRequired,
        E::ControllerConflict,
        E::SessionExited,
        E::SessionNotFound,
        E::HostUnavailable,
        E::ProviderUnavailable,
        E::ProviderFailed,
        E::DeliveryFailed,
        E::Internal,
        E::Unknown("fixture".into()),
    ] {
        assert_eq!(BotInputRefusal::of_session(&code), None, "{code}");
    }
    // A session port's transport codes are not session codes at all.
    for transport in ["disconnected", "timed_out", "unavailable", "port", ""] {
        assert_eq!(BotInputRefusal::of_session_code(transport), None);
    }
}

#[test]
fn only_a_picture_or_size_refusal_concerns_the_pictures() {
    let concerned: Vec<_> = BotInputRefusal::ALL
        .into_iter()
        .filter(|refusal| refusal.concerns_pictures())
        .collect();
    assert_eq!(
        concerned,
        [BotInputRefusal::TooLarge, BotInputRefusal::Pictures]
    );
}
