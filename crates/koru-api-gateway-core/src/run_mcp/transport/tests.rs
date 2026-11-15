// SPDX-License-Identifier: MPL-2.0
use super::*;
mod routines;

#[test]
fn bot_initialize_separates_memory_receipts_from_results_and_ordinary_stays_artifact_only() {
    let bot = initialize(&ProtocolVersion::LATEST, true)
        .instructions
        .unwrap();
    for text in [
        "thirteen scoped tools",
        "list_memory/read_memory",
        "remember_memory/correct_memory/forget_memory",
        "never become Run results",
        "original empty seed",
        "same operation and body",
        "create_routine, read_routine_creation, retry_routine_creation and list_routines",
        "initially Paused",
        "observed_token",
        "not current execution or permission authority",
    ] {
        assert!(bot.contains(text), "missing instruction: {text}");
    }
    let ordinary = initialize(&ProtocolVersion::LATEST, false)
        .instructions
        .unwrap();
    assert!(ordinary.contains("artifact endpoint"));
    assert!(!ordinary.contains("memory"));
    assert!(!ordinary.contains("routine"));
}
