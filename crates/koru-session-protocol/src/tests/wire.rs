// SPDX-License-Identifier: MPL-2.0

use std::{fmt::Debug, str::FromStr};

use serde::{Serialize, de::DeserializeOwned};

use crate::*;

fn round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let json = serde_json::to_string(value).unwrap();
    assert_eq!(&serde_json::from_str::<T>(&json).unwrap(), value, "{json}");
}

#[test]
fn profile_uses_argv_without_a_command_line_or_credentials() {
    let profile = SessionProfile {
        id: SessionProfileKey::Custom(super::id("88888888-8888-4888-8888-888888888888")),
        kind: ProfileKind::Custom,
        display: ProfileDisplay {
            name: "Review shell".into(),
            icon: IconName::from_str("terminal").unwrap(),
        },
        executable: ExecutableSpec {
            program: "pwsh".into(),
            argv: vec!["-NoLogo".into(), "space quote ' ; $(literal)".into()],
        },
        working_directory: WorkingDirectoryPolicy::Explicit {
            path: "C:\\work tree".into(),
        },
        input: InputCapabilities {
            text: true,
            raw_bytes: true,
            terminal_keys: true,
            paste: true,
            files: true,
            images: true,
        },
        attachment_delivery: AttachmentDeliveryPolicy::QuotedPath {
            shell: ShellQuoting::PowerShell,
        },
        declared_capabilities: CapabilitySet::default(),
        provider: None,
        availability: ExecutableAvailability::Available { version: None },
    };
    round_trip(&profile);
    let json = serde_json::to_string(&profile).unwrap();
    assert!(!json.contains("command_line"));
    assert!(!json.contains("credential"));
    assert!(!json.contains("environment"));
    let mut value = serde_json::to_value(&profile).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("future_field".into(), true.into());
    assert_eq!(
        serde_json::from_value::<SessionProfile>(value).unwrap(),
        profile
    );
    let mut legacy = serde_json::to_value(&profile).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("attachment_delivery");
    assert_eq!(
        serde_json::from_value::<SessionProfile>(legacy)
            .unwrap()
            .attachment_delivery,
        AttachmentDeliveryPolicy::Unsupported
    );
}

#[test]
fn behavior_enums_reject_unknown_variants() {
    assert!(serde_json::from_str::<SessionEvent>(r#"{"kind":"future_success"}"#).is_err());
    assert!(serde_json::from_str::<RawCommand>(r#"{"kind":"write_image","bytes":[]}"#).is_err());
    assert!(serde_json::from_str::<ProcessState>(r#"{"kind":"detached"}"#).is_err());
}

#[test]
fn unknown_error_codes_remain_explicit_failures() {
    let error: SessionFailure =
        serde_json::from_str(r#"{"code":"future_provider_failure","message":"provider declined"}"#)
            .unwrap();
    assert_eq!(
        error.code,
        ErrorCode::Unknown("future_provider_failure".into())
    );
    assert_eq!(error.code.class(), ErrorClass::Internal);
}

#[test]
fn every_stable_error_code_is_unique_snake_case_and_classed() {
    let mut names = std::collections::BTreeSet::new();
    for code in ErrorCode::ALL {
        assert!(names.insert(code.as_str()), "duplicate {}", code.as_str());
        assert!(
            code.as_str()
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        );
        let _ = code.class();
        round_trip(code);
    }
}

#[test]
fn every_raw_command_has_a_stable_tag_and_round_trips() {
    let commands = vec![
        RawCommand::WriteBytes {
            bytes: vec![0, 1, 255],
        },
        RawCommand::WriteText {
            text: "echo α".into(),
        },
        RawCommand::SendKey {
            key: KeyStroke {
                key: TerminalKey::Character { text: "c".into() },
                modifiers: KeyModifiers {
                    control: true,
                    ..KeyModifiers::default()
                },
            },
        },
        RawCommand::SendMouse {
            event: TerminalMouseEvent {
                action: MouseAction::Press,
                button: Some(MouseButton::Left),
                column: 4,
                row: 2,
                modifiers: KeyModifiers::default(),
            },
        },
        RawCommand::SetFocus { focused: true },
        RawCommand::PasteText {
            text: "one bulk paste".into(),
        },
        RawCommand::Interrupt,
        RawCommand::Resize {
            size: GridSize::new(120, 40).unwrap(),
        },
        RawCommand::Terminate,
    ];
    for command in commands {
        command.validate().unwrap();
        round_trip(&command);
        assert!(
            serde_json::to_string(&command)
                .unwrap()
                .contains("\"kind\"")
        );
    }
    assert_eq!(
        RawCommand::SetFocus { focused: true }.minimum_protocol_version(),
        2
    );
    assert_eq!(RawCommand::Interrupt.minimum_protocol_version(), 1);
    assert_eq!(
        RawCommand::Resize {
            size: GridSize {
                columns: 0,
                rows: 40
            }
        }
        .validate(),
        Err(ErrorCode::InvalidSize)
    );
    assert_eq!(
        RawCommand::SendMouse {
            event: TerminalMouseEvent {
                action: MouseAction::Press,
                button: None,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::default(),
            }
        }
        .validate(),
        Err(ErrorCode::InvalidRequest)
    );
}

#[test]
fn lease_gated_command_round_trips_without_leaking_the_token_in_debug() {
    let command = SessionCommand {
        idempotency_key: super::idempotency_key(),
        session_id: super::session_id(),
        controller_lease: super::lease(),
        action: RawCommand::Interrupt,
    };
    round_trip(&command);
    assert!(format!("{command:?}").contains("<redacted>"));
}

#[test]
fn event_families_round_trip() {
    round_trip(&SequencedSessionEvent {
        session_id: super::session_id(),
        sequence: super::seq(1),
        event: SessionEvent::TitleChanged {
            title: "مرحبا".into(),
        },
    });
    round_trip(&SequencedAgentEvent {
        session_id: super::session_id(),
        sequence: super::seq(2),
        event: AgentEvent::MessageDelta {
            turn_id: super::turn_id(),
            text: "streamed text".into(),
        },
    });
    let output = RawOutputChunk {
        session_id: super::session_id(),
        sequence: super::seq(3),
        bytes: vec![0, 27, 255],
    };
    round_trip(&output);
    let debug = format!("{output:?}");
    assert!(debug.contains("byte_count"));
    assert!(!debug.contains("255"));
}

#[test]
fn built_in_profile_and_capability_tokens_are_unique_and_valid() {
    let built_ins: std::collections::BTreeSet<_> = built_in_profile::ALL.iter().copied().collect();
    assert_eq!(built_ins.len(), built_in_profile::ALL.len());
    for key in built_in_profile::ALL {
        BuiltInProfileKey::from_str(key).unwrap();
    }
    let capabilities: std::collections::BTreeSet<_> = capability::ALL.iter().copied().collect();
    assert_eq!(capabilities.len(), capability::ALL.len());
    for name in capability::ALL {
        CapabilityName::from_str(name).unwrap();
    }
}

/// **A refused version says what was found and what is needed**, and a
/// refusal without them is byte-for-byte the shape older peers write, so
/// either side may be the older one.
#[test]
fn an_incompatible_row_carries_the_versions_and_stays_readable_by_older_peers() {
    let bare = ExecutableAvailability::incompatible(ErrorCode::ProviderIncompatible);
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        serde_json::json!({"kind": "incompatible", "code": "provider_incompatible"})
    );
    let old: ExecutableAvailability =
        serde_json::from_str(r#"{"kind":"incompatible","code":"provider_incompatible"}"#).unwrap();
    assert_eq!(old, bare);

    let refused = ExecutableAvailability::Incompatible {
        code: ErrorCode::ProviderIncompatible,
        found: Some("1.18.25".into()),
        needed: Some(VersionNeed::AtLeast("1.18.27".into())),
    };
    assert_eq!(
        serde_json::to_value(&refused).unwrap(),
        serde_json::json!({
            "kind": "incompatible",
            "code": "provider_incompatible",
            "found": "1.18.25",
            "needed": {"kind": "at_least", "version": "1.18.27"}
        })
    );
    round_trip(&refused);
    round_trip(&ExecutableAvailability::Incompatible {
        code: ErrorCode::ProviderIncompatible,
        found: None,
        needed: Some(VersionNeed::Reviewed("0.166.0".into())),
    });
}

/// Only a reviewed list can be outgrown, and only by a release newer than
/// its newest entry.
#[test]
fn a_reviewed_list_is_outgrown_only_by_a_newer_release() {
    let reviewed = VersionNeed::Reviewed("0.166.0".into());
    assert!(reviewed.outgrown_by("0.167.0"));
    assert!(reviewed.outgrown_by("1.0"));
    assert!(!reviewed.outgrown_by("0.166.0"));
    assert!(!reviewed.outgrown_by("0.155.0"));
    assert!(!reviewed.outgrown_by("garbage"));
    assert!(!VersionNeed::AtLeast("1.18.27".into()).outgrown_by("9.0.0"));
    assert!(version_at_least("1.0.34", "1.0.25"));
    assert!(version_at_least("2.0", "2.0.0"));
    assert!(version_at_least("0.76.1-beta", "0.76.0"));
    assert!(!version_at_least("1.18.25", "1.18.27"));
}

#[test]
fn a_reviewed_need_names_the_newest_entry_in_any_order() {
    assert_eq!(
        VersionNeed::reviewed(["0.150.1", "0.166.0", "0.158.0"]),
        Some(VersionNeed::Reviewed("0.166.0".into()))
    );
    assert_eq!(VersionNeed::reviewed([]), None);
}
