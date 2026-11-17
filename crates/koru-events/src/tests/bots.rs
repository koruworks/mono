// SPDX-License-Identifier: MPL-2.0

//! Cohort `m8_events_content_free`, the registry's half: the six `bots.*`
//! rows declare exactly the attributes they were approved with, every value
//! a payload can build is an id, a number, a flag or a declared token, and
//! none of them can leave the device. The emitter's half (each row at its
//! transition, none on a replay) is in `koru-ai-server`.

use super::*;
use crate::registry::{
    BOTS_CLIENT_ROLES, BOTS_COMMAND_FAILURES, BOTS_COMMAND_KINDS, BOTS_COMMAND_OUTCOMES,
    BOTS_MIRROR_REASONS, BOTS_MIRROR_REMOVED_REASONS, BOTS_MIRROR_STATES, BOTS_RESET_REASONS,
    BOTS_SESSION_REASONS, BOTS_SESSION_STAGES, BOTS_TEAM_PAUSE_STAGES,
};

/// One declared attribute: its key, its kind, and whether it is required.
type Listed = (&'static str, AttrKind, bool);

/// The six rows as they were approved: name, class, and every attribute.
const ROWS: &[(&str, EventClass, &[Listed])] = &[
    (
        "bots.team_pause.changed",
        EventClass::Domain,
        &[
            ("owner_device", AttrKind::Id, true),
            ("epoch", AttrKind::Count, true),
            ("paused", AttrKind::Flag, true),
            ("stage", AttrKind::Token, true),
        ],
    ),
    (
        "bots.client.session",
        EventClass::Instrumentation,
        &[
            ("peer_device", AttrKind::Id, true),
            ("stage", AttrKind::Token, true),
            ("role", AttrKind::Token, false),
            ("protocol_version", AttrKind::Count, false),
            ("reason", AttrKind::Token, false),
        ],
    ),
    (
        "bots.client.command",
        EventClass::Domain,
        &[
            ("peer_device", AttrKind::Id, true),
            ("request", AttrKind::Id, true),
            ("kind", AttrKind::Token, true),
            ("outcome", AttrKind::Token, true),
            ("failure", AttrKind::Token, false),
        ],
    ),
    (
        "bots.client.reset",
        EventClass::Instrumentation,
        &[
            ("peer_device", AttrKind::Id, true),
            ("reason", AttrKind::Token, true),
        ],
    ),
    (
        "bots.mirror.state",
        EventClass::Instrumentation,
        &[
            ("owner_device", AttrKind::Id, true),
            ("state", AttrKind::Token, true),
            ("reason", AttrKind::Token, true),
            ("cursor", AttrKind::Count, true),
        ],
    ),
    (
        "bots.mirror.removed",
        EventClass::Domain,
        &[
            ("owner_device", AttrKind::Id, true),
            ("reason", AttrKind::Token, true),
        ],
    ),
];

fn row(name: &str) -> &'static EventSpec {
    registry::spec(name).unwrap_or_else(|| panic!("{name} is not registered"))
}

#[test]
fn m8_events_content_free_the_six_rows_hold_only_the_listed_attributes() {
    let registered: Vec<&str> = REGISTRY
        .iter()
        .map(|spec| spec.name)
        .filter(|name| name.starts_with("bots."))
        .collect();
    let listed: Vec<&str> = ROWS.iter().map(|(name, ..)| *name).collect();
    assert_eq!(registered, listed, "the `bots` area holds these six rows");

    for (name, class, attrs) in ROWS {
        let spec = row(name);
        assert_eq!(spec.version, 1, "{name}");
        assert_eq!(spec.class, *class, "{name}");
        assert_eq!(
            spec.telemetry,
            TelemetryPolicy::Never,
            "{name} never leaves the device"
        );
        let declared: Vec<Listed> = spec
            .attrs
            .iter()
            .map(|attr| (attr.key, attr.kind, attr.required))
            .collect();
        assert_eq!(declared, attrs.to_vec(), "{name}");
        for attr in spec.attrs {
            // No kind that could hold free text, a time or a measurement of
            // what somebody wrote: an id, a count, a flag or a closed token.
            assert!(
                matches!(
                    attr.kind,
                    AttrKind::Id | AttrKind::Count | AttrKind::Flag | AttrKind::Token
                ),
                "{name}.{}",
                attr.key
            );
            if attr.kind == AttrKind::Id {
                assert_eq!(
                    attr.class,
                    FieldClass::LocalReference,
                    "{name}.{}",
                    attr.key
                );
            }
            // What a key may not even be named after.
            for word in [
                "name", "title", "text", "message", "path", "folder", "token", "user", "chat",
                "content", "prompt", "label",
            ] {
                assert!(
                    !attr.key.contains(word),
                    "{name}.{} is named after content",
                    attr.key
                );
            }
        }
    }
}

fn tokens_of(name: &str, key: &str) -> &'static [&'static str] {
    row(name)
        .attr(key)
        .unwrap_or_else(|| panic!("{name}.{key}"))
        .vocabulary
}

#[test]
fn m8_events_content_free_tokens_are_the_vocabularies() {
    fn same<T: Copy>(
        all: &[T],
        token: fn(T) -> &'static str,
        vocabulary: &[&str],
        row: &[&str],
        what: &str,
    ) {
        let tokens: Vec<&str> = all.iter().map(|value| token(*value)).collect();
        assert_eq!(tokens, vocabulary, "{what}: the enum and the vocabulary");
        assert_eq!(row, vocabulary, "{what}: the row and the vocabulary");
    }
    same(
        BotsTeamPauseStage::ALL,
        BotsTeamPauseStage::token,
        BOTS_TEAM_PAUSE_STAGES,
        tokens_of("bots.team_pause.changed", "stage"),
        "team pause stage",
    );
    same(
        BotsSessionStage::ALL,
        BotsSessionStage::token,
        BOTS_SESSION_STAGES,
        tokens_of("bots.client.session", "stage"),
        "session stage",
    );
    same(
        BotsClientRole::ALL,
        BotsClientRole::token,
        BOTS_CLIENT_ROLES,
        tokens_of("bots.client.session", "role"),
        "role",
    );
    same(
        BotsSessionReason::ALL,
        BotsSessionReason::token,
        BOTS_SESSION_REASONS,
        tokens_of("bots.client.session", "reason"),
        "session reason",
    );
    same(
        BotsCommandKind::ALL,
        BotsCommandKind::token,
        BOTS_COMMAND_KINDS,
        tokens_of("bots.client.command", "kind"),
        "command kind",
    );
    same(
        BotsCommandOutcome::ALL,
        BotsCommandOutcome::token,
        BOTS_COMMAND_OUTCOMES,
        tokens_of("bots.client.command", "outcome"),
        "command outcome",
    );
    same(
        BotsCommandFailure::ALL,
        BotsCommandFailure::token,
        BOTS_COMMAND_FAILURES,
        tokens_of("bots.client.command", "failure"),
        "command failure",
    );
    same(
        BotsResetReason::ALL,
        BotsResetReason::token,
        BOTS_RESET_REASONS,
        tokens_of("bots.client.reset", "reason"),
        "reset reason",
    );
    same(
        BotsMirrorState::ALL,
        BotsMirrorState::token,
        BOTS_MIRROR_STATES,
        tokens_of("bots.mirror.state", "state"),
        "mirror state",
    );
    same(
        BotsMirrorReason::ALL,
        BotsMirrorReason::token,
        BOTS_MIRROR_REASONS,
        tokens_of("bots.mirror.state", "reason"),
        "mirror reason",
    );
    same(
        BotsMirrorRemovedReason::ALL,
        BotsMirrorRemovedReason::token,
        BOTS_MIRROR_REMOVED_REASONS,
        tokens_of("bots.mirror.removed", "reason"),
        "removed reason",
    );
}

/// Every payload, with every variant of every token it can carry.
fn every_payload() -> Vec<Draft> {
    let device = Uuid::from_u128(0x6666_6666_6666_6666_6666_6666_6666_6666);
    let request = Uuid::from_u128(0x7777_7777_7777_7777_7777_7777_7777_7777);
    let mut drafts: Vec<Draft> = Vec::new();
    for stage in BotsTeamPauseStage::ALL {
        for paused in [false, true] {
            drafts.push(
                BotsTeamPauseChanged {
                    owner_device: device,
                    epoch: 7,
                    paused,
                    stage: *stage,
                }
                .into(),
            );
        }
    }
    for stage in BotsSessionStage::ALL {
        // With nothing optional, then with every optional value.
        drafts.push(
            BotsClientSession {
                peer_device: device,
                stage: *stage,
                role: None,
                protocol_version: None,
                reason: None,
            }
            .into(),
        );
        for role in BotsClientRole::ALL {
            for reason in BotsSessionReason::ALL {
                drafts.push(
                    BotsClientSession {
                        peer_device: device,
                        stage: *stage,
                        role: Some(*role),
                        protocol_version: Some(1),
                        reason: Some(*reason),
                    }
                    .into(),
                );
            }
        }
    }
    for kind in BotsCommandKind::ALL {
        for outcome in BotsCommandOutcome::ALL {
            drafts.push(
                BotsClientCommand {
                    peer_device: device,
                    request,
                    kind: *kind,
                    outcome: *outcome,
                    failure: None,
                }
                .into(),
            );
        }
    }
    for failure in BotsCommandFailure::ALL {
        drafts.push(
            BotsClientCommand {
                peer_device: device,
                request,
                kind: BotsCommandKind::SendTurn,
                outcome: BotsCommandOutcome::Refused,
                failure: Some(*failure),
            }
            .into(),
        );
    }
    for reason in BotsResetReason::ALL {
        drafts.push(
            BotsClientReset {
                peer_device: device,
                reason: *reason,
            }
            .into(),
        );
    }
    for state in BotsMirrorState::ALL {
        for reason in BotsMirrorReason::ALL {
            drafts.push(
                BotsMirrorStateChanged {
                    owner_device: device,
                    state: *state,
                    reason: *reason,
                    cursor: 42,
                }
                .into(),
            );
        }
    }
    for reason in BotsMirrorRemovedReason::ALL {
        drafts.push(
            BotsMirrorRemoved {
                owner_device: device,
                reason: *reason,
            }
            .into(),
        );
    }
    drafts
}

#[test]
fn m8_events_content_free_every_payload_is_valid_and_holds_no_text() {
    let ctx = fixed_context();
    let mut names = std::collections::BTreeSet::new();
    for draft in every_payload() {
        let envelope = draft.build(&ctx, fixed_event_id(), FIXED_TIME);
        validate(&envelope).unwrap_or_else(|violation| {
            panic!("{} fails its own registry row: {violation}", envelope.name)
        });
        let spec = row(envelope.name.as_str());
        // Nothing but the attributes: no subject, no workspace, no chain.
        assert!(envelope.subject.is_none(), "{}", envelope.name);
        assert!(envelope.scope.workspace_id.is_none(), "{}", envelope.name);
        for (key, value) in &envelope.attrs {
            let attr = spec.attr(key).expect("validated");
            match value {
                AttrValue::Id(_) | AttrValue::Count(_) | AttrValue::Flag(_) => {}
                // The only text a row can hold is one of its declared tokens.
                AttrValue::Token(token) => assert!(
                    attr.vocabulary.contains(&token.as_str()),
                    "{}.{key} = {token}",
                    envelope.name
                ),
                other => panic!("{}.{key} holds {other:?}", envelope.name),
            }
        }
        names.insert(envelope.name.as_str().to_owned());
    }
    let listed: std::collections::BTreeSet<String> =
        ROWS.iter().map(|(name, ..)| (*name).to_owned()).collect();
    assert_eq!(names, listed, "every row has a payload");
}

#[test]
fn m8_events_content_free_a_code_outside_a_set_is_not_carried() {
    // What an emitter holds as text is looked up; anything a person, a Bot or
    // another computer could have put there names no variant, so the emitter
    // sends `other` and never the text.
    for text in [
        "Budget Bot",
        "C:\\Users\\someone\\Documents\\plan.md",
        "please summarize the acquisition",
        "sk-live-0123456789",
        "Someone's laptop",
        "",
        "NOT_FOUND",
        "not found",
    ] {
        assert_eq!(BotsSessionReason::from_token(text), None, "{text}");
        assert_eq!(BotsCommandKind::from_token(text), None, "{text}");
        assert_eq!(BotsCommandFailure::from_token(text), None, "{text}");
        assert_eq!(BotsMirrorState::from_token(text), None, "{text}");
    }
    assert_eq!(
        BotsCommandFailure::from_token("revision_conflict"),
        Some(BotsCommandFailure::RevisionConflict)
    );
    // The boundary holds the same line for anything that bypassed the typed
    // payloads: a token outside the vocabulary is refused whole.
    let ctx = fixed_context();
    let forged = Draft::new(event_name!("bots.client.command"))
        .attr("peer_device", AttrValue::Id(Uuid::from_u128(1)))
        .attr("request", AttrValue::Id(Uuid::from_u128(2)))
        .attr("kind", AttrValue::Token("send_turn".into()))
        .attr("outcome", AttrValue::Token("refused".into()))
        .attr("failure", AttrValue::Token("Budget Bot".into()))
        .build(&ctx, fixed_event_id(), FIXED_TIME);
    assert_eq!(
        validate(&forged).unwrap_err().code(),
        "token_outside_vocabulary"
    );
    let extra = Draft::from(BotsClientReset {
        peer_device: Uuid::from_u128(1),
        reason: BotsResetReason::FeedRewound,
    })
    .attr("bot", AttrValue::Id(Uuid::from_u128(3)))
    .build(&ctx, fixed_event_id(), FIXED_TIME);
    assert_eq!(validate(&extra).unwrap_err().code(), "undeclared_attr");
}

#[test]
fn m8_events_content_free_none_leaves_the_device() {
    let ctx = fixed_context();
    for draft in every_payload() {
        let envelope = draft.build(&ctx, fixed_event_id(), FIXED_TIME);
        assert!(
            project(&envelope, &granted()).is_none(),
            "{} was projected for export",
            envelope.name
        );
    }
}
