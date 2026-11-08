// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::{Timestamp, model::Workspace};

fn window(id: u64) -> WindowId {
    WindowId::from(id)
}

#[test]
fn removal_precedes_cloud_and_discards_inflight_landing_until_known_cleared() {
    let mut waits = Waits::default();
    let target = window(1);
    let attempt = waits.begin(target, ProfileId::new(), None);
    waits.observe(target, ProfileDelivery::Delivered);
    let (_, reply) = waits.request(target).unwrap();
    let mark = koru_store_core::profile_removal::RemovalMark {
        attempt: koru_store_core::ids::RemovalAttemptId::new(),
        kind: koru_store_core::profile_removal::RemovalKind::Local,
        started_at: Timestamp::from_millis(1),
        last_step_at: Timestamp::from_millis(1),
        stopped: true,
    };
    assert!(waits.observe_removal(target, Some(Some(mark.clone()))));
    assert!(reply.send(Ok(active(attempt.profile))).is_err());
    assert!(waits.poll(target).is_none());
    assert!(waits.request(target).is_none());
    assert!(
        waits.observe_removal(target, None),
        "unknown transport cannot abandon removal"
    );
    assert!(
        matches!(&waits.get(target).unwrap().presentation.reading, WaitReading::Removing { mark: held } if held == &mark)
    );
    assert!(!waits.observe_removal(target, Some(None)));
    waits.observe(target, ProfileDelivery::Unknown);
    assert!(
        waits.request(target).is_none(),
        "Keep resumes ordinary Cloud-gated selection"
    );
    waits.observe(target, ProfileDelivery::Delivered);
    assert!(waits.request(target).is_some());
}

fn active(profile: ProfileId) -> EnsuredWorkspace {
    EnsuredWorkspace::Active {
        workspace: Workspace::new(
            koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
            "Ready",
            Timestamp::from_millis(1),
        )
        .unwrap(),
        issues: vec![],
    }
}

#[test]
fn only_authorized_delivery_starts_one_command() {
    let mut waits = Waits::default();
    let target = window(1);
    let attempt = waits.begin(target, ProfileId::new(), None);
    for delivery in [
        ProfileDelivery::Unknown,
        ProfileDelivery::InProgress {
            enrollment: false,
            counts: Some((0, 1)),
        },
        ProfileDelivery::Failed {
            phase: FailurePhase::Content,
            code: Some("session_revoked".into()),
            retry_at_millis: None,
            automatic: false,
        },
    ] {
        waits.observe(target, delivery);
        assert!(waits.request(target).is_none());
    }
    waits.observe(target, ProfileDelivery::Delivered);
    let (issued, _reply) = waits.request(target).unwrap();
    assert_eq!(issued, attempt);
    assert!(waits.request(target).is_none());
    assert!(matches!(
        waits.get(target).unwrap().presentation.reading,
        WaitReading::Preparing
    ));
}

#[test]
fn retired_open_content_resolves_locally_and_cancel_cannot_return_to_it() {
    let mut waits = Waits::default();
    let target = window(1);
    let profile = ProfileId::new();
    let attempt = waits.begin_post_removal(target, profile);
    waits.observe(target, ProfileDelivery::Unknown);
    let (issued, reply) = waits.request(target).unwrap();
    assert_eq!(issued, attempt);
    assert!(matches!(
        waits.get(target).unwrap().presentation.reading,
        WaitReading::Preparing
    ));
    reply
        .send(Ok(EnsuredWorkspace::Pending { workspaces: vec![] }))
        .unwrap();
    assert!(waits.poll(target).is_none());
    assert!(
        waits.request(target).is_none(),
        "pending content is not a retry loop"
    );
    assert!(waits.leave(target).unwrap().origin.is_none());
    let next = waits.begin(target, ProfileId::new(), None);
    assert!(next.sequence > attempt.sequence);
    waits.observe(target, ProfileDelivery::Unknown);
    assert!(
        waits.request(target).is_none(),
        "post-removal permission does not follow a switch"
    );
}

#[test]
fn cancel_switch_and_close_drop_receivers_and_never_reuse_an_attempt() {
    let mut waits = Waits::default();
    let target = window(1);
    let profile = ProfileId::new();
    let origin = Place {
        profile: ProfileId::new(),
        workspace: WorkspaceId::new(),
    };
    let first = waits.begin(target, profile, Some(origin));
    waits.observe(target, ProfileDelivery::NotApplicable);
    let (_, reply) = waits.request(target).unwrap();
    assert_eq!(waits.leave(target).unwrap().origin, Some(origin));
    assert!(reply.send(Ok(active(profile))).is_err());
    assert!(!waits.is_current(first));
    let second = waits.begin(target, profile, None);
    assert!(second.sequence > first.sequence);
    assert!(!waits.is_current(WaitAttempt {
        window: window(2),
        ..second
    }));
    assert!(!waits.is_current(WaitAttempt {
        profile: ProfileId::new(),
        ..second
    }));
    waits.observe(target, ProfileDelivery::Delivered);
    let (_, reply) = waits.request(target).unwrap();
    let third = waits.begin(target, ProfileId::new(), None);
    assert!(reply.send(Ok(active(profile))).is_err());
    assert!(!waits.is_current(second));
    assert!(waits.is_current(third));
    waits.clear();
    assert!(!waits.is_current(third));
}

#[test]
fn pending_waits_for_changed_catalog_including_a_push_before_settlement() {
    let mut waits = Waits::default();
    let target = window(1);
    waits.begin(target, ProfileId::new(), None);
    waits.observe(target, ProfileDelivery::NotApplicable);
    let (_, reply) = waits.request(target).unwrap();
    reply
        .send(Ok(EnsuredWorkspace::Pending { workspaces: vec![] }))
        .unwrap();
    assert!(waits.poll(target).is_none());
    for _ in 0..20 {
        waits.observe(target, ProfileDelivery::NotApplicable);
        assert!(waits.request(target).is_none());
        assert!(waits.poll(target).is_none());
    }
    waits.catalog_changed();
    let (_, reply) = waits.request(target).unwrap();
    waits.catalog_changed();
    reply
        .send(Ok(EnsuredWorkspace::Pending { workspaces: vec![] }))
        .unwrap();
    assert!(waits.poll(target).is_none());
    assert!(
        waits.request(target).is_some(),
        "the earlier push is not lost"
    );
}

#[test]
fn uncertainty_requires_explicit_retry_and_wrong_owner_never_lands() {
    let mut waits = Waits::default();
    let target = window(1);
    let attempt = waits.begin(target, ProfileId::new(), None);
    waits.observe(target, ProfileDelivery::NotApplicable);
    let (_, reply) = waits.request(target).unwrap();
    reply
        .send(Ok(EnsuredWorkspace::Uncertain { issues: vec![] }))
        .unwrap();
    assert!(waits.poll(target).is_none());
    waits.catalog_changed();
    assert!(waits.request(target).is_none());
    waits.retry(attempt);
    let (_, reply) = waits.request(target).unwrap();
    reply.send(Ok(active(ProfileId::new()))).unwrap();
    assert!(waits.poll(target).is_none());
    assert!(matches!(&waits.get(target).unwrap().presentation.reading,
        WaitReading::Unavailable { cause } if cause == "profile_mismatch"));
    waits.retry(attempt);
    let (_, reply) = waits.request(target).unwrap();
    reply.send(Ok(active(attempt.profile))).unwrap();
    let (land, _) = waits.poll(target).unwrap();
    assert_eq!(land, attempt);
    assert!(waits.poll(target).is_none());
    waits.landing_failed(land);
    assert!(matches!(
        waits.get(target).unwrap().presentation.reading,
        WaitReading::Unavailable { .. }
    ));
}

#[test]
fn authority_change_discards_inflight_reply_and_dead_worker_exposes_retry() {
    let mut waits = Waits::default();
    let target = window(1);
    let attempt = waits.begin(target, ProfileId::new(), None);
    waits.observe(target, ProfileDelivery::Delivered);
    let (_, reply) = waits.request(target).unwrap();
    waits.observe(target, ProfileDelivery::Unknown);
    assert!(reply.send(Ok(active(attempt.profile))).is_err());
    assert!(waits.poll(target).is_none());
    waits.observe(target, ProfileDelivery::Delivered);
    let (_, reply) = waits.request(target).unwrap();
    drop(reply);
    assert!(waits.poll(target).is_none());
    assert!(matches!(
        waits.get(target).unwrap().presentation.reading,
        WaitReading::Unavailable { .. }
    ));
    assert!(waits.request(target).is_none());
    waits.retry(attempt);
    assert!(waits.request(target).is_some());
}
