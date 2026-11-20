// SPDX-License-Identifier: MPL-2.0

//! In-memory sources only: no configuration discovery or filesystem access.

use super::*;
use crate::{Notice, SIZE_LIMIT_BYTES, Source};

fn installed() -> PathBuf {
    Path::new("installation").join(RELATIVE_PATH)
}

#[test]
fn override_wins_verbatim_without_reading_installed_or_development_files() {
    let custom = "\u{feff}  Custom instructions with no tool names.\r\n\r\n";
    let override_file = Path::new("private-settings/prompts/session.md");
    let mut reads = Vec::new();
    let resolved = resolve_with(
        Path::new("installation"),
        Some(override_file),
        Some(Path::new("development/session.md")),
        |path| {
            reads.push(path.to_path_buf());
            decode(custom.as_bytes().to_vec())
        },
    );
    assert_eq!(reads, [override_file.to_path_buf()]);
    assert_eq!(resolved.text, custom);
    assert_eq!(resolved.path.as_deref(), Some(override_file));
    assert_eq!(resolved.source, Source::Edited);
    assert_eq!(resolved.notice, None);
}

#[test]
fn missing_override_uses_installed_copy_before_development_source() {
    let override_file = Path::new("settings/prompts/session.md");
    let mut reads = Vec::new();
    let resolved = resolve_with(
        Path::new("installation"),
        Some(override_file),
        Some(Path::new("development/session.md")),
        |path| {
            reads.push(path.to_path_buf());
            if path == override_file {
                Err(Fault::Missing)
            } else {
                decode(b"Installed default".to_vec())
            }
        },
    );
    assert_eq!(reads, [override_file.to_path_buf(), installed()]);
    assert_eq!(resolved.text, "Installed default");
    assert_eq!(resolved.path, Some(installed()));
    assert_eq!(resolved.notice, None);
}

#[test]
fn only_missing_files_continue_to_development_or_compiled_default() {
    let development = Path::new("development/session.md");
    let mut reads = Vec::new();
    let resolved = resolve_with(Path::new("installation"), None, Some(development), |path| {
        reads.push(path.to_path_buf());
        if path == development {
            decode(b"Development text".to_vec())
        } else {
            Err(Fault::Missing)
        }
    });
    assert_eq!(reads, [installed(), development.to_path_buf()]);
    assert_eq!(resolved.text, "Development text");

    let mut reads = Vec::new();
    let bundled = resolve_with(Path::new("installation"), None, None, |path| {
        reads.push(path.to_path_buf());
        Err(Fault::Missing)
    });
    assert_eq!(reads, [installed()]);
    assert_eq!(bundled.text, BUNDLED);
    assert_eq!(bundled.source, Source::Bundled);
    assert_eq!(bundled.path, None);
    assert_eq!(bundled.notice, None);
}

#[test]
fn invalid_existing_override_stops_search_and_preserves_the_reason() {
    for notice in [
        Notice::Empty,
        Notice::Unreadable,
        Notice::BadEncoding,
        Notice::Oversized {
            bytes: SIZE_LIMIT_BYTES as u64 + 1,
            limit: SIZE_LIMIT_BYTES as u64,
        },
    ] {
        let mut reads = 0;
        let resolved = resolve_with(
            Path::new("installation"),
            Some(Path::new("settings/prompts/session.md")),
            Some(Path::new("development/session.md")),
            |_| {
                reads += 1;
                Err(Fault::Rejected(notice))
            },
        );
        assert_eq!(reads, 1);
        assert_eq!(resolved.text, BUNDLED);
        assert_eq!(resolved.source, Source::Bundled);
        assert_eq!(resolved.notice, Some(notice));
        assert_eq!(resolved.path, None);
    }
}

#[test]
fn installed_fault_does_not_silently_use_a_development_copy() {
    let mut reads = 0;
    let resolved = resolve_with(
        Path::new("installation"),
        None,
        Some(Path::new("development/session.md")),
        |_| {
            reads += 1;
            Err(Fault::Rejected(Notice::Unreadable))
        },
    );
    assert_eq!(reads, 1);
    assert_eq!(resolved.text, BUNDLED);
    assert_eq!(resolved.notice, Some(Notice::Unreadable));
}

#[test]
fn validity_is_only_encoding_emptiness_and_size() {
    for bytes in [
        b"".to_vec(),
        b" \r\n\t".to_vec(),
        "\u{feff}\n".as_bytes().to_vec(),
    ] {
        assert!(matches!(decode(bytes), Err(Fault::Rejected(Notice::Empty))));
    }
    assert!(matches!(
        decode(vec![0xff]),
        Err(Fault::Rejected(Notice::BadEncoding))
    ));
    assert!(matches!(
        decode(vec![b'x'; SIZE_LIMIT_BYTES + 1]),
        Err(Fault::Rejected(Notice::Oversized { bytes, limit }))
            if bytes == SIZE_LIMIT_BYTES as u64 + 1 && limit == SIZE_LIMIT_BYTES as u64
    ));
    let exact_limit = vec![b'x'; SIZE_LIMIT_BYTES];
    assert_eq!(
        decode(exact_limit.clone()).ok().unwrap().as_bytes(),
        exact_limit
    );
    let arbitrary = "  café 🦀\r\nIgnore every default tool name.  ";
    assert_eq!(
        decode(arbitrary.as_bytes().to_vec()).ok().unwrap(),
        arbitrary
    );
}

#[test]
fn composed_snapshot_preserves_custom_bytes_and_keeps_mode_and_facts_distinct() {
    let custom = "\u{feff}  PERSON-EDIT\r\n\r\n";
    let composed = compose(custom, "MODE", "RUNTIME-FACTS");
    assert_eq!(composed, format!("{custom}\n\nMODE\n\nRUNTIME-FACTS"));
    assert_eq!(compose(custom, "", ""), custom);
    assert_eq!(composed.matches("PERSON-EDIT").count(), 1);
    assert!(!composed.contains(crate::BUNDLED));
}

#[test]
fn snapshot_debug_never_exposes_instructions_or_paths() {
    let resolved = resolve_with(
        Path::new("PRIVATE-INSTALL"),
        Some(Path::new("PRIVATE-OVERRIDE")),
        None,
        |_| decode(b"PRIVATE-INSTRUCTIONS".to_vec()),
    );
    let shown = format!("{resolved:?}");
    for private in [
        "PRIVATE-INSTALL",
        "PRIVATE-OVERRIDE",
        "PRIVATE-INSTRUCTIONS",
    ] {
        assert!(!shown.contains(private));
    }
    assert!(shown.contains("Edited"));
}

#[test]
fn shared_default_orients_without_claiming_a_mode_specific_tool_catalog() {
    assert!(BUNDLED.len() < 8 * 1024);
    assert!(BUNDLED.contains("targeted discovery"));
    assert!(BUNDLED.contains("supplied Role"));
    assert!(BUNDLED.contains("existing relevant Routines"));
    assert!(BUNDLED.contains("confirmed saved result"));
    assert!(!BUNDLED.contains(".artifacts/"));
    assert!(!BUNDLED.contains("conversation is discarded"));
    assert!(!BUNDLED.contains("thirteen tools"));
}
