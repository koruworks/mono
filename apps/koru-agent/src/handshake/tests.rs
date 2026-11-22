// SPDX-License-Identifier: MPL-2.0

use super::*;

fn hello(protocol_version: u32) -> ShellHello {
    ShellHello {
        product: "koru".into(),
        version: "0.1.0".into(),
        protocol_version,
    }
}

#[test]
fn a_shell_that_speaks_this_protocol_is_welcomed_with_the_device() {
    let greeting = accept(&hello(PROTOCOL_VERSION), "device-1", "instance-1");
    match greeting {
        AgentGreeting::Welcome(welcome) => {
            assert_eq!(welcome.protocol_version, PROTOCOL_VERSION);
            assert_eq!(welcome.device_id, "device-1");
            // New on every agent start, so a shell holding a stale connection
            // can tell without dialling it.
            assert_eq!(welcome.instance_id, "instance-1");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

#[test]
fn a_protocol_version_this_agent_does_not_speak_is_refused_by_name() {
    // **A refusal, not a close.** The framing was understood, so there is a
    // shared format to answer in — which is the whole difference between this
    // layer and the one below it.
    match accept(&hello(99), "d", "i") {
        AgentGreeting::Refused {
            what, ours, theirs, ..
        } => {
            assert_eq!(what, "protocol");
            assert_eq!(ours, PROTOCOL_VERSION);
            assert_eq!(theirs, 99);
        }
        other => panic!("expected Refused, got {other:?}"),
    }
}

#[test]
fn a_refusal_names_the_program_so_a_person_knows_which_to_update() {
    // Two binaries are installed together and released together, so "one of
    // them has to be updated" is only useful if it says which one is behind.
    let AgentGreeting::Refused { message, .. } = accept(&hello(0), "d", "i") else {
        panic!("expected Refused");
    };
    assert!(message.contains("koru"), "{message}");
    assert!(message.contains(&PROTOCOL_VERSION.to_string()), "{message}");
}

#[test]
fn an_older_shell_is_refused_as_readily_as_a_newer_one() {
    // Not a `>=` check anywhere: this agent speaks exactly one protocol, and
    // guessing that an older peer is close enough is how a field that changed
    // meaning arrives as a silent default.
    for theirs in [0, 28, 32, PROTOCOL_VERSION - 1, PROTOCOL_VERSION + 1] {
        assert!(matches!(
            accept(&hello(theirs), "d", "i"),
            AgentGreeting::Refused { .. }
        ));
    }
}

#[test]
fn a_shell_on_49_is_refused_because_it_would_read_a_task_as_a_document() {
    // 50 gave a target its kind. A 49 shell reads a target without one as a
    // document or a workspace, so a task or a task list sent to it would open
    // the wrong thing; the handshake is where that is stopped.
    assert_eq!(PROTOCOL_VERSION, 50);
    match accept(&hello(49), "d", "i") {
        AgentGreeting::Refused { ours, theirs, .. } => {
            assert_eq!(ours, 50);
            assert_eq!(theirs, 49);
        }
        other => panic!("expected Refused, got {other:?}"),
    }
}

#[test]
fn an_unknown_frame_version_is_closed_rather_than_answered() {
    // **The rule that cannot be expressed as a refusal.** Writing one would
    // mean framing a reply in the very format the peer has just shown it does
    // not share.
    assert_eq!(framing(FRAME_VERSION), Framing::Understood);
    for version in [0, 2, 99, 255] {
        assert_eq!(framing(version), Framing::Close, "frame version {version}");
    }
}

#[test]
fn the_two_versions_are_read_independently() {
    // A frame this agent can read carrying a protocol it cannot, and the answer
    // is a refusal rather than a close — which is only possible because the two
    // are separate numbers.
    assert_eq!(framing(FRAME_VERSION), Framing::Understood);
    assert!(matches!(
        accept(&hello(PROTOCOL_VERSION + 1), "d", "i"),
        AgentGreeting::Refused { .. }
    ));
}
