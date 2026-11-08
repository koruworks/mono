// SPDX-License-Identifier: MPL-2.0

//! Help reads one status/offer snapshot without starting a worker or probing disk.

use super::*;

fn manifest(build_number: u64) -> UpdateManifest {
    UpdateManifest {
        schema_version: 1,
        channel: ReleaseChannel::Nightly,
        platform: "windows".into(),
        architecture: "x86_64".into(),
        version: format!("0.0.4-nightly.{build_number}"),
        build_number,
        published_at: "2026-11-07T00:00:00Z".into(),
        commit: Some("abc1234".into()),
        source: None,
        minimum_updater_version: Some(1),
        minimum_supported_build: None,
        required: false,
        release_notes: crate::manifest::ReleaseNotes {
            title: format!("Release {build_number}"),
            summary: vec!["Faster opening.".into(), "Clearer update progress.".into()],
            url: Some(format!("https://example.invalid/releases/{build_number}")),
        },
        package: crate::manifest::UpdatePackage {
            url: format!("https://example.invalid/{build_number}.zip"),
            file_name: format!("{build_number}.zip"),
            size: 4_096,
            sha256: "a".repeat(64),
            signature: "test signature".into(),
            format: crate::manifest::PackageFormat::Zip,
        },
    }
}

fn controller(status: UpdateStatus, cached: Option<UpdateManifest>) -> UpdateController {
    // Construct the private state directly. The public constructor loads disk
    // state; status_with_offer only reads this mutex and never uses these paths.
    UpdateController {
        shared: Arc::new(Mutex::new(Shared {
            base_url: "https://example.invalid".into(),
            rehearsal_package: None,
            paths: UpdatePaths {
                config_dir: PathBuf::new(),
                data_dir: PathBuf::new(),
                cache_dir: PathBuf::new(),
                logs_dir: PathBuf::new(),
            },
            state: UpdateState {
                last_manifest: cached,
                ..UpdateState::default()
            },
            channel: ReleaseChannel::Nightly,
            session_deferred: None,
            session_newer_dismissed: false,
            status,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            notify: None,
        })),
    }
}

#[test]
fn status_and_offer_keep_the_complete_cached_release_during_progress() {
    let offer = manifest(16);
    let controller = controller(UpdateStatus::Available(offer.clone()), Some(offer.clone()));
    for status in [
        UpdateStatus::Downloading {
            received: 1_024,
            total: 4_096,
        },
        UpdateStatus::Verifying,
        UpdateStatus::Installing,
        UpdateStatus::Deferred(16),
        UpdateStatus::Failed(UpdateFailure::new(
            "Download failed.",
            "connection reset",
            true,
        )),
    ] {
        controller.shared.lock().unwrap().status = status.clone();

        assert_eq!(
            controller.status_with_offer(),
            (status, Some(offer.clone()))
        );
        let shared = controller.shared.lock().unwrap();
        assert!(
            shared.worker.is_none(),
            "reading Help must not start a worker"
        );
        assert_eq!(shared.state.last_manifest.as_ref(), Some(&offer));
    }
}

#[test]
fn available_and_ready_use_their_own_offer_instead_of_an_older_cache() {
    let offer = manifest(17);
    let stale = manifest(16);
    for cached in [Some(stale), None] {
        for status in [
            UpdateStatus::Available(offer.clone()),
            UpdateStatus::ReadyToInstall {
                manifest: offer.clone(),
                package: PathBuf::from("not-opened.zip"),
            },
        ] {
            let controller = controller(status.clone(), cached.clone());

            assert_eq!(
                controller.status_with_offer(),
                (status, Some(offer.clone()))
            );
            assert_eq!(
                controller.shared.lock().unwrap().state.last_manifest,
                cached
            );
        }
    }
}

#[test]
fn cached_release_is_not_an_offer_in_idle_checking_or_terminal_states() {
    let cached = manifest(16);
    for status in [
        UpdateStatus::Idle,
        UpdateStatus::Checking,
        UpdateStatus::UpToDate,
        UpdateStatus::InstalledBuildNewer { available: 12 },
    ] {
        let controller = controller(status.clone(), Some(cached.clone()));

        assert_eq!(controller.status_with_offer(), (status, None));
        assert_eq!(
            controller
                .shared
                .lock()
                .unwrap()
                .state
                .last_manifest
                .as_ref(),
            Some(&cached),
            "hiding an offer does not erase the conditional-fetch cache"
        );
    }
}

#[test]
fn deferred_metadata_belongs_to_the_exact_deferred_build() {
    let cached = manifest(16);
    for deferred_build in [15, 17] {
        let status = UpdateStatus::Deferred(deferred_build);
        let controller = controller(status.clone(), Some(cached.clone()));

        assert_eq!(controller.status_with_offer(), (status, None));
    }
}

#[test]
fn progress_without_a_cached_release_does_not_invent_an_offer() {
    for status in [
        UpdateStatus::Downloading {
            received: 1,
            total: 2,
        },
        UpdateStatus::Verifying,
        UpdateStatus::Installing,
        UpdateStatus::Deferred(16),
        UpdateStatus::Failed(UpdateFailure::new("Check failed.", "no manifest", true)),
    ] {
        let controller = controller(status.clone(), None);

        assert_eq!(controller.status_with_offer(), (status, None));
    }
}
