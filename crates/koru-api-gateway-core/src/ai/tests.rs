// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_retry_refusal_is_a_nonretryable_conflict() {
    let failure = Failure {
        code: "routine_retry_unsupported".into(),
        message: "".into(),
        retryable: false,
        details: Default::default(),
    };
    assert_eq!(status_for(&failure), StatusCode::CONFLICT);
    assert!(!failure.retryable);
}
