// SPDX-License-Identifier: MPL-2.0
//! Authored bytes, independent of the production serialization projection.
use super::*;

const OWNER: &str = r#"{"user_scope":"@01","device":"@02"}"#;
const IDENTITY: &str = r#"{"revision":2,"digest":"$D4","owner_handoff":null}"#;
const SLOT: &str = r#"{"owner":$OWNER,"bot":"@04","schedule":"@05","occurrence_ms":60000}"#;
const IDS: &str = r#"{"submission":"@0b","run":"@0c","chat":"@0d","attempt":"@0e","user_message":"@0f","assistant_message":"@10"}"#;
const POLICY: &str =
    r#"{"workspace":"@03","access":"default","selected_pages":[],"destination":"@03"}"#;
const ACTOR: &str = r#"{"kind":"person","profile":"@08"}"#;
const AGENT: &str = r#"{"kind":"built_in","value":"codex"}"#;
const SCOPE: &str = r#"{"kind":"bot_folder","folder":"$FOLDER"}"#;
const ROLE: &str = r#"{"page":{"workspace":"@03","document":"@11"},"version":"@12"}"#;
const TASK: &str = r#"{"page":{"workspace":"@03","document":"@09"},"version":"@13"}"#;
const DEFINITION: &str = r#"{"revision":2,"effective_at_ms":0,"template":{"requested_by":$ACTOR,"objective":"PRIVATE-ROUTINE-OBJECTIVE","agent":$AGENT,"requested_model":"fixture-model","scope":$SCOPE,"grants":["@03"],"context":[{"kind":"document","workspace":"@03","document":"@09"}]},"recurrence":{"type":"floating","cron":"0 9 * * *"},"lifecycle":"active","routine":{"owner":$OWNER,"bot":"@04","home":"@03","creation_request":"@06"}}"#;
const CAPTURE: &str = r#"{"version":1,"creation":{"owner":$OWNER,"bot":"@04","schedule":"@05","workspace":"@03","creation_request":"@06","seed_hash":"$D2","created_at_ms":0},"schedule_definition":$DEFINITION,"canonical":{"home":"@03","config":{"name":"PRIVATE-BOT-NAME","role":{"workspace":"@03","document":"@11"},"working_folder":"$FOLDER","model":{"provider":"agent-codex","requested_model":"fixture-model","default_reasoning":"default"},"mode":null,"fast":null},"config_revision":2,"policy_revision":2},"grant":{"source_bot_policy_revision":2,"grant_revision":1,"policy":$POLICY,"digest":"$D3"},"library":{"version":1,"owner":$OWNER,"bot":"@04","submission":"@0b","config_revision":2,"knowledge":[],"memory":{"kind":"absent"},"index_digest":[$ONES]},"role":$ROLE,"task":[$TASK]}"#;
const SUBMISSION: &str = r#"{"provenance":{"owner":$OWNER,"bot":"@04","submission":"@0b","chat_id":"@0d","ai_attempt_id":"@0e","assistant_message_id":"@10","config_revision":2,"policy_revision":2,"continued_from":null,"routine":{"schedule":"@05","occurrence_ms":60000,"definition":$IDENTITY,"grant_revision":1}},"policy":$POLICY,"request":{"run":"@0c","requested_by":$ACTOR,"objective":"PRIVATE-ROUTINE-OBJECTIVE","agent":$AGENT,"requested_model":"fixture-model","scope":$SCOPE,"context":[{"kind":"document","workspace":"@03","document":"@11","version":"@12"},{"kind":"document","workspace":"@03","document":"@09","version":"@13"}],"grants":["@03"],"executing_device":"@02","origin":{"origin":"scheduled","schedule":"@05","occurrence_ms":60000,"revision":2}},"workspace":"@03","input":{"idempotency_key":"@14","text":"PRIVATE-ROUTINE-OBJECTIVE","attachment_ids":[],"model":"fixture-model"}}"#;
const PREPARATION: &str = r#"{"version":1,"slot":$SLOT,"definition":$IDENTITY,"ids":$IDS,"semantic_hash":"$D5","submission_version":2,"submission":$SUBMISSION,"config_revision":2,"policy_revision":2,"grant_revision":1,"capture_digest":"$D7"}"#;

fn expand(input: &str) -> String {
    let mut value = input.to_owned();
    for (token, bytes) in [
        ("$SUBMISSION", SUBMISSION),
        ("$DEFINITION", DEFINITION),
        ("$SLOT", SLOT),
        ("$IDS", IDS),
        ("$IDENTITY", IDENTITY),
        ("$POLICY", POLICY),
        ("$ACTOR", ACTOR),
        ("$AGENT", AGENT),
        ("$SCOPE", SCOPE),
        ("$ROLE", ROLE),
        ("$TASK", TASK),
        ("$OWNER", OWNER),
    ] {
        value = value.replace(token, bytes);
    }
    for n in 0..=20 {
        value = value.replace(
            &format!("@{n:02x}"),
            &format!("00000000-0000-4000-8000-0000000000{n:02x}"),
        );
    }
    for n in 2..=7 {
        value = value.replace(&format!("$D{n}"), &format!("{n:02x}").repeat(32));
    }
    value = value.replace("$ONES", &vec!["1"; 32].join(","));
    value = value.replace(
        "$FOLDER",
        if cfg!(windows) {
            "C:/fixture/work"
        } else {
            "/fixture/work"
        },
    );
    assert!(!value.contains('$'));
    value
}

#[test]
fn private_capture_and_preparation_have_frozen_digest_free_bytes() {
    // These byte vectors deliberately use constant digest fields. Separate
    // validation tests recompute the complete valid chain of nested digests.
    let row = super::super::super::tests::preparation();
    let capture = expand(CAPTURE);
    assert_eq!(serde_json::to_string(&row.capture).unwrap(), capture);
    let p = &row.prepared;
    let projection = PreparationV1 {
        version: 1,
        slot: &p.slot,
        definition: &p.definition,
        ids: &p.ids,
        semantic_hash: p.semantic_hash,
        submission_version: 2,
        submission: &p.submission,
        config_revision: 2,
        policy_revision: 2,
        grant_revision: 1,
        capture_digest: RoutineDigest32::from_bytes([7; 32]),
    };
    assert_eq!(
        serde_json::to_string(&projection).unwrap(),
        expand(PREPARATION)
    );
    eprintln!(
        "CAPTURE_GOLDEN_HASH={}",
        String::from(row.capture.canonical_digest().unwrap())
    );
    eprintln!(
        "PREPARATION_GOLDEN_HASH={}",
        String::from(
            routine_digest_json(
                b"koru.bot.routine.preparation.v1",
                &projection,
                BOT_ROUTINE_SUBMISSION_BYTES + BOT_ROUTINE_RECEIPT_BYTES
            )
            .unwrap()
        )
    );
}
