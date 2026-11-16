// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::collections::BTreeSet;

/// The tokens are matched by a caller and printed in a log, so a duplicate or a
/// stray capital is a rename waiting to happen.
#[test]
fn every_code_is_lowercase_ascii_and_unique() {
    let mut seen = BTreeSet::new();
    for code in ModelErrorCode::ALL {
        let token = code.as_str();
        assert!(
            !token.is_empty()
                && token
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
            "{token:?} is not lowercase ASCII with underscores"
        );
        assert!(seen.insert(token), "{token} is declared twice");
    }
}

/// `as_str` and serde agree, so the wire spelling and the log spelling cannot
/// come apart.
#[test]
fn serde_writes_the_stable_token() {
    for code in ModelErrorCode::ALL {
        assert_eq!(
            serde_json::to_string(&code).unwrap(),
            format!("\"{}\"", code.as_str())
        );
        assert_eq!(ModelErrorCode::parse(code.as_str()), Some(code));
    }
}

#[test]
fn an_io_error_is_io_with_its_words() {
    let error: ModelError = std::io::Error::other("the disk said no").into();
    assert_eq!(error.code, ModelErrorCode::Io);
    assert_eq!(error.to_string(), "io: the disk said no");
}
