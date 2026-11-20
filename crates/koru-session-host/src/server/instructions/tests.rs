// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn only_a_manual_managed_nondelegated_session_captures_orientation() {
    let managed = SessionMode::Managed {
        provider: "codex".parse().unwrap(),
    };
    assert!(eligible(&managed, SessionOrigin::Manual, false));
    assert!(!eligible(&managed, SessionOrigin::Manual, true));
    assert!(!eligible(
        &SessionMode::RawPty,
        SessionOrigin::Manual,
        false
    ));
    let chat = SessionOrigin::Chat {
        chat_id: koru_session_protocol::ChatOriginId::mint(),
    };
    assert!(!eligible(&managed, chat, false));
}
