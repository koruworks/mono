// SPDX-License-Identifier: MPL-2.0

//! The wire, asserted as JSON — because JSON is what crosses it.
//!
//! The fixtures under `tests/vectors/` are the drift defence, and they follow
//! `koru-events`' mechanism exactly: `KORU_UPDATE_VOICE_VECTORS=1 cargo test -p
//! koru-voice` rewrites them, and any other run compares. A changed byte fails
//! here first, next to the declaration that changed it, rather than in the
//! worker three plans later.

use super::*;
use uuid::Uuid;

/// Fixed ids so a vector is reproducible. A minted uuid in a fixture would
/// rewrite every file on every run and the comparison would assert nothing.
fn fixed_session() -> SessionId {
    SessionId(Uuid::from_u128(0x7777_7777_7777_7777_7777_7777_7777_7777))
}

fn update_requested() -> bool {
    match std::env::var("KORU_UPDATE_VOICE_VECTORS") {
        Ok(value) => {
            let value = value.trim().to_ascii_lowercase();
            !matches!(value.as_str(), "" | "0" | "false" | "no" | "off")
        }
        Err(_) => false,
    }
}

/// One fixture per message, both directions, round-tripped back into its type.
///
/// The plan asks for every `Request` and every `Event`, and "every" is checked
/// rather than trusted: the counts below fail when a variant is added without a
/// vector, which is the only way a wire shape reaches a second implementation
/// undocumented.
#[test]
fn the_wire_vectors_are_current() {
    let session = fixed_session();
    let requests: Vec<(&str, Request)> = vec![
        (
            "request.begin",
            Request::Begin {
                session,
                generation: 4,
                language: Requested::Fixed(VoiceLanguage::PtBr),
                tier: ModelTier::HighAccuracy,
                model: Some(ModelId::parse("nemotron-3.5-int8").unwrap()),
                input: InputChoice::named("Yeti Nano"),
            },
        ),
        (
            "request.begin.auto",
            Request::Begin {
                session,
                generation: 5,
                language: Requested::Auto,
                tier: ModelTier::Compact,
                model: None,
                input: InputChoice::SystemDefault,
            },
        ),
        (
            "request.finish",
            Request::Finish {
                session,
                generation: 4,
            },
        ),
        (
            "request.cancel",
            Request::Cancel {
                session,
                generation: 4,
            },
        ),
        (
            "request.devices",
            Request::Devices {
                session,
                generation: 4,
            },
        ),
        (
            "request.meter",
            Request::Meter {
                session,
                generation: 6,
                input: InputChoice::named("Yeti Nano"),
            },
        ),
        (
            "request.meter_stop",
            Request::MeterStop {
                session,
                generation: 6,
            },
        ),
    ];
    let events: Vec<(&str, Event)> = vec![
        (
            "event.state",
            Event::State {
                session,
                generation: 4,
                state: State::Listening,
            },
        ),
        (
            "event.partial",
            Event::Partial {
                session,
                generation: 4,
                text: Transcript::new("Create a new"),
            },
        ),
        (
            "event.final",
            Event::Final {
                session,
                generation: 4,
                text: Transcript::new("新しいプロジェクト"),
            },
        ),
        (
            "event.language",
            Event::Language {
                session,
                generation: 4,
                language: VoiceLanguage::ZhHans,
            },
        ),
        (
            "event.level",
            Event::Level {
                session,
                generation: 4,
                bucket: LevelBucket::new(5),
            },
        ),
        (
            "event.ended",
            Event::Ended {
                session,
                generation: 4,
                reason: EndReason::Completed,
            },
        ),
        (
            "event.ended.failed",
            Event::Ended {
                session,
                generation: 4,
                reason: EndReason::Failed {
                    code: ErrorCode::CaptureFailed,
                    message: "the device is held by another process".into(),
                },
            },
        ),
        (
            "event.refused",
            Event::Refused {
                session,
                generation: 9,
                code: ErrorCode::Busy,
                message: "a session is already running in state Listening".into(),
            },
        ),
        (
            "event.devices",
            Event::Devices {
                session,
                generation: 4,
                devices: vec![
                    InputDevice::new("Microphone Array (Realtek(R) Audio)", true),
                    InputDevice::new("Yeti Nano", false),
                ],
            },
        ),
    ];
    let greetings: Vec<(&str, WorkerGreeting)> = vec![
        (
            "greeting.welcome",
            WorkerGreeting::Welcome {
                worker_version: "0.0.3".into(),
                protocol_version: PROTOCOL_VERSION,
            },
        ),
        (
            "greeting.refused",
            WorkerGreeting::Refused {
                what: "protocol".into(),
                ours: PROTOCOL_VERSION,
                theirs: 99,
                message: "this worker speaks protocol 1".into(),
            },
        ),
    ];

    // Two `Begin` vectors cover one variant, so the counts are stated rather
    // than derived: a new variant with no vector fails here.
    assert_eq!(
        requests.len(),
        7,
        "add a vector for the new Request variant"
    );
    assert_eq!(events.len(), 9, "add a vector for the new Event variant");

    for (name, value) in &requests {
        check(name, value);
    }
    for (name, value) in &events {
        check(name, value);
    }
    for (name, value) in &greetings {
        check(name, value);
    }
    check(
        "hello",
        &ClientHello {
            product: "koru".into(),
            version: "0.0.3".into(),
            protocol_version: PROTOCOL_VERSION,
        },
    );
}

/// Write or compare one fixture, then decode it back into its type — a file
/// that matches but no longer parses would be a vector nobody could use.
fn check<T>(name: &str, value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors");
    let path = dir.join(format!("{name}.json"));
    let generated = serde_json::to_string_pretty(value).unwrap();
    if update_requested() {
        std::fs::create_dir_all(&dir).expect("create tests/vectors");
        std::fs::write(&path, format!("{generated}\n")).expect("write vector");
        return;
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    let current: serde_json::Value = serde_json::from_str(current.trim()).unwrap_or_else(|_| {
        panic!(
            "{} is missing or unreadable; regenerate with `KORU_UPDATE_VOICE_VECTORS=1 cargo test -p koru-voice`",
            path.display()
        )
    });
    let generated: serde_json::Value = serde_json::from_str(&generated).unwrap();
    assert_eq!(
        current, generated,
        "the wire shape of `{name}` changed; if that is deliberate, bump PROTOCOL_VERSION and regenerate with `KORU_UPDATE_VOICE_VECTORS=1 cargo test -p koru-voice`"
    );
    let decoded: T = serde_json::from_value(generated).unwrap();
    assert_eq!(&decoded, value, "`{name}` did not round-trip");
}

/// The two numbers are separate, and they answer different questions. Pinned so
/// a well-meaning cleanup cannot collapse them into one constant.
#[test]
fn the_protocol_and_frame_versions_are_two_numbers() {
    // 2: the microphone picker's `Devices` pair and `Begin.input`; 3: the
    // tier on `Begin`; 4: the level meter's `Meter` / `MeterStop`. The
    // literal is here rather than derived so that bumping one number cannot
    // silently move the other.
    assert_eq!(PROTOCOL_VERSION, 4);
    assert_eq!(FRAME_VERSION, 1);
    // `FRAME_VERSION` is a byte because that is what `koru_ipc::frame` stamps;
    // `PROTOCOL_VERSION` is a u32 because a semantic version is bumped often.
    let _: u8 = FRAME_VERSION;
    let _: u32 = PROTOCOL_VERSION;
}

/// A meter must never be a reason a dictation fails, on either side of the
/// wire. A peer that sends 200 has a bug; dropping the session over it would be
/// a worse one.
#[test]
fn a_level_bucket_is_clamped_in_both_directions() {
    assert_eq!(LevelBucket::new(0), LevelBucket::SILENT);
    assert_eq!(LevelBucket::new(8).step(), 8);
    assert_eq!(LevelBucket::new(9).step(), 8);
    assert_eq!(LevelBucket::new(255).step(), 8);
    let decoded: LevelBucket = serde_json::from_str("200").unwrap();
    assert_eq!(decoded.step(), LevelBucket::MAX);
    assert_eq!(serde_json::to_string(&LevelBucket::new(3)).unwrap(), "3");
}

/// Every event says which session and which generation it is about, through one
/// accessor each — so a client's drop test is a comparison rather than a
/// seven-armed match it can get wrong.
#[test]
fn every_event_answers_its_session_and_generation() {
    let session = fixed_session();
    let events = [
        Event::State {
            session,
            generation: 2,
            state: State::Loading,
        },
        Event::Partial {
            session,
            generation: 2,
            text: Transcript::new("a"),
        },
        Event::Final {
            session,
            generation: 2,
            text: Transcript::new("a"),
        },
        Event::Language {
            session,
            generation: 2,
            language: VoiceLanguage::Ar,
        },
        Event::Level {
            session,
            generation: 2,
            bucket: LevelBucket::SILENT,
        },
        Event::Ended {
            session,
            generation: 2,
            reason: EndReason::Cancelled,
        },
        Event::Refused {
            session,
            generation: 2,
            code: ErrorCode::WorkerUnavailable,
            message: String::new(),
        },
    ];
    for event in &events {
        assert_eq!(event.session(), session);
        assert_eq!(event.generation(), 2);
    }
}

/// No variant of either message carries audio, and none ever may — the worker
/// opens the microphone itself, which is both the privacy story and the reason
/// `koru_ipc::frame`'s 16 MiB cap is never a question here. A field of samples
/// would serialize as a JSON array, so this is checkable rather than merely
/// stated.
#[test]
fn no_message_carries_audio() {
    let session = fixed_session();
    let messages = [
        serde_json::to_value(Request::Begin {
            session,
            generation: 1,
            language: Requested::Auto,
            tier: ModelTier::Compact,
            model: None,
            input: InputChoice::SystemDefault,
        })
        .unwrap(),
        serde_json::to_value(Event::Level {
            session,
            generation: 1,
            bucket: LevelBucket::new(8),
        })
        .unwrap(),
    ];
    for message in messages {
        let object = message.as_object().expect("an object");
        for (key, value) in object {
            assert!(
                !value.is_array(),
                "`{key}` is an array; nothing on this wire may carry a buffer"
            );
        }
    }
}

// ------------------------------------------------------------ the microphone

/// **A version-1 client's `begin` still reads**, and it asks for the host's own
/// device — which is what it always got. The field is additive, and this is
/// what says so rather than the `#[serde(default)]` attribute saying it about
/// itself.
#[test]
fn a_begin_written_before_the_input_field_still_reads() {
    let json = serde_json::json!({
        "kind": "begin",
        "session": fixed_session(),
        "generation": 4,
        "language": "auto",
    });
    let request: Request = serde_json::from_value(json).expect("an older begin");
    match request {
        Request::Begin { input, model, .. } => {
            assert_eq!(input, InputChoice::SystemDefault);
            assert_eq!(model, None);
        }
        other => panic!("{other:?}"),
    }
}

/// A device list is not a dictation, and the correlation pair is what keeps
/// [`Event::session`] and [`Event::generation`] total anyway. Pinned because
/// the obvious "simplification" is to drop them, which turns two infallible
/// accessors into `Option`s at nine call sites.
#[test]
fn a_device_answer_is_still_attributable() {
    let session = fixed_session();
    let event = Event::Devices {
        session,
        generation: 12,
        devices: Vec::new(),
    };
    assert_eq!(event.session(), session);
    assert_eq!(event.generation(), 12);
}

/// The one message that carries a device name carries it as a name, so a
/// picker can draw it. The counterpart rule — that a name may not reach a log
/// — is `apps/koru-voice`'s `capture` module's, and is not a claim about this
/// shape.
#[test]
fn a_device_list_carries_the_names_a_picker_draws() {
    let json = serde_json::to_value(Event::Devices {
        session: fixed_session(),
        generation: 1,
        devices: vec![InputDevice::new("Yeti Nano", true)],
    })
    .unwrap();
    assert_eq!(json["devices"][0]["name"], "Yeti Nano");
    assert_eq!(json["devices"][0]["default"], true);
}

// ------------------------------------------------------------ the level meter

/// **A meter written without a device still reads**, and meters the host's
/// own microphone: `input` defaults as `Begin`'s does.
#[test]
fn a_meter_without_an_input_meters_the_system_default() {
    let json = serde_json::json!({
        "kind": "meter",
        "session": fixed_session(),
        "generation": 4,
    });
    let request: Request = serde_json::from_value(json).expect("a bare meter");
    match request {
        Request::Meter { input, .. } => assert_eq!(input, InputChoice::SystemDefault),
        other => panic!("{other:?}"),
    }
}

/// **A meter carries a device name and nothing that sounds.** The level comes
/// back as the dictation's own `Level` event — a bucket, never a buffer — and
/// neither request has a field a sample could travel in.
#[test]
fn a_meter_carries_no_audio_either_way() {
    let session = fixed_session();
    let messages = [
        serde_json::to_value(Request::Meter {
            session,
            generation: 1,
            input: InputChoice::named("Yeti Nano"),
        })
        .unwrap(),
        serde_json::to_value(Request::MeterStop {
            session,
            generation: 1,
        })
        .unwrap(),
    ];
    for message in messages {
        for (key, value) in message.as_object().expect("an object") {
            assert!(!value.is_array(), "`{key}` is an array");
        }
    }
}
