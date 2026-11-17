// SPDX-License-Identifier: MPL-2.0

//! `m8_wire_vectors`: the golden bytes of every message of contract 2.5 and
//! every record of 2.6.
//!
//! The files under `vectors/` were written by hand before any type in this
//! crate existed. Each case reads one, checks what it decoded to, and writes
//! it back: the bytes must be the file's, byte for byte.

use koru_ai_core::bots::BotCommand;
use koru_ai_wire::{CancelAttemptResponse, StreamEvent};
use uuid::Uuid;

use super::{client, id, negotiated, owner, vector};
use crate::{
    Ack, ClientCommand, ClientHello, ClientQuery, FrameDecoder, Gap, GapReason, Keepalive, Limits,
    LiveCounts, LiveEndReason, Outcome, PageExpectation, Record, Refused, Role, Subscribe,
    WireMessage, decode_body, encode, encode_body, refusal,
};

const FEED_ID: &str = "0123456789abcdef0123456789abcdef";
const BOOT_ID: &str = "fedcba9876543210fedcba9876543210";

/// Decode `bytes` and require that writing the message gives `bytes` back.
fn golden(bytes: &str) -> WireMessage {
    let message = decode_body(bytes.as_bytes()).expect("the vector decodes");
    let written = encode_body(&message).expect("the vector's message encodes");
    assert_eq!(
        String::from_utf8(written).unwrap(),
        bytes,
        "the message is written as the vector's bytes"
    );
    message
}

fn golden_record(bytes: &str, kind: &str) -> Record {
    let record: Record = serde_json::from_str(bytes).expect("the record vector decodes");
    assert_eq!(serde_json::to_string(&record).unwrap(), bytes);
    assert_eq!(record.kind(), kind);
    assert_eq!(record.format(), 1);
    assert_eq!(record.encoded_len(), bytes.len());
    record
}

fn request() -> Uuid {
    id("10")
}

#[test]
fn m8_wire_vectors_frame_is_a_big_endian_length_then_the_json() {
    let hex = vector!("frame_keepalive.hex");
    let expected: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect();
    let framed = encode(&WireMessage::Keepalive(Keepalive {})).unwrap();
    assert_eq!(framed, expected);
    assert_eq!(&framed[..4], &[0, 0, 0, 20]);
    assert_eq!(&framed[4..], vector!("keepalive.json").as_bytes());

    let mut decoder = FrameDecoder::new();
    decoder.push(&expected).unwrap();
    assert_eq!(
        decoder.next().unwrap(),
        Some(WireMessage::Keepalive(Keepalive {}))
    );
    assert_eq!(decoder.next().unwrap(), None);
    assert_eq!(decoder.buffered(), 0);
}

#[test]
fn m8_wire_vectors_client_hello() {
    let message = golden(vector!("client_hello.json"));
    assert_eq!(
        message,
        WireMessage::ClientHello(ClientHello {
            min_version: 1,
            max_version: 1,
            capabilities: crate::client_capabilities(),
            role: Role::Feed,
            client_device: client(),
            client_build: "0.0.3".to_owned(),
        })
    );
}

#[test]
fn m8_wire_vectors_owner_hello() {
    let WireMessage::OwnerHello(hello) = golden(vector!("owner_hello.json")) else {
        panic!("an owner_hello");
    };
    assert_eq!(hello.version, 1);
    assert_eq!(hello.capabilities, ["feed.1", "cmd.chat.1"]);
    assert_eq!(hello.owner_device, owner());
    assert_eq!(hello.feed_id, FEED_ID);
    assert_eq!(hello.boot_id, BOOT_ID);
    assert_eq!(hello.head, 42);
    assert_eq!(hello.replica_format, 1);
    assert_eq!(hello.scopes, ["read", "control", "configure"]);
    assert_eq!(hello.limits, Limits::this_build());
    assert_eq!(hello.owner_time_ms, 1_763_337_600_000);

    // The owner's own builder writes the same bytes for the same stream.
    let mut asked = super::hello(Role::Feed);
    asked.capabilities = vec!["feed.1".to_owned(), "cmd.chat.1".to_owned()];
    let stream =
        crate::Negotiated::owner_side(&asked, &client(), &super::all_scopes(), &super::supported())
            .unwrap();
    let built = stream.owner_hello(
        owner(),
        FEED_ID.to_owned(),
        BOOT_ID.to_owned(),
        42,
        1_763_337_600_000,
    );
    assert_eq!(built, hello);
}

#[test]
fn m8_wire_vectors_refused() {
    assert_eq!(
        golden(vector!("refused.json")),
        WireMessage::Refused(Refused::new(refusal::NOT_OWN_DEVICE))
    );
    assert_eq!(
        golden(vector!("refused_unsupported_version.json")),
        WireMessage::Refused(Refused::unsupported_version())
    );
    // Every reason of this build is written as its own name.
    for reason in refusal::ALL {
        let body = encode_body(&WireMessage::Refused(Refused::new(reason))).unwrap();
        assert_eq!(
            String::from_utf8(body).unwrap(),
            format!(r#"{{"type":"refused","reason":"{reason}"}}"#)
        );
    }
}

#[test]
fn m8_wire_vectors_subscribe_ack_gap_keepalive() {
    assert_eq!(
        golden(vector!("subscribe_first.json")),
        WireMessage::Subscribe(Subscribe::fresh(owner()))
    );
    let WireMessage::Subscribe(resume) = golden(vector!("subscribe_resume.json")) else {
        panic!("a subscribe");
    };
    assert!(!resume.is_fresh());
    assert_eq!(resume.feed_id.as_deref(), Some(FEED_ID));
    assert_eq!(resume.boot_id.as_deref(), Some(BOOT_ID));
    assert_eq!(resume.cursor, Some(37));

    assert_eq!(
        golden(vector!("ack.json")),
        WireMessage::Ack(Ack { cursor: 42 })
    );
    assert_eq!(
        golden(vector!("gap.json")),
        WireMessage::Gap(Gap {
            owner_device: owner(),
            reason: GapReason::FeedRewound,
            head: 42,
        })
    );
    assert_eq!(
        golden(vector!("keepalive.json")),
        WireMessage::Keepalive(Keepalive {})
    );
    for (reason, name) in [
        (GapReason::FirstSubscribe, "first_subscribe"),
        (GapReason::FeedReplaced, "feed_replaced"),
        (GapReason::FeedRewound, "feed_rewound"),
        (GapReason::ClientRequested, "client_requested"),
    ] {
        assert_eq!(
            serde_json::to_string(&reason).unwrap(),
            format!("\"{name}\"")
        );
    }
}

#[test]
fn m8_wire_vectors_changes_carries_every_record_kind_and_a_removal() {
    let message = golden(vector!("changes.json"));
    let WireMessage::Changes(page) = &message else {
        panic!("a changes page");
    };
    assert_eq!(page.owner_device, owner());
    assert_eq!((page.from_cursor, page.to_cursor, page.head), (37, 42, 42));
    assert!(!page.reset);
    assert!(page.caught_up);
    assert_eq!(
        page.live,
        Some(LiveCounts {
            bots: 1,
            chats: 1,
            messages: 1
        })
    );
    assert_eq!(page.withheld_bots, Some(1));
    let kinds: Vec<(u64, &str, bool)> = page
        .entries
        .iter()
        .map(|entry| (entry.seq, entry.kind.as_str(), entry.removed))
        .collect();
    assert_eq!(
        kinds,
        [
            (38, "bot", false),
            (39, "chat", false),
            (40, "message", false),
            (41, "team_pause", false),
            (42, "message", true),
        ]
    );
    // Each entry's record is the record vector of its kind, and the entry
    // the owner's constructor builds from that record is the entry sent.
    let records = [
        golden_record(vector!("record_bot.json"), "bot"),
        golden_record(vector!("record_chat.json"), "chat"),
        golden_record(vector!("record_message.json"), "message"),
        golden_record(vector!("record_team_pause.json"), "team_pause"),
    ];
    for (entry, record) in page.entries.iter().zip(&records) {
        assert_eq!(entry.record.as_ref(), Some(record));
    }
    assert_eq!(page.entries[4].record, None);
    assert_eq!(
        crate::Entry::present(39, page.entries[1].id.clone(), records[1].clone()),
        page.entries[1]
    );
    let mut message_entry =
        crate::Entry::present(40, page.entries[2].id.clone(), records[2].clone());
    assert_eq!(message_entry.chat_id, page.entries[2].chat_id);
    message_entry.bot_id = page.entries[2].bot_id.clone();
    assert_eq!(message_entry, page.entries[2]);

    // The page passes the client's transport checks and the stream's rules.
    page.check_page(&PageExpectation {
        owner_device: &owner(),
        feed_id: FEED_ID,
        boot_id: BOOT_ID,
        cursor: 37,
    })
    .unwrap();
    page.check_limits(&Limits::this_build()).unwrap();
    negotiated(Role::Feed).check_from_owner(&message).unwrap();

    let WireMessage::Changes(reset) = golden(vector!("changes_reset_page.json")) else {
        panic!("a changes page");
    };
    assert!(reset.reset);
    assert!(!reset.caught_up);
    assert_eq!((reset.live, reset.withheld_bots), (None, None));
    reset
        .check_page(&PageExpectation {
            owner_device: &owner(),
            feed_id: FEED_ID,
            boot_id: BOOT_ID,
            cursor: 0,
        })
        .unwrap();
}

#[test]
fn m8_wire_vectors_records() {
    let Record::Bot(bot) = golden_record(vector!("record_bot.json"), "bot") else {
        panic!("a bot record");
    };
    assert_eq!(bot.bot.owner_device, owner());
    // The bot record carries no team state.
    assert!(!vector!("record_bot.json").contains("team"));

    let Record::Chat(chat) = golden_record(vector!("record_chat.json"), "chat") else {
        panic!("a chat record");
    };
    assert_eq!(chat.purpose.token(), "main");
    assert!(!chat.trashed);
    assert!(chat.activity.running);
    assert!(!chat.activity.needs_you);
    assert!(chat.run_id.is_some() && chat.run_workspace_id.is_some());

    let Record::Chat(additional) = golden_record(vector!("record_chat_additional.json"), "chat")
    else {
        panic!("a chat record");
    };
    assert_eq!(additional.purpose.token(), "additional");
    assert!(additional.trashed);
    assert_eq!(
        (additional.run_id, additional.run_workspace_id),
        (None, None)
    );

    let Record::Message(message) = golden_record(vector!("record_message.json"), "message") else {
        panic!("a message record");
    };
    assert_eq!(message.blocks.len(), 1);
    assert_eq!(message.tool_activities.len(), 1);
    assert_eq!(message.notices.len(), 1);
    assert_eq!(message.feedback.len(), 1);
    assert!(message.usage.is_some() && message.sources.is_some());
    assert_eq!(message.attachment_count, 2);
    assert!(!message.body_omitted);

    // A pending message is sent with an empty markdown and no blocks.
    let Record::Message(pending) = golden_record(vector!("record_message_pending.json"), "message")
    else {
        panic!("a message record");
    };
    assert!(pending.message.markdown.is_empty());
    assert!(pending.blocks.is_empty());
    assert_eq!(
        (pending.usage.clone(), pending.sources.clone()),
        (None, None)
    );

    let Record::TeamPause(team) = golden_record(vector!("record_team_pause.json"), "team_pause")
    else {
        panic!("a team_pause record");
    };
    assert!(team.team.paused);
    assert_eq!(team.team.revision, 3);
    assert_eq!(team.team.owner_device, owner());
}

#[test]
fn m8_wire_vectors_live_role() {
    let WireMessage::Watch(watch) = golden(vector!("watch.json")) else {
        panic!("a watch");
    };
    assert_eq!(watch.owner_device, owner());
    assert_eq!(watch.chat_id, id("06"));
    let WireMessage::Unwatch(unwatch) = golden(vector!("unwatch.json")) else {
        panic!("an unwatch");
    };
    assert_eq!(unwatch.chat_id, id("06"));

    let WireMessage::Live(live) = golden(vector!("live.json")) else {
        panic!("a live event");
    };
    assert_eq!(live.attempt_id, id("0b"));
    // The event is the public stream event, passed through.
    let event: StreamEvent = serde_json::from_str(
        r#"{"version":1,"sequence":7,"event":{"type":"text_delta","attempt_id":"00000000-0000-4000-8000-00000000000b","text":"Hel","at":3}}"#,
    )
    .unwrap();
    assert_eq!(live.event, event);
    assert_eq!(live.event.version, koru_ai_wire::AI_STREAM_VERSION);

    let WireMessage::LiveEnd(end) = golden(vector!("live_end.json")) else {
        panic!("a live_end");
    };
    assert_eq!(end.reason, LiveEndReason::Terminal);
    assert_eq!(end.attempt_id, Some(id("0b")));
    let WireMessage::LiveEnd(missing) = golden(vector!("live_end_not_found.json")) else {
        panic!("a live_end");
    };
    assert_eq!(missing.reason, LiveEndReason::NotFound);
    assert_eq!(missing.attempt_id, None);
    for (reason, name) in [
        (LiveEndReason::Terminal, "terminal"),
        (LiveEndReason::Dropped, "dropped"),
        (LiveEndReason::Closed, "closed"),
        (LiveEndReason::NotFound, "not_found"),
    ] {
        assert_eq!(
            serde_json::to_string(&reason).unwrap(),
            format!("\"{name}\"")
        );
    }
}

/// Every command kind: its vector, its capability, and, for the kinds the
/// Bot authority runs, the exact public JSON of the local command it is.
#[test]
fn m8_wire_vectors_every_command_kind() {
    const P: &str = "00000000-0000-4000-8000-0000000000";
    let cases: [(&str, &str, &str, bool, Option<String>); 11] = [
        (
            vector!("command_send_turn.json"),
            "send_turn",
            "cmd.chat.1",
            true,
            None,
        ),
        (
            vector!("command_retry_turn.json"),
            "retry_turn",
            "cmd.chat.1",
            true,
            None,
        ),
        (
            vector!("command_cancel_attempt.json"),
            "cancel_attempt",
            "cmd.chat.1",
            false,
            None,
        ),
        (
            vector!("command_steer.json"),
            "steer",
            "cmd.chat.1",
            true,
            None,
        ),
        (
            vector!("command_create_chat.json"),
            "create_chat",
            "cmd.chat.1",
            true,
            Some(format!(
                r#"{{"command":"create_chat","request_id":"{P}10","bot_id":"{P}01"}}"#
            )),
        ),
        (
            vector!("command_pause_bot.json"),
            "pause_bot",
            "cmd.pause.1",
            true,
            Some(format!(
                r#"{{"command":"pause","request_id":"{P}10","bot_id":"{P}01","expected_revision":3}}"#
            )),
        ),
        (
            vector!("command_resume_bot.json"),
            "resume_bot",
            "cmd.pause.1",
            true,
            Some(format!(
                r#"{{"command":"resume","request_id":"{P}10","bot_id":"{P}01","expected_revision":4}}"#
            )),
        ),
        (
            vector!("command_pause_team.json"),
            "pause_team",
            "cmd.team_pause.1",
            true,
            Some(format!(
                r#"{{"command":"pause_team","request_id":"{P}10","expected_team_revision":0}}"#
            )),
        ),
        (
            vector!("command_resume_team.json"),
            "resume_team",
            "cmd.team_pause.1",
            true,
            Some(format!(
                r#"{{"command":"resume_team","request_id":"{P}10","expected_team_revision":1}}"#
            )),
        ),
        (
            vector!("command_answer_decision.json"),
            "answer_decision",
            "cmd.decisions.1",
            false,
            None,
        ),
        (
            vector!("command_update_bot.json"),
            "update_bot",
            "cmd.config.1",
            true,
            Some(format!(
                concat!(
                    r#"{{"command":"update","request_id":"{p}10","bot_id":"{p}01","expected_revision":3,"#,
                    r#""config":{{"name":"renamed","role":{{"workspace":"{p}02","document":"{p}05"}},"#,
                    r#""working_folder":"/fixture","model":{{"provider":"agent-codex","#,
                    r#""requested_model":"fixture","default_reasoning":"default"}},"mode":"auto","fast":true}}}}"#
                ),
                p = P
            )),
        ),
    ];
    let stream = negotiated(Role::Command);
    for (bytes, kind, capability, has_request_id, local) in cases {
        let message = golden(bytes);
        let WireMessage::Command(command) = &message else {
            panic!("{kind}: a command");
        };
        assert_eq!(command.request_id, request(), "{kind}");
        assert_eq!(command.owner_device, owner(), "{kind}");
        assert_eq!(command.command.kind(), kind);
        assert_eq!(command.command.capability(), Some(capability), "{kind}");
        // One request id per intent: the embedded command's is the wrapper's.
        assert_eq!(
            command.command.embedded_request_id(),
            has_request_id.then(request),
            "{kind}"
        );
        assert!(!command.command.carries_attachments(), "{kind}");
        let bot_command: Option<BotCommand> = command.command.bot_command();
        assert_eq!(
            bot_command.map(|command| serde_json::to_string(&command).unwrap()),
            local,
            "{kind}"
        );
        stream.check_from_client(&message).unwrap();
    }
}

#[test]
fn m8_wire_vectors_command_payloads_are_the_public_request_shapes() {
    let WireMessage::Command(send) = golden(vector!("command_send_turn.json")) else {
        panic!("a command");
    };
    let ClientCommand::SendTurn { request } = &send.command else {
        panic!("a send_turn");
    };
    assert_eq!(request.chat_id, id("06"));
    assert_eq!(request.explicit_refs.len(), 1);
    assert!(request.attachment_ids.is_empty());

    let WireMessage::Command(answer) = golden(vector!("command_answer_decision.json")) else {
        panic!("a command");
    };
    let ClientCommand::AnswerDecision {
        tool_id,
        approve,
        decision,
    } = &answer.command
    else {
        panic!("an answer_decision");
    };
    assert_eq!(*tool_id, id("0a"));
    assert!(*approve);
    assert_eq!(decision.choice.as_deref(), Some("allow_once"));
}

#[test]
fn m8_wire_vectors_receipts() {
    let WireMessage::Receipt(done) = golden(vector!("receipt_completed.json")) else {
        panic!("a receipt");
    };
    assert_eq!(done.request_id, request());
    assert_eq!(done.outcome, Outcome::Completed);
    assert!(!done.replayed);
    assert_eq!(done.feed_cursor, Some(43));
    assert_eq!(done.failure, None);
    // The response is the local command's public response, byte for byte.
    let response = done.response.as_ref().unwrap();
    assert_eq!(
        response.to_json(),
        r#"{"attempt_id":"00000000-0000-4000-8000-00000000000b","status":"cancelled"}"#
    );
    let cancelled: CancelAttemptResponse = response.parse().unwrap();
    assert_eq!(cancelled.attempt_id, id("0b"));
    assert_eq!(
        crate::JsonObject::from_serialize(&cancelled).unwrap(),
        *response
    );

    let WireMessage::Receipt(accepted) = golden(vector!("receipt_accepted.json")) else {
        panic!("a receipt");
    };
    assert_eq!(accepted.outcome, Outcome::Accepted);
    let team: koru_ai_core::bots::BotResponse =
        accepted.response.as_ref().unwrap().parse().unwrap();
    assert!(matches!(
        team,
        koru_ai_core::bots::BotResponse::TeamPause { .. }
    ));

    let WireMessage::Receipt(refused) = golden(vector!("receipt_refused.json")) else {
        panic!("a receipt");
    };
    assert_eq!(refused.outcome, Outcome::Refused);
    let failure = refused.failure.as_ref().unwrap();
    assert_eq!(failure.code, "executing_computer_only");
    assert!(!failure.retryable);
    assert_eq!(
        (refused.response.clone(), refused.feed_cursor),
        (None, None)
    );

    let WireMessage::Receipt(unknown) = golden(vector!("receipt_unknown.json")) else {
        panic!("a receipt");
    };
    assert_eq!(unknown.outcome, Outcome::Unknown);
    assert!(unknown.replayed);
    for (outcome, name) in [
        (Outcome::Completed, "completed"),
        (Outcome::Accepted, "accepted"),
        (Outcome::Refused, "refused"),
        (Outcome::Unknown, "unknown"),
    ] {
        assert_eq!(
            serde_json::to_string(&outcome).unwrap(),
            format!("\"{name}\"")
        );
    }
}

#[test]
fn m8_wire_vectors_queries_and_answers() {
    let stream = negotiated(Role::Command);
    let run = golden(vector!("query_read_run.json"));
    let WireMessage::Query(query) = &run else {
        panic!("a query");
    };
    assert_eq!(query.query_id, "q-1");
    assert_eq!(query.owner_device, owner());
    assert_eq!(query.query.kind(), "read_run");
    assert_eq!(query.query.capability(), Some("query.run.1"));
    assert_eq!(
        serde_json::to_string(&query.query.bot_command().unwrap()).unwrap(),
        concat!(
            r#"{"command":"read_run","bot_id":"00000000-0000-4000-8000-000000000001","#,
            r#""chat_id":"00000000-0000-4000-8000-000000000006","#,
            r#""message_id":"00000000-0000-4000-8000-000000000008","#,
            r#""run_id":"00000000-0000-4000-8000-00000000000e","from":0,"len":50}"#
        )
    );
    stream.check_from_client(&run).unwrap();

    let operation = golden(vector!("query_read_operation.json"));
    let WireMessage::Query(query) = &operation else {
        panic!("a query");
    };
    assert!(matches!(query.query, ClientQuery::ReadOperation { .. }));
    assert_eq!(
        serde_json::to_string(&query.query.bot_command().unwrap()).unwrap(),
        concat!(
            r#"{"command":"read_operation","request_id":"00000000-0000-4000-8000-000000000010","#,
            r#""bot_id":"00000000-0000-4000-8000-000000000001"}"#
        )
    );
    stream.check_from_client(&operation).unwrap();

    let WireMessage::Answer(answer) = golden(vector!("answer.json")) else {
        panic!("an answer");
    };
    assert_eq!(answer.query_id, "q-2");
    assert!(answer.response.is_some() && answer.failure.is_none());
    let WireMessage::Answer(failed) = golden(vector!("answer_failed.json")) else {
        panic!("an answer");
    };
    assert_eq!(failed.failure.as_ref().unwrap().code, "not_found");
    assert_eq!(failed.response, None);
}

/// Sixteen message types, and a vector for each.
#[test]
fn m8_wire_vectors_cover_every_message_type() {
    let vectors = [
        vector!("client_hello.json"),
        vector!("owner_hello.json"),
        vector!("refused.json"),
        vector!("subscribe_first.json"),
        vector!("ack.json"),
        vector!("gap.json"),
        vector!("changes.json"),
        vector!("keepalive.json"),
        vector!("watch.json"),
        vector!("unwatch.json"),
        vector!("live.json"),
        vector!("live_end.json"),
        vector!("command_send_turn.json"),
        vector!("receipt_completed.json"),
        vector!("query_read_run.json"),
        vector!("answer.json"),
    ];
    let types: Vec<&str> = vectors
        .iter()
        .map(|bytes| golden(bytes).type_name())
        .collect();
    assert_eq!(types, WireMessage::TYPES);
}

/// No `Debug` in this crate prints what a person or a Bot wrote.
#[test]
fn m8_wire_vectors_debug_prints_no_content() {
    let cases = [
        (vector!("command_send_turn.json"), "Check the report"),
        (vector!("command_steer.json"), "Also the appendix"),
        (vector!("command_update_bot.json"), "renamed"),
        (vector!("command_answer_decision.json"), "allow_once"),
        (vector!("changes.json"), "Done."),
        (vector!("changes.json"), "Hook failed"),
        (vector!("changes.json"), "New chat"),
        (vector!("changes.json"), "/fixture"),
        (vector!("live.json"), "Hel"),
        (vector!("receipt_refused.json"), "This change is made"),
        (vector!("receipt_completed.json"), "cancelled"),
        (vector!("answer_failed.json"), "No such Run"),
        (vector!("query_read_run.json"), "q-1"),
    ];
    for (bytes, content) in cases {
        let message = golden(bytes);
        let printed = format!("{message:?} {message:#?}");
        assert!(
            !printed.contains(content),
            "{} prints its content",
            message.type_name()
        );
    }
    for bytes in [
        vector!("record_bot.json"),
        vector!("record_chat.json"),
        vector!("record_message.json"),
    ] {
        let record: Record = serde_json::from_str(bytes).unwrap();
        let printed = format!("{record:?}");
        for content in ["fixture", "New chat", "Done."] {
            assert!(!printed.contains(content));
        }
    }
}
