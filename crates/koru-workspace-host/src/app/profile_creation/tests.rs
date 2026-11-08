// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn publication_conflict_is_distinct_from_a_retryable_transport_failure() {
    let result = create_onboarding_profile(
        None,
        koru_core::ProfileId::new(),
        "Personal",
        "Notes",
        "empty",
        None,
        None,
        None,
        None,
        |_| {
            Err(koru_store_core::Error::bare(
                koru_store_core::Code::ProfileExists,
            ))
        },
    );
    assert!(matches!(result, OnboardingCreation::Conflict));
}
