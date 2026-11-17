// SPDX-License-Identifier: MPL-2.0

//! The classification on its own, over answers written out by hand: the
//! cases a well-behaved server never produces, where the rule is "when the
//! answer is not the contract's, the write is unknown".

use std::collections::HashSet;

use serde_json::{Value, json};
use uuid::Uuid;

use super::*;

const OPERATION: &str = "0b9b1c9e-6a5b-4d0c-9a57-1f2f3a4b5c6d";
const DOCUMENT: &str = "5c0f7a52-8a0e-4f0e-9d5e-3b1c2d4e5f60";
const OTHER: &str = "9a1b2c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d";

fn edit() -> Sent {
    Sent {
        tool: Some(WriteTool::EditDocument),
        create: None,
    }
}

fn creation(key: Uuid) -> Sent {
    Sent {
        tool: Some(WriteTool::CreateDocument),
        create: Some(PendingCreate::new(key, json!({ "title": "Sent" }))),
    }
}

fn envelope(code: &str, effects: Value) -> Value {
    json!({
        "code": code,
        "message": "Words from the server.",
        "details": { "operation_id": OPERATION, "effects": effects },
    })
}

fn effect(effect: &str, state: &str) -> Value {
    json!({ "effect": effect, "document_id": DOCUMENT, "state": state })
}

#[track_caller]
fn unknown_because(outcome: WriteOutcome) -> UnknownCause {
    match outcome {
        WriteOutcome::Unknown(unknown) => unknown.cause,
        other => panic!("expected unknown, got {other:?}"),
    }
}

#[test]
fn a_success_without_what_a_success_must_carry_is_unknown_not_saved() {
    // An edit's success carries a revision.
    for answer in [
        json!({}),
        json!({ "revision": "" }),
        json!({ "revision": 7 }),
    ] {
        let outcome = classify_write(edit(), ToolAnswer::Success(answer));
        assert_eq!(unknown_because(outcome), UnknownCause::AnswerNotUnderstood);
    }
    let outcome = classify_write(edit(), ToolAnswer::Success(json!({ "revision": "0a" })));
    assert!(matches!(outcome, WriteOutcome::Saved(saved) if saved.revision.is_some()));

    // An extraction's carries both documents' revisions.
    let extract = || Sent {
        tool: Some(WriteTool::ExtractToDocument),
        create: None,
    };
    let half = json!({ "document_id": DOCUMENT, "document_revision": "0a" });
    assert_eq!(
        unknown_because(classify_write(extract(), ToolAnswer::Success(half))),
        UnknownCause::AnswerNotUnderstood
    );
    let whole =
        json!({ "document_id": DOCUMENT, "document_revision": "0a", "source_revision": "0b" });
    assert!(matches!(
        classify_write(extract(), ToolAnswer::Success(whole)),
        WriteOutcome::Saved(saved) if saved.source_revision.is_some()
    ));

    // A claim's carries the handle: a claim nobody can present is not one.
    let claim = || Sent {
        tool: Some(WriteTool::ClaimDocument),
        create: None,
    };
    assert_eq!(
        unknown_because(classify_write(
            claim(),
            ToolAnswer::Success(json!({ "expires_at": 5 }))
        )),
        UnknownCause::AnswerNotUnderstood
    );
}

#[test]
fn a_creation_answered_under_another_key_is_unknown() {
    let key = Uuid::new_v4();
    let answer = |answered: Option<String>| {
        let mut answer = json!({ "document_id": DOCUMENT, "revision": "0a" });
        if let Some(answered) = answered {
            answer["idempotency_key"] = json!(answered);
        }
        ToolAnswer::Success(answer)
    };
    let outcome = classify_write(creation(key), answer(Some(Uuid::new_v4().to_string())));
    let WriteOutcome::Unknown(unknown) = outcome else {
        panic!("another key answered another request");
    };
    assert_eq!(unknown.cause, UnknownCause::AnswerNotUnderstood);
    assert!(
        matches!(&unknown.recovery, Recovery::FinishCreate(pending) if pending.idempotency_key == key),
        "and the way out is still the key that was sent"
    );

    for answered in [Some(key.to_string()), None] {
        let outcome = classify_write(creation(key), answer(answered));
        assert!(
            matches!(outcome, WriteOutcome::Saved(saved) if saved.idempotency_key == Some(key))
        );
    }
}

#[test]
fn partially_saved_is_believed_only_as_a_saved_prefix() {
    let believed = envelope(
        "partially_saved",
        json!([
            effect("destination", "saved"),
            effect("source", "not_saved")
        ]),
    );
    let WriteOutcome::PartiallySaved(partial) =
        classify_write(edit(), ToolAnswer::Failure(believed))
    else {
        panic!("a saved prefix");
    };
    assert_eq!(partial.operation_id.to_string(), OPERATION);
    assert_eq!(partial.recovery, Recovery::ReadFirst);

    let not_the_contracts = [
        // No effect saved.
        json!([effect("document", "not_saved")]),
        // Every effect saved.
        json!([effect("document", "saved"), effect("manifest", "saved")]),
        // Not a prefix.
        json!([effect("document", "not_saved"), effect("manifest", "saved")]),
        // An unknown under the wrong code.
        json!([effect("document", "saved"), effect("manifest", "unknown")]),
        // A state and an effect nobody defined.
        json!([effect("document", "saved"), effect("manifest", "pending")]),
        json!([effect("document", "saved"), effect("index", "not_saved")]),
        json!([]),
        json!("none"),
    ];
    for effects in not_the_contracts {
        let outcome = classify_write(
            edit(),
            ToolAnswer::Failure(envelope("partially_saved", effects.clone())),
        );
        assert_eq!(
            unknown_because(outcome),
            UnknownCause::AnswerNotUnderstood,
            "{effects}"
        );
    }
    // The envelope without its operation id.
    let outcome = classify_write(
        edit(),
        ToolAnswer::Failure(json!({
            "code": "partially_saved",
            "details": { "effects": [effect("document", "saved"), effect("manifest", "not_saved")] },
        })),
    );
    assert_eq!(unknown_because(outcome), UnknownCause::AnswerNotUnderstood);
}

#[test]
fn save_status_unknown_is_unknown_whatever_else_it_says() {
    for error in [
        envelope(
            "save_status_unknown",
            json!([effect("document", "unknown")]),
        ),
        envelope("save_status_unknown", json!([effect("document", "saved")])),
        json!({ "code": "save_status_unknown" }),
    ] {
        let outcome = classify_write(edit(), ToolAnswer::Failure(error));
        assert_eq!(
            unknown_because(outcome),
            UnknownCause::ServerCouldNotConfirm
        );
    }
    let WriteOutcome::Unknown(unknown) = classify_write(
        edit(),
        ToolAnswer::Failure(json!({ "code": "save_status_unknown" })),
    ) else {
        panic!("unknown");
    };
    assert!(unknown.operation_id.is_none() && unknown.effects.is_empty());
    assert!(
        unknown.answer().is_some(),
        "the answer is kept for a caller that hands it on"
    );
}

#[test]
fn a_refusal_of_its_own_is_not_saved_unless_its_effects_say_otherwise() {
    let not_saved = |error: Value| match classify_write(edit(), ToolAnswer::Failure(error)) {
        WriteOutcome::NotSaved(not_saved) => not_saved.reason,
        other => panic!("expected not saved, got {other:?}"),
    };
    let plain = not_saved(json!({ "code": "revision_conflict", "message": "Stale." }));
    assert!(matches!(&plain, Refusal::Tool(tool) if tool.code() == "revision_conflict"));
    let listed = not_saved(envelope(
        "edit_not_saved",
        json!([effect("document", "not_saved")]),
    ));
    assert!(matches!(&listed, Refusal::Tool(tool) if tool.code() == "edit_not_saved"));

    // A not-saved code beside an effect that is saved or unknown.
    for state in ["saved", "unknown"] {
        let outcome = classify_write(
            edit(),
            ToolAnswer::Failure(envelope(
                "edit_not_saved",
                json!([effect("document", state)]),
            )),
        );
        assert_eq!(unknown_because(outcome), UnknownCause::AnswerNotUnderstood);
    }
    // An error object with no code is not one.
    let outcome = classify_write(
        edit(),
        ToolAnswer::Failure(json!({ "message": "Something happened." })),
    );
    assert_eq!(unknown_because(outcome), UnknownCause::AnswerNotUnderstood);
}

#[test]
fn only_a_proof_is_not_saved() {
    assert!(matches!(
        classify_write(edit(), ToolAnswer::NotSent),
        WriteOutcome::NotSaved(NotSaved {
            reason: Refusal::NotSent,
            ..
        })
    ));
    assert!(matches!(
        classify_write(edit(), ToolAnswer::Refused(Refusal::TokenExpired)),
        WriteOutcome::NotSaved(NotSaved {
            reason: Refusal::TokenExpired,
            ..
        })
    ));
    assert_eq!(
        unknown_because(classify_write(edit(), ToolAnswer::NoAnswer)),
        UnknownCause::NoAnswer
    );
    assert_eq!(
        unknown_because(classify_write(edit(), ToolAnswer::NotUnderstood)),
        UnknownCause::AnswerNotUnderstood
    );

    // The two refusals that prove nothing are a read's only.
    for refusal in [Refusal::NoAnswer, Refusal::AnswerNotUnderstood] {
        assert!(!refusal.proves_nothing_ran());
    }
    assert!(
        Refusal::RevisionRefused {
            supported: Vec::new()
        }
        .proves_nothing_ran()
    );
}

#[test]
fn only_a_creation_is_offered_again() {
    let key = Uuid::new_v4();
    let unconfirmed = || {
        ToolAnswer::Failure(envelope(
            "save_status_unknown",
            json!([effect("document", "unknown")]),
        ))
    };
    let WriteOutcome::Unknown(unknown) = classify_write(creation(key), unconfirmed()) else {
        panic!("unknown");
    };
    assert!(
        matches!(unknown.recovery, Recovery::FinishCreate(pending) if pending.idempotency_key == key)
    );

    for tool in WriteTool::ALL {
        if tool == WriteTool::CreateDocument {
            continue;
        }
        for answer in [
            unconfirmed(),
            ToolAnswer::NoAnswer,
            ToolAnswer::NotUnderstood,
        ] {
            let sent = Sent {
                tool: Some(tool),
                create: None,
            };
            let WriteOutcome::Unknown(unknown) = classify_write(sent, answer) else {
                panic!("unknown");
            };
            assert_eq!(unknown.recovery, Recovery::ReadFirst, "{tool:?} has no key");
        }
    }
}

#[test]
fn a_tools_error_becomes_the_refusal_its_code_names() {
    let of = |code: &str| refusal_of_tool_error(json!({ "code": code, "message": "Quoted text." }));
    assert_eq!(of("authorization_revoked"), Refusal::ConnectionRevoked);
    assert_eq!(of("cloud_subscription_ended"), Refusal::SubscriptionEnded);
    assert_eq!(
        of("authorization_unavailable"),
        Refusal::AuthorizationUnavailable
    );
    assert_eq!(of("tool_not_available"), Refusal::ToolNotAvailable);
    assert_eq!(of("insufficient_scope"), Refusal::InsufficientScope);

    // A code is printed, so it is only ever a short token.
    let long = "x".repeat(65);
    for strange in ["Has Spaces", "", "a\nb", long.as_str()] {
        let refusal = of(strange);
        assert!(
            matches!(&refusal, Refusal::Tool(tool) if tool.code() == "unrecognised"),
            "{strange:?}"
        );
    }
    // The server's message is never part of what is printed.
    let refusal = of("revision_conflict");
    assert_eq!(refusal.to_string(), "tool_refused: revision_conflict");
    assert!(!format!("{refusal:?}").contains("Quoted"));

    let claimed = refusal_of_tool_error(json!({
        "code": "document_claimed",
        "details": {
            "document_id": OTHER, "expires_at": 1_800_000_300_000_i64, "retry_after_seconds": 300,
            "purpose": "Quoted purpose", "holder": { "connection_name": "Quoted name" },
        },
    }));
    let Refusal::Tool(tool) = &claimed else {
        panic!("a tool's refusal");
    };
    let what = tool.claimed.expect("the claim in the way");
    assert_eq!(
        what.document_id.map(|id| id.to_string()).as_deref(),
        Some(OTHER)
    );
    assert_eq!(
        (what.expires_at, what.retry_after_seconds),
        (Some(1_800_000_300_000), Some(300))
    );
    assert!(!format!("{claimed:?}").contains("Quoted"));
}

#[test]
fn every_refusal_has_its_own_code() {
    let refusals = [
        Refusal::TokenExpired,
        Refusal::ConnectionRevoked,
        Refusal::SubscriptionEnded,
        Refusal::RevisionRefused {
            supported: Vec::new(),
        },
        Refusal::InsufficientScope,
        Refusal::Forbidden,
        Refusal::AuthorizationUnavailable,
        Refusal::ToolNotAvailable,
        refusal_of_tool_error(json!({ "code": "revision_conflict" })),
        Refusal::RequestRefused { rpc_code: -32020 },
        Refusal::NotSent,
        Refusal::NoAnswer,
        Refusal::AnswerNotUnderstood,
        Refusal::InvalidCall(InvalidCall::ToolName),
    ];
    let codes: HashSet<&str> = refusals.iter().map(Refusal::code).collect();
    assert_eq!(codes.len(), refusals.len());
    // The four the plan names, spelled out: these strings are stored.
    assert_eq!(Refusal::TokenExpired.code(), "token_expired");
    assert_eq!(Refusal::ConnectionRevoked.code(), "connection_revoked");
    assert_eq!(Refusal::SubscriptionEnded.code(), "subscription_ended");
    assert_eq!(
        Refusal::RevisionRefused {
            supported: Vec::new()
        }
        .code(),
        "revision_refused"
    );
    assert_eq!(
        [
            UnknownCause::ServerCouldNotConfirm.code(),
            UnknownCause::NoAnswer.code(),
            UnknownCause::AnswerNotUnderstood.code()
        ],
        [
            "server_could_not_confirm",
            "no_answer",
            "answer_not_understood"
        ]
    );
}

#[test]
fn coverage_is_complete_only_when_the_server_says_so_and_its_counts_agree() {
    let coverage = |searched: u64, eligible: u64, partial: bool| {
        Coverage::of_search(&json!({
            "hits": [],
            "search_coverage": {
                "content_documents_searched": searched, "eligible_documents": eligible, "partial": partial,
            },
        }))
    };
    let counts = |searched, eligible| SearchCoverage {
        content_documents_searched: searched,
        eligible_documents: eligible,
    };
    assert_eq!(coverage(3, 3, false), Coverage::Complete(counts(3, 3)));
    assert_eq!(coverage(1, 3, true), Coverage::Partial(counts(1, 3)));
    assert_eq!(
        coverage(1, 3, false),
        Coverage::Partial(counts(1, 3)),
        "the counts disagree with the flag"
    );
    assert_eq!(
        coverage(3, 3, true),
        Coverage::Partial(counts(3, 3)),
        "the server said partial"
    );

    // No coverage is not complete coverage.
    assert_eq!(
        Coverage::of_search(&json!({ "hits": [] })),
        Coverage::NotReported
    );
    assert_eq!(
        Coverage::of_search(&json!({ "search_coverage": { "partial": false } })),
        Coverage::NotReported
    );
    assert_eq!(
        Coverage::of_search(&json!({ "search_coverage": {
            "content_documents_searched": 1, "eligible_documents": 1,
        } })),
        Coverage::NotReported
    );
}

#[test]
fn the_shared_tool_layers_effects_read_as_this_crates() {
    use koru_mcp_server::effects as theirs;
    let kinds = [
        (theirs::EffectKind::System, EffectKind::System),
        (theirs::EffectKind::Document, EffectKind::Document),
        (theirs::EffectKind::Manifest, EffectKind::Manifest),
        (
            theirs::EffectKind::DestinationSeed,
            EffectKind::DestinationSeed,
        ),
        (theirs::EffectKind::Destination, EffectKind::Destination),
        (theirs::EffectKind::Source, EffectKind::Source),
    ];
    let states = [
        (theirs::EffectState::Saved, EffectState::Saved),
        (theirs::EffectState::NotSaved, EffectState::NotSaved),
        (theirs::EffectState::Unknown, EffectState::Unknown),
    ];
    for (their_kind, kind) in kinds {
        for (their_state, state) in states {
            for document in [Some(DOCUMENT.to_owned()), None] {
                let written = serde_json::to_value(theirs::Effect {
                    effect: their_kind,
                    document_id: document.clone(),
                    state: their_state,
                })
                .unwrap();
                let read = Effect::deserialize(&written).expect("the shared layer's effect reads");
                assert_eq!((read.effect, read.state), (kind, state));
                assert_eq!(read.document_id.map(|id| id.to_string()), document);
            }
        }
    }
}
