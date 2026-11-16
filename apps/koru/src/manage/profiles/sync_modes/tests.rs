// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn sync_modes_late_result_cannot_replace_another_profiles_settings() {
    let a = ProfileId::new();
    let b = ProfileId::new();
    let mut c = ProfilesController::new();
    c.selected = Some(b);
    c.sync_modes.target = Some(b);
    let (tx, rx) = mpsc::channel();
    c.sync_modes.receiver = Some(rx);
    tx.send((
        a,
        Ok(ProfileTransportSettings {
            mode: "cloud".into(),
            explicit_choice: true,
            revision: 2,
            workspaces: vec![],
        }),
        false,
    ))
    .unwrap();
    c.maintain_sync_modes(Instant::now());
    assert!(c.sync_settings().is_none());
    assert!(c.cloud.set_request.is_none());
}

#[test]
fn sync_modes_pending_save_disables_choices_and_disconnection_reports_failure() {
    let mut c = ProfilesController::new();
    c.selected = Some(ProfileId::new());
    c.set_sync_settings_for_test(ProfileTransportSettings {
        mode: "p2p".into(),
        explicit_choice: false,
        revision: 1,
        workspaces: vec![],
    });
    assert!(c.sync_mode_enabled(None, TransportMode::LocalOnly));
    let (tx, rx) = mpsc::channel();
    c.sync_modes.receiver = Some(rx);
    c.sync_modes.saving = true;
    assert!(!c.sync_mode_enabled(None, TransportMode::LocalOnly));
    drop(tx);
    c.maintain_sync_modes(Instant::now());
    assert!(!c.sync_modes.saving);
    assert!(c.sync_notice().is_some());
    assert_eq!(c.sync_settings().unwrap().mode, "p2p");
}
