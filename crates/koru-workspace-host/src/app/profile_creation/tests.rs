// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn publication_conflict_is_distinct_from_a_retryable_transport_failure() {
    let result = create_onboarding_profile(
        None,
        koru_core::ProfileId::new(),
        "Personal",
        "Notes",
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

/// **A broken installation says it needs repair, in the reader's language**
/// (welcome Q-D): the retryable message is the `SetupInstallationNeedsRepair`
/// row, with no template name and no technical cause in it — there is no
/// other template to choose, and the cause goes to the log.
#[test]
fn installation_needs_repair_is_localized_and_names_no_template() {
    for lang in koru_i18n::Lang::ALL {
        let result = koru_i18n::with_current(lang, || {
            installation_needs_repair("packaged Workspace template empty is missing")
        });
        let OnboardingCreation::Retryable { message } = result else {
            panic!("{}: a missing packaged file is retryable", lang.tag());
        };
        assert_eq!(
            message,
            koru_i18n::t(lang, koru_i18n::Key::SetupInstallationNeedsRepair),
            "{}",
            lang.tag()
        );
        for word in ["empty", "template", "Template", "packaged"] {
            assert!(
                !message.contains(word),
                "{}: the repair sentence names {word:?}: {message}",
                lang.tag()
            );
        }
    }
}
