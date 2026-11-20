// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_delegate_preamble::{Notice, Source};

fn source(text: &str) -> Resolved {
    Resolved {
        text: text.to_owned().into(),
        source: Source::Edited,
        notice: None,
        path: Some("private/prompts/session.md".into()),
    }
}

#[test]
fn only_the_same_attempt_or_successor_reservation_reuses_its_snapshot() {
    let mut held = Snapshot::default();
    assert!(held.get("attempt:one").is_none());
    held.keep("attempt:one".into(), source("EXACT\r\nBYTES\n"));
    assert_eq!(held.get("attempt:one").unwrap().text, "EXACT\r\nBYTES\n");
    assert!(held.get("attempt:two").is_none());
    held.keep("child:wait-one".into(), source("NEW-SUCCESSOR"));
    assert!(held.get("attempt:one").is_none());
    assert_eq!(held.get("child:wait-one").unwrap().text, "NEW-SUCCESSOR");
    assert!(held.get("decision:wait-one").is_none());
}

#[test]
fn snapshot_retains_source_and_fallback_metadata_without_content_in_debug() {
    let mut source = source("PRIVATE-CUSTOM-INSTRUCTIONS");
    source.notice = Some(Notice::Unreadable);
    let mut held = Snapshot::default();
    held.keep("attempt:one".into(), source);
    let source = held.get("attempt:one").unwrap();
    assert_eq!(source.notice, Some(Notice::Unreadable));
    assert_eq!(source.source, Source::Edited);
    assert!(source.path.is_some());
    assert!(!format!("{source:?}").contains("PRIVATE-CUSTOM"));
}
