// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The conflict lands on the card, whose primary offers Manage in every
/// language; pressing it finishes the card and raises `ManageProfiles` once,
/// keeping the creation id.
#[test]
fn conflicting_identity_offers_manage_without_republishing_or_replacing_the_id() {
    for language in koru_i18n::Lang::ALL {
        let mut view = OnboardingView::default();
        view.language = language;
        let id = view.local_creation_id;
        view.creation_conflict();
        assert_eq!(view.screen, Screen::Setup);
        assert!(view.controls().contains(&Hit::Primary));
        assert_eq!(
            view.setup_primary_label(),
            koru_i18n::t(language, koru_i18n::Key::ProfileCreationManage)
        );
        view.activate(Hit::Primary);
        assert_eq!(view.take_action(), Some(Action::ManageProfiles));
        assert_eq!(view.local_creation_id, id);
        assert!(view.setup_finishing);
        view.activate(Hit::Primary);
        assert!(
            view.take_action().is_none(),
            "opening Manage has one pending action"
        );
    }
}

#[test]
fn failed_manage_open_preserves_the_conflict_and_cannot_retry_creation() {
    let mut view = OnboardingView::default();
    view.creation_conflict();
    view.activate(Hit::Primary);
    view.take_action();
    view.creation_failed("Store is unavailable");
    view.activate(Hit::Primary);
    assert_eq!(view.take_action(), Some(Action::ManageProfiles));
}
