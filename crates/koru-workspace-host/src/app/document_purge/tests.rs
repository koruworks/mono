// SPDX-License-Identifier: MPL-2.0
use super::decision_for_dialog;
use koru_workspace::DocumentPurgeDecision::{Delete, Keep};
use rfd::MessageDialogResult::{Cancel, Custom, No, Ok, Yes};

#[test]
fn native_and_custom_answers_choose_the_same_durable_decision() {
    for answer in [Yes, Custom("Keep".into())] {
        assert_eq!(decision_for_dialog(answer, "Keep", "Delete"), Some(Keep));
    }
    for answer in [No, Custom("Delete".into())] {
        assert_eq!(decision_for_dialog(answer, "Keep", "Delete"), Some(Delete));
    }
}

#[test]
fn dismissal_or_an_unrecognized_answer_never_releases_the_source() {
    for answer in [Cancel, Ok, Custom("Cancel".into()), Custom("".into())] {
        assert_eq!(decision_for_dialog(answer, "Keep", "Delete"), None);
    }
}
