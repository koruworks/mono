// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::collections::HashSet;

#[test]
fn every_code_is_in_all() {
    // `ALL` is what the docs and the OpenAPI table are generated from, so a
    // code missing from it is a code no client is told about. There is no
    // reflection over enum variants in Rust, so this is the guard: add a
    // variant, forget `ALL`, and the count assertion below fails.
    assert_eq!(Code::ALL.len(), 52, "a new Code was not added to Code::ALL");
}

#[test]
fn tokens_are_unique() {
    let seen: HashSet<&str> = Code::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(
        seen.len(),
        Code::ALL.len(),
        "two codes share a token; a client cannot tell them apart"
    );
}

#[test]
fn tokens_are_stable_ascii_snake_case() {
    // These cross the wire and are branched on. Never a display string, never
    // localized, never punctuated.
    for c in Code::ALL {
        let t = c.as_str();
        assert!(!t.is_empty(), "{c:?}");
        assert!(
            t.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
            "{t} is not a stable token"
        );
        assert!(!t.starts_with('_') && !t.ends_with('_'), "{t}");
    }
}

#[test]
fn every_code_has_a_class() {
    // Exhaustive by construction — `class()` matches without a wildcard, so a
    // new variant fails to compile rather than defaulting to Internal.
    for c in Code::ALL {
        let _ = c.class();
    }
}

#[test]
fn classes_are_transport_neutral() {
    assert_eq!(Code::WorkspaceNotFound.class(), Class::NotFound);
    assert_eq!(Code::Unauthenticated.class(), Class::Unauthenticated);
    assert_eq!(Code::Forbidden.class(), Class::Forbidden);
    assert_eq!(Code::ReadOnly.class(), Class::Conflict);
    // A document written by a newer build is a conflict, not a 404 and not an
    // internal error: the store understood the request and declined it.
    assert_eq!(Code::SchemaTooNew.class(), Class::Conflict);
    // Damaged state is Unavailable rather than Internal — it is a condition of
    // the data, and a client may usefully retry after a recovery.
    assert_eq!(Code::CorruptState.class(), Class::Unavailable);
}

#[test]
fn an_error_carries_its_code_and_a_developer_message() {
    let e = Error::new(Code::WorkspaceNotFound, "no row for the given id");
    assert_eq!(e.code(), Code::WorkspaceNotFound);
    assert_eq!(e.message(), "no row for the given id");
    assert_eq!(
        e.to_string(),
        "workspace_not_found: no row for the given id"
    );
}

#[test]
fn a_bare_error_displays_as_its_token_alone() {
    assert_eq!(Error::bare(Code::Forbidden).to_string(), "forbidden");
    assert!(Error::bare(Code::Forbidden).message().is_empty());
}

#[test]
fn the_code_survives_being_formatted() {
    // A log line is often all a bug report has, so the token must be in it.
    for c in Code::ALL {
        assert!(Error::bare(*c).to_string().contains(c.as_str()));
    }
}
