// SPDX-License-Identifier: MPL-2.0

use super::*;

fn refusal(code: &str) -> ToolError {
    ToolError {
        code: code.into(),
        message: format!("refused with {code}"),
        details: Some(json!({ "own": true })),
    }
}

fn edit_recovery(_: &[Effect]) -> Recovery {
    Recovery::sentence("The tags were created; the edit was not saved.")
}

/// An edit's two effects, the system document first.
fn an_edit() -> Ledger {
    let mut ledger = Ledger::new(Uuid::nil());
    ledger.plan(EffectKind::System, Some("system".into()));
    ledger.plan(EffectKind::Document, Some("page".into()));
    ledger
}

fn states(error: &ToolError) -> Vec<(String, String)> {
    error.details.as_ref().unwrap()["effects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|effect| {
            (
                effect["effect"].as_str().unwrap().to_owned(),
                effect["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pair(effect: &str, state: &str) -> (String, String) {
    (effect.to_owned(), state.to_owned())
}

/// **A refusal before anything landed keeps its own code and details**: a
/// local Store error answers exactly as it did before effects existed.
#[test]
fn nothing_saved_answers_the_refusals_own_code() {
    let mut ledger = an_edit();
    let error = ledger.failed(
        &[EffectKind::System],
        refusal("storage_failed"),
        edit_recovery,
    );
    assert_eq!(error.code, "storage_failed");
    assert_eq!(error.details, Some(json!({ "own": true })));
}

/// …unless the refusal has no code of its own: then it is `edit_not_saved`,
/// with the envelope's details beside the refusal's.
#[test]
fn nothing_saved_without_a_code_of_its_own_is_edit_not_saved_with_the_envelope() {
    let operation = Uuid::new_v4();
    let mut ledger = Ledger::new(operation);
    ledger.plan(EffectKind::Document, Some("page".into()));
    let error = ledger.failed(
        &[EffectKind::Document],
        refusal(EDIT_NOT_SAVED),
        edit_recovery,
    );
    assert_eq!(error.code, EDIT_NOT_SAVED);
    let details = error.details.as_ref().unwrap();
    assert_eq!(details["operation_id"], operation.to_string());
    assert_eq!(details["own"], true);
    assert_eq!(states(&error), [pair("document", "not_saved")]);
}

/// **A saved prefix is `partially_saved`**, every planned effect listed with
/// its state, the saved one included, and the tool's recovery sentence.
#[test]
fn a_saved_prefix_is_partially_saved_with_every_effect_and_the_recovery() {
    let mut ledger = an_edit();
    ledger.saved(EffectKind::System);
    let error = ledger.failed(
        &[EffectKind::Document],
        refusal("storage_failed"),
        edit_recovery,
    );
    assert_eq!(error.code, PARTIALLY_SAVED);
    assert_eq!(
        error.message,
        "The tags were created; the edit was not saved."
    );
    assert_eq!(
        states(&error),
        [pair("system", "saved"), pair("document", "not_saved")]
    );
    let details = error.details.unwrap();
    assert_eq!(details["effects"][0]["document_id"], "system");
    assert!(details.get("own").is_none(), "the cause's details stay out");
}

/// **Any `unknown` wins**, whatever else landed, with the contract's message.
#[test]
fn an_unconfirmed_submission_is_save_status_unknown_with_the_exact_message() {
    let mut ledger = an_edit();
    let error = ledger.failed(
        &[EffectKind::System],
        refusal(SAVE_STATUS_UNKNOWN),
        edit_recovery,
    );
    assert_eq!(error.code, SAVE_STATUS_UNKNOWN);
    assert_eq!(error.message, SAVE_STATUS_UNKNOWN_MESSAGE);
    assert!(error.message.starts_with("Koru could not confirm"));
    assert_eq!(
        states(&error),
        [pair("system", "unknown"), pair("document", "not_saved")]
    );

    let mut ledger = an_edit();
    ledger.saved(EffectKind::System);
    let error = ledger.failed(
        &[EffectKind::Document],
        refusal("durability_timeout"),
        edit_recovery,
    );
    assert_eq!(
        error.code, SAVE_STATUS_UNKNOWN,
        "a local timeout proves nothing"
    );
}

/// A port call covering two effects reports each, and the report names the
/// unit the call could not have known; what the call adds to every envelope
/// is there too.
#[test]
fn a_port_reports_each_effect_of_one_call() {
    let mut ledger = Ledger::new(Uuid::nil());
    ledger.plan(EffectKind::Document, None);
    ledger.plan(EffectKind::Manifest, Some("manifest".into()));
    ledger.always("idempotency_key", json!("key"));
    let report = refusal("cloud_refused").with_effects(&[
        Effect {
            effect: EffectKind::Document,
            document_id: Some("candidate".into()),
            state: EffectState::Saved,
        },
        Effect {
            effect: EffectKind::Manifest,
            document_id: Some("manifest".into()),
            state: EffectState::NotSaved,
        },
    ]);
    let error = ledger.failed(
        &[EffectKind::Document, EffectKind::Manifest],
        report,
        |_| Recovery::sentence("listed later"),
    );
    assert_eq!(error.code, PARTIALLY_SAVED);
    let details = error.details.unwrap();
    assert_eq!(details["effects"][0]["document_id"], "candidate");
    assert_eq!(details["effects"][0]["state"], "saved");
    assert_eq!(details["idempotency_key"], "key");

    // Without a report, both take the refusal's state.
    let mut ledger = Ledger::new(Uuid::nil());
    ledger.plan(EffectKind::Document, None);
    ledger.plan(EffectKind::Manifest, None);
    let error = ledger.failed(
        &[EffectKind::Document, EffectKind::Manifest],
        refusal("storage_failed"),
        |_| Recovery::sentence("unused"),
    );
    assert_eq!(error.code, "storage_failed");
}

/// The wire names are the contract's.
#[test]
fn effect_and_state_names_are_the_contracts() {
    let names: Vec<Value> = [
        EffectKind::System,
        EffectKind::Document,
        EffectKind::Manifest,
        EffectKind::DestinationSeed,
        EffectKind::Destination,
        EffectKind::Source,
    ]
    .iter()
    .map(|kind| json!(kind))
    .collect();
    assert_eq!(
        names,
        [
            "system",
            "document",
            "manifest",
            "destination_seed",
            "destination",
            "source"
        ]
    );
    assert_eq!(
        json!([
            EffectState::Saved,
            EffectState::NotSaved,
            EffectState::Unknown
        ]),
        json!(["saved", "not_saved", "unknown"])
    );
}
