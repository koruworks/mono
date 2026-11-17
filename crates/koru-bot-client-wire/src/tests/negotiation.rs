// SPDX-License-Identifier: MPL-2.0

//! `m8_wire_negotiation`: version ranges, the capability intersection, the
//! reserved names, the open scope set, entry kinds gated by capability, and
//! what an unknown `type` and an unknown field do.

use std::collections::BTreeSet;

use super::{all_scopes, client, hello, id, negotiated, owner, supported, vector};
use crate::{
    Ack, BUILT_CAPABILITIES, CapabilityState, ClientCommand, ClientQuery, Entry, Negotiated,
    OwnerHello, RESERVED_CAPABILITIES, Role, Violation, WireError, WireMessage, capability,
    common_version, decode_body, encode_body, owner_capabilities, refusal, scope,
};

fn scopes(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn owner_hello(sent: &crate::ClientHello) -> OwnerHello {
    Negotiated::owner_side(sent, &client(), &all_scopes(), &supported())
        .unwrap()
        .owner_hello(
            owner(),
            "0123456789abcdef0123456789abcdef".to_owned(),
            "fedcba9876543210fedcba9876543210".to_owned(),
            0,
            0,
        )
}

#[test]
fn m8_wire_negotiation_version_ranges() {
    assert_eq!(common_version(1, 1), Some(1));
    assert_eq!(common_version(0, 5), Some(1));
    assert_eq!(common_version(1, 9), Some(1));
    assert_eq!(common_version(2, 3), None);
    assert_eq!(common_version(0, 0), None);
    // An inverted range holds no version.
    assert_eq!(common_version(3, 1), None);

    // A newer client that still speaks 1 is served at 1.
    let mut wide = hello(Role::Feed);
    wide.max_version = 4;
    let stream = Negotiated::owner_side(&wide, &client(), &all_scopes(), &supported()).unwrap();
    assert_eq!(stream.version, 1);

    // No common version: refused with the owner's range.
    let mut newer = hello(Role::Feed);
    (newer.min_version, newer.max_version) = (2, 3);
    let refused =
        Negotiated::owner_side(&newer, &client(), &all_scopes(), &supported()).unwrap_err();
    assert_eq!(refused.reason, refusal::UNSUPPORTED_VERSION);
    assert_eq!(
        (refused.min_version, refused.max_version),
        (Some(1), Some(1))
    );

    // An inverted range is a malformed hello, not a version question.
    let mut inverted = hello(Role::Feed);
    (inverted.min_version, inverted.max_version) = (2, 1);
    let refused =
        Negotiated::owner_side(&inverted, &client(), &all_scopes(), &supported()).unwrap_err();
    assert_eq!(refused.reason, refusal::PROTOCOL_ERROR);

    // The client refuses an answer outside the range it offered.
    let sent = hello(Role::Feed);
    let mut answer = owner_hello(&sent);
    answer.version = 2;
    assert_eq!(
        Negotiated::client_side(&sent, &answer),
        Err(Violation::BadAnswer)
    );
}

#[test]
fn m8_wire_negotiation_the_principal_is_checked_after_the_version() {
    let sent = hello(Role::Feed);
    let other = id("c2");
    let refused = Negotiated::owner_side(&sent, &other, &all_scopes(), &supported()).unwrap_err();
    assert_eq!(refused.reason, refusal::PRINCIPAL_MISMATCH);
    assert_eq!((refused.min_version, refused.max_version), (None, None));

    let mut long = hello(Role::Feed);
    long.client_build = "b".repeat(65);
    let refused =
        Negotiated::owner_side(&long, &client(), &all_scopes(), &supported()).unwrap_err();
    assert_eq!(refused.reason, refusal::PROTOCOL_ERROR);
    long.client_build = "b".repeat(64);
    assert!(Negotiated::owner_side(&long, &client(), &all_scopes(), &supported()).is_ok());
}

#[test]
fn m8_wire_negotiation_capability_intersection() {
    // What the client asks, what the owner built, and what the peer's scopes
    // allow: only names in all three are negotiated.
    let mut sent = hello(Role::Command);
    sent.capabilities = vec![
        "cmd.future.9".to_owned(),
        capability::CMD_CONFIG.to_owned(),
        capability::CMD_CHAT.to_owned(),
        capability::FEED.to_owned(),
    ];
    let stream = Negotiated::owner_side(
        &sent,
        &client(),
        &scopes(&[scope::READ, scope::CONTROL]),
        &supported(),
    )
    .unwrap();
    // In hello order, whatever order the client asked in.
    assert_eq!(stream.capabilities(), ["feed.1", "cmd.chat.1"]);
    assert!(!stream.has(capability::CMD_CONFIG), "needs `configure`");
    assert!(!stream.has("cmd.future.9"));
    assert!(!stream.has(capability::CMD_PAUSE), "not asked for");

    // An owner that has not built a capability does not offer it.
    let without_live: Vec<&str> = supported()
        .into_iter()
        .filter(|name| *name != capability::LIVE_AI_STREAM)
        .collect();
    let stream =
        Negotiated::owner_side(&hello(Role::Feed), &client(), &all_scopes(), &without_live)
            .unwrap();
    assert!(!stream.has(capability::LIVE_AI_STREAM));
    assert_eq!(stream.capabilities().len(), 7);

    // The client holds the same set after reading the owner's answer.
    let sent = hello(Role::Command);
    let answer = owner_hello(&sent);
    let client_side = Negotiated::client_side(&sent, &answer).unwrap();
    assert_eq!(client_side.capabilities(), answer.capabilities);
    assert_eq!(
        client_side,
        Negotiated::owner_side(&sent, &client(), &all_scopes(), &supported()).unwrap()
    );

    // An answer that names a capability the client did not ask for, or does
    // not know, is refused by the client.
    let mut narrow = hello(Role::Feed);
    narrow.capabilities = vec![capability::FEED.to_owned()];
    let mut answer = owner_hello(&narrow);
    answer.capabilities.push(capability::CMD_CHAT.to_owned());
    assert_eq!(
        Negotiated::client_side(&narrow, &answer),
        Err(Violation::BadAnswer)
    );
    let mut answer = owner_hello(&sent);
    answer.capabilities.push("cmd.future.9".to_owned());
    assert_eq!(
        Negotiated::client_side(&sent, &answer),
        Err(Violation::BadAnswer)
    );
}

#[test]
fn m8_wire_negotiation_feed_is_required_and_a_role_needs_its_capability() {
    // Without `feed.1` no stream is served.
    let mut sent = hello(Role::Command);
    sent.capabilities.retain(|name| name != capability::FEED);
    let refused =
        Negotiated::owner_side(&sent, &client(), &all_scopes(), &supported()).unwrap_err();
    assert_eq!(refused.reason, refusal::UNSUPPORTED_MESSAGE);

    // A live stream needs the live capability.
    let mut sent = hello(Role::Live);
    sent.capabilities
        .retain(|name| name != capability::LIVE_AI_STREAM);
    let refused =
        Negotiated::owner_side(&sent, &client(), &all_scopes(), &supported()).unwrap_err();
    assert_eq!(refused.reason, refusal::UNSUPPORTED_MESSAGE);

    // A command stream needs at least one command or query capability.
    let mut sent = hello(Role::Command);
    sent.capabilities = vec![capability::FEED.to_owned()];
    let refused =
        Negotiated::owner_side(&sent, &client(), &all_scopes(), &supported()).unwrap_err();
    assert_eq!(refused.reason, refusal::UNSUPPORTED_MESSAGE);

    // A peer with no scope gets nothing.
    let refused = Negotiated::owner_side(&hello(Role::Feed), &client(), &scopes(&[]), &supported())
        .unwrap_err();
    assert_eq!(refused.reason, refusal::UNSUPPORTED_MESSAGE);
}

#[test]
fn m8_wire_negotiation_a_reserved_capability_is_not_offered() {
    let reserved: Vec<&str> = RESERVED_CAPABILITIES
        .iter()
        .map(|capability| capability.name)
        .collect();
    assert_eq!(
        reserved,
        [
            "cmd.routines.1",
            "cmd.lifecycle.1",
            "cmd.create.1",
            "cmd.access.1",
            "cmd.context.1",
            "cmd.routines.admin.1",
            "cmd.cloud.1",
            "query.server.1",
            "feed.routines.1",
            "feed.runs.1",
        ]
    );
    assert!(
        RESERVED_CAPABILITIES
            .iter()
            .all(|capability| capability.state == CapabilityState::Reserved)
    );
    // The two whose scope M8 fixes keep it.
    assert_eq!(RESERVED_CAPABILITIES[0].scope, Some(scope::CONFIGURE));
    assert_eq!(RESERVED_CAPABILITIES[1].scope, Some(scope::CONFIGURE));

    // Never offered, whatever scopes the peer holds.
    let offered = owner_capabilities(&all_scopes());
    assert!(reserved.iter().all(|name| !offered.contains(name)));
    assert_eq!(offered.len(), BUILT_CAPABILITIES.len());

    // Never negotiated, even when a client asks for one and an owner
    // wrongly lists it as built.
    let mut sent = hello(Role::Command);
    sent.capabilities
        .extend(reserved.iter().map(|name| (*name).to_owned()));
    let mut wrongly_supported = supported();
    wrongly_supported.extend(&reserved);
    let stream =
        Negotiated::owner_side(&sent, &client(), &all_scopes(), &wrongly_supported).unwrap();
    assert!(reserved.iter().all(|name| !stream.has(name)));
    assert_eq!(stream.capabilities().len(), BUILT_CAPABILITIES.len());

    // A client of this build does not ask for one.
    let asked = crate::client_capabilities();
    assert!(reserved.iter().all(|name| !asked.iter().any(|a| a == name)));

    // And a client refuses an owner's answer that names one.
    let sent = hello(Role::Command);
    let mut answer = owner_hello(&sent);
    answer.capabilities.push("cmd.routines.1".to_owned());
    assert_eq!(
        Negotiated::client_side(&sent, &answer),
        Err(Violation::BadAnswer)
    );
}

#[test]
fn m8_wire_negotiation_capabilities_have_one_scope() {
    let table: Vec<(&str, Option<&str>)> = BUILT_CAPABILITIES
        .iter()
        .map(|capability| (capability.name, capability.scope))
        .collect();
    assert_eq!(
        table,
        [
            ("feed.1", Some("read")),
            ("cmd.chat.1", Some("control")),
            ("cmd.pause.1", Some("control")),
            ("cmd.team_pause.1", Some("control")),
            ("cmd.decisions.1", Some("control")),
            ("cmd.config.1", Some("configure")),
            ("query.run.1", Some("read")),
            ("live.ai_stream.1", Some("read")),
        ]
    );
    // Read only: the feed, the Run read and live text, and no command.
    assert_eq!(
        owner_capabilities(&scopes(&[scope::READ])),
        ["feed.1", "query.run.1", "live.ai_stream.1"]
    );
}

#[test]
fn m8_wire_negotiation_an_unknown_scope_value_is_ignored() {
    // Owner side: a scope this build does not know adds nothing and refuses
    // nothing.
    let with_unknown = scopes(&[scope::READ, "server.admin"]);
    assert_eq!(
        owner_capabilities(&with_unknown),
        owner_capabilities(&scopes(&[scope::READ]))
    );
    let stream =
        Negotiated::owner_side(&hello(Role::Feed), &client(), &with_unknown, &supported()).unwrap();
    assert!(stream.has(capability::FEED));

    // Client side: an owner's hello with a scope the client does not know
    // decodes, negotiates, and the unknown value is dropped.
    let sent = hello(Role::Feed);
    let mut answer = owner_hello(&sent);
    answer.scopes = vec![
        "server.admin".to_owned(),
        scope::READ.to_owned(),
        "another".to_owned(),
    ];
    let body = encode_body(&WireMessage::OwnerHello(answer)).unwrap();
    let WireMessage::OwnerHello(read) = decode_body(&body).unwrap() else {
        panic!("an owner_hello");
    };
    let stream = Negotiated::client_side(&sent, &read).unwrap();
    assert_eq!(stream.scopes(), &scopes(&[scope::READ]));

    // The owner lists its scopes in the documented order, others after.
    let listed = Negotiated::owner_side(
        &hello(Role::Feed),
        &client(),
        &scopes(&["zeta", scope::CONFIGURE, scope::READ, scope::CONTROL]),
        &supported(),
    )
    .unwrap()
    .owner_hello(owner(), String::new(), String::new(), 0, 0);
    assert_eq!(listed.scopes, ["read", "control", "configure", "zeta"]);
}

#[test]
fn m8_wire_negotiation_an_entry_kind_whose_capability_was_not_negotiated_is_an_error() {
    let stream = negotiated(Role::Feed);
    // A kind a later capability defines (`feed.routines.1`), which this
    // build cannot have negotiated.
    let body = concat!(
        r#"{"type":"changes","owner_device":"00000000-0000-4000-8000-000000000004","#,
        r#""feed_id":"0123456789abcdef0123456789abcdef","boot_id":"fedcba9876543210fedcba9876543210","#,
        r#""from_cursor":0,"to_cursor":1,"reset":false,"caught_up":false,"head":1,"#,
        r#""entries":[{"seq":1,"kind":"routine","id":"r","removed":true}]}"#
    );
    let page = decode_body(body.as_bytes()).unwrap();
    assert_eq!(
        stream.check_from_owner(&page),
        Err(Violation::EntryKindNotNegotiated)
    );
    assert_eq!(
        Violation::EntryKindNotNegotiated.refusal(),
        refusal::UNSUPPORTED_MESSAGE
    );
    assert_eq!(crate::entry_kind::capability("routine"), None);
    for kind in ["bot", "chat", "message", "team_pause"] {
        assert_eq!(crate::entry_kind::capability(kind), Some("feed.1"));
    }

    // A known kind passes; its record must be of that kind, present exactly
    // when it is not removed, and in a known format.
    let record: crate::Record = serde_json::from_str(vector!("record_team_pause.json")).unwrap();
    let entry = Entry::present(1, "t".to_owned(), record.clone());
    stream.check_entry(&entry).unwrap();
    let mut wrong_kind = entry.clone();
    wrong_kind.kind = "bot".to_owned();
    assert_eq!(
        stream.check_entry(&wrong_kind),
        Err(Violation::RecordMismatch)
    );
    let mut missing = entry.clone();
    missing.record = None;
    assert_eq!(stream.check_entry(&missing), Err(Violation::RecordMismatch));
    let mut removed_with_record = entry.clone();
    removed_with_record.removed = true;
    assert_eq!(
        stream.check_entry(&removed_with_record),
        Err(Violation::RecordMismatch)
    );
    let crate::Record::TeamPause(mut newer) = record else {
        panic!("a team_pause record");
    };
    newer.format = 2;
    let newer = Entry::present(1, "t".to_owned(), crate::Record::TeamPause(newer));
    assert_eq!(
        stream.check_entry(&newer),
        Err(Violation::UnknownReplicaFormat)
    );

    // The client refuses a whole feed in a replica format it does not know.
    let sent = hello(Role::Feed);
    let mut answer = owner_hello(&sent);
    answer.replica_format = 2;
    assert_eq!(
        Negotiated::client_side(&sent, &answer),
        Err(Violation::UnknownReplicaFormat)
    );
}

#[test]
fn m8_wire_negotiation_an_unknown_message_type_is_an_error() {
    assert_eq!(
        decode_body(br#"{"type":"teleport","to":"anywhere"}"#),
        Err(WireError::UnknownType)
    );
    // A known type with a wrong shape, and bytes that are no object, are
    // malformed. Neither error carries the bytes.
    assert_eq!(decode_body(br#"{"type":"ack"}"#), Err(WireError::Malformed));
    assert_eq!(
        decode_body(br#"{"type":"ack","cursor":"seven"}"#),
        Err(WireError::Malformed)
    );
    assert_eq!(decode_body(br#"{"cursor":7}"#), Err(WireError::Malformed));
    assert_eq!(decode_body(b"[1,2]"), Err(WireError::Malformed));
    assert_eq!(decode_body(b"\xff\xfe"), Err(WireError::Malformed));
    assert_eq!(decode_body(b""), Err(WireError::EmptyFrame));
    let printed = format!(
        "{} {:?}",
        WireError::Malformed,
        decode_body(br#"{"type":"ack","cursor":"a private word"}"#)
    );
    assert!(!printed.contains("private"));

    // A closed field of this version with an unknown value is malformed too.
    assert_eq!(
        decode_body(
            br#"{"type":"live_end","chat_id":"00000000-0000-4000-8000-000000000006","reason":"bored"}"#
        ),
        Err(WireError::Malformed)
    );
}

#[test]
fn m8_wire_negotiation_an_unknown_field_is_ignored() {
    assert_eq!(
        decode_body(br#"{"type":"ack","cursor":7,"later":{"a":[1,2]}}"#).unwrap(),
        WireMessage::Ack(Ack { cursor: 7 })
    );
    // Also in a nested object of the protocol's own, and before the type.
    let hello = vector!("owner_hello.json")
        .replace(
            r#""max_frame_bytes":1048576,"#,
            r#""max_frame_bytes":1048576,"max_streams":9,"#,
        )
        .replace(
            r#"{"type":"owner_hello","#,
            r#"{"era":"m9","type":"owner_hello","#,
        );
    assert_eq!(
        decode_body(hello.as_bytes()).unwrap(),
        decode_body(vector!("owner_hello.json").as_bytes()).unwrap()
    );
    let entry = vector!("changes_reset_page.json").replace(
        r#"{"seq":38,"kind":"bot","#,
        r#"{"seq":38,"weight":3,"kind":"bot","#,
    );
    assert_eq!(
        decode_body(entry.as_bytes()).unwrap(),
        decode_body(vector!("changes_reset_page.json").as_bytes()).unwrap()
    );
}

#[test]
fn m8_wire_negotiation_an_unknown_command_kind_is_read_and_never_written() {
    let stream = negotiated(Role::Command);
    let body = concat!(
        r#"{"type":"command","request_id":"00000000-0000-4000-8000-000000000010","#,
        r#""owner_device":"00000000-0000-4000-8000-000000000004","#,
        r#""command":{"kind":"purge_bot","bot_id":"00000000-0000-4000-8000-000000000001"}}"#
    );
    let message = decode_body(body.as_bytes()).unwrap();
    let WireMessage::Command(command) = &message else {
        panic!("a command");
    };
    assert_eq!(command.command, ClientCommand::Unsupported);
    assert_eq!(command.command.capability(), None);
    assert_eq!(command.command.bot_command(), None);
    // The stream stays up: the owner answers it with a receipt.
    stream.check_from_client(&message).unwrap();
    assert_eq!(encode_body(&message), Err(WireError::Unsendable));

    let body = concat!(
        r#"{"type":"query","query_id":"q","owner_device":"00000000-0000-4000-8000-000000000004","#,
        r#""query":{"kind":"read_secrets"}}"#
    );
    let message = decode_body(body.as_bytes()).unwrap();
    let WireMessage::Query(query) = &message else {
        panic!("a query");
    };
    assert_eq!(query.query, ClientQuery::Unsupported);
    stream.check_from_client(&message).unwrap();
    assert_eq!(encode_body(&message), Err(WireError::Unsendable));
}

#[test]
fn m8_wire_negotiation_a_message_belongs_to_its_role_direction_and_capability() {
    let feed = negotiated(Role::Feed);
    let command = negotiated(Role::Command);
    let live = negotiated(Role::Live);
    let read = |name: &str| decode_body(name.as_bytes()).unwrap();

    let subscribe = read(vector!("subscribe_first.json"));
    let ack = read(vector!("ack.json"));
    let watch = read(vector!("watch.json"));
    let send = read(vector!("command_send_turn.json"));
    let query = read(vector!("query_read_run.json"));
    let changes = read(vector!("changes.json"));
    let receipt = read(vector!("receipt_completed.json"));
    let live_event = read(vector!("live.json"));
    let refused = read(vector!("refused.json"));
    let client_hello = read(vector!("client_hello.json"));
    let owner_hello = read(vector!("owner_hello.json"));

    // Each role takes its own messages from the client and no other's.
    feed.check_from_client(&subscribe).unwrap();
    feed.check_from_client(&ack).unwrap();
    live.check_from_client(&watch).unwrap();
    command.check_from_client(&send).unwrap();
    command.check_from_client(&query).unwrap();
    assert_eq!(
        command.check_from_client(&subscribe),
        Err(Violation::WrongRole)
    );
    assert_eq!(feed.check_from_client(&send), Err(Violation::WrongRole));
    assert_eq!(feed.check_from_client(&watch), Err(Violation::WrongRole));
    assert_eq!(live.check_from_client(&query), Err(Violation::WrongRole));
    assert_eq!(Violation::WrongRole.refusal(), refusal::PROTOCOL_ERROR);

    // The owner's messages are not the client's, and the reverse.
    for message in [&changes, &receipt, &live_event, &refused, &owner_hello] {
        assert_eq!(
            feed.check_from_client(message),
            Err(Violation::WrongDirection)
        );
    }
    for message in [&subscribe, &send, &watch, &client_hello] {
        assert_eq!(
            feed.check_from_owner(message),
            Err(Violation::WrongDirection)
        );
    }
    // One hello per stream.
    assert_eq!(
        feed.check_from_client(&client_hello),
        Err(Violation::UnexpectedHello)
    );
    assert_eq!(
        feed.check_from_owner(&owner_hello),
        Err(Violation::UnexpectedHello)
    );

    // From the owner: each role's own, and `refused` in any role.
    feed.check_from_owner(&changes).unwrap();
    command.check_from_owner(&receipt).unwrap();
    live.check_from_owner(&live_event).unwrap();
    for stream in [&feed, &command, &live] {
        stream.check_from_owner(&refused).unwrap();
    }
    assert_eq!(
        command.check_from_owner(&changes),
        Err(Violation::WrongRole)
    );
    assert_eq!(feed.check_from_owner(&receipt), Err(Violation::WrongRole));

    // A command whose capability was not negotiated is outside the stream.
    let mut asked = hello(Role::Command);
    asked.capabilities = vec![
        capability::FEED.to_owned(),
        capability::CMD_PAUSE.to_owned(),
    ];
    let narrow = Negotiated::owner_side(&asked, &client(), &all_scopes(), &supported()).unwrap();
    narrow
        .check_from_client(&read(vector!("command_pause_bot.json")))
        .unwrap();
    assert_eq!(
        narrow.check_from_client(&send),
        Err(Violation::NotNegotiated)
    );
    assert_eq!(
        narrow.check_from_client(&query),
        Err(Violation::NotNegotiated)
    );
    assert_eq!(
        Violation::NotNegotiated.refusal(),
        refusal::UNSUPPORTED_MESSAGE
    );

    // A read-only peer cannot send any command, and can still read a Run.
    let read_only = Negotiated::owner_side(
        &hello(Role::Command),
        &client(),
        &scopes(&[scope::READ]),
        &supported(),
    )
    .unwrap();
    assert_eq!(
        read_only.check_from_client(&send),
        Err(Violation::NotNegotiated)
    );
    read_only.check_from_client(&query).unwrap();
}
