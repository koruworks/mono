// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::enrollment::{EnrollmentAuthorization, ProfileReport, Report, Snapshot};
use crate::identity::{AccountOwner, DirectoryRecord, DirectoryState};
use koru_agent_wire::EnrollmentState;
use std::collections::{BTreeMap, BTreeSet};

const FIRST: &str = "10000000-0000-4000-8000-00000000000a";
const SECOND: &str = "20000000-0000-4000-8000-00000000000b";

pub(super) fn authority_a() -> EnrollmentAuthorization {
    EnrollmentAuthorization {
        access: AccessToken("access-a".into()),
        owner: AccountOwner {
            account: "30000000-0000-4000-8000-000000000001".parse().unwrap(),
            auth_identity: "40000000-0000-4000-8000-000000000001".parse().unwrap(),
        },
        namespace: "a".repeat(64),
        record_version: 17,
        session_id: "50000000-0000-4000-8000-000000000001".into(),
    }
}

fn authority_b() -> EnrollmentAuthorization {
    EnrollmentAuthorization {
        access: AccessToken("access-b".into()),
        owner: AccountOwner {
            account: "30000000-0000-4000-8000-000000000002".parse().unwrap(),
            auth_identity: "40000000-0000-4000-8000-000000000002".parse().unwrap(),
        },
        record_version: 29,
        session_id: "50000000-0000-4000-8000-000000000002".into(),
        ..authority_a()
    }
}

pub(super) fn acknowledge(
    service: &mut ProductionAccountService,
    authority: EnrollmentAuthorization,
) {
    service.directory_namespace = Some(authority.namespace);
    service.access = Some(authority.access);
    service.access_expires_at_millis = Some(2_000_000_000_000);
    service.unacknowledged_session_id = Some(authority.session_id);
    service.credential_owner_acknowledged(Some(DirectoryRecord {
        owner: authority.owner,
        state: DirectoryState::Ready,
        version: authority.record_version,
    }));
}

fn assert_authority(actual: &EnrollmentAuthorization, expected: &EnrollmentAuthorization) {
    assert_eq!(actual.owner, expected.owner);
    assert_eq!(actual.namespace, expected.namespace);
    assert_eq!(actual.record_version, expected.record_version);
    assert_eq!(actual.session_id, expected.session_id);
    assert_eq!(actual.access, expected.access);
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Invocation {
    Select(BTreeSet<ProfileId>),
    Retry(Option<ProfileId>),
    Cancel,
}

struct AdmissionGate {
    entered: mpsc::Sender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
}

#[derive(Default)]
struct SetPort {
    targets: Mutex<BTreeMap<String, BTreeSet<ProfileId>>>,
    calls: Mutex<Vec<(EnrollmentAuthorization, Invocation)>>,
    reject_selection: AtomicBool,
    gate: Option<AdmissionGate>,
    local: Arc<LocalProfiles>,
}

impl SetPort {
    fn invoke(&self, authority: EnrollmentAuthorization, invocation: Invocation) {
        if let Some(gate) = &self.gate {
            gate.entered.send(()).unwrap();
            gate.resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        self.calls.lock().unwrap().push((authority, invocation));
    }
}

impl EnrollmentAccountControl for SetPort {
    fn activate_owned(
        &self,
        _: Option<crate::enrollment::EnrollmentAuthorization>,
        _: u64,
    ) -> bool {
        false
    }
    fn refresh_authorization(
        &self,
        _: crate::enrollment::EnrollmentAuthorization,
        _: crate::enrollment::EnrollmentAuthorization,
    ) -> bool {
        false
    }
    fn set_foreground(&self, _: bool) {}
    fn wake(&self) -> bool {
        false
    }
    fn snapshot(&self) -> crate::enrollment::Snapshot {
        crate::enrollment::Snapshot::default()
    }
    fn select_profiles(
        &self,
        profiles: BTreeSet<ProfileId>,
        authority: EnrollmentAuthorization,
    ) -> Result<Snapshot, &'static str> {
        for profile in &profiles {
            assert!(
                self.local.committed.lock().unwrap().contains(profile),
                "local Profile must be durable before enrollment admission"
            );
        }
        self.invoke(authority.clone(), Invocation::Select(profiles.clone()));
        if self.reject_selection.load(Ordering::Acquire) {
            return Err(crate::enrollment::PERSISTENCE_FAILED);
        }
        self.targets
            .lock()
            .unwrap()
            .insert(authority.owner.account.to_string(), profiles.clone());
        Ok(Snapshot {
            account_id: Some(authority.owner.account.to_string()),
            profiles: profiles
                .into_iter()
                .enumerate()
                .map(|(index, profile)| ProfileReport {
                    profile,
                    report: if index == 0 {
                        Report {
                            state: EnrollmentState::Waiting,
                            failure_code: Some(crate::enrollment::NO_PEER_ONLINE),
                            retry_at_millis: Some(450),
                            ..Report::default()
                        }
                    } else {
                        Report {
                            state: EnrollmentState::Complete,
                            ..Report::default()
                        }
                    },
                })
                .collect(),
            ..Snapshot::default()
        })
    }

    fn update_authorization(&self, _: EnrollmentAuthorization) -> bool {
        true
    }

    fn retry(&self, authority: EnrollmentAuthorization, profile: Option<ProfileId>) -> bool {
        self.invoke(authority, Invocation::Retry(profile));
        true
    }

    fn cancel_durable(&self) -> Result<(), &'static str> {
        panic!("account commands must not use unscoped cancellation")
    }

    fn cancel_durable_for(&self, authority: EnrollmentAuthorization) -> Result<(), &'static str> {
        self.invoke(authority.clone(), Invocation::Cancel);
        self.targets
            .lock()
            .unwrap()
            .remove(&authority.owner.account.to_string());
        Ok(())
    }
}

#[derive(Default)]
struct LocalProfiles {
    committed: Mutex<BTreeSet<ProfileId>>,
    inventory: Mutex<Vec<koru_store_wire::ProfileLifecycleInventoryItem>>,
    /// Whatever this device's copies of the Profile identity documents say.
    identities: Mutex<Vec<lifecycle::LocalProfileIdentity>>,
    refuse: AtomicBool,
    cancel_after_commit: Mutex<Option<Arc<AtomicBool>>>,
    /// Profiles brought back after being removed from this computer, in order.
    restored: Mutex<Vec<ProfileId>>,
}
impl lifecycle::LocalProfileInventory for LocalProfiles {
    fn restore_removed(&self, profile: ProfileId) -> Result<(), ServiceError> {
        // Restoring clears the removal mark, as the Store's does.
        for row in self.inventory.lock().unwrap().iter_mut() {
            if row.profile_id == profile {
                row.removed_at_millis = None;
            }
        }
        self.restored.lock().unwrap().push(profile);
        Ok(())
    }
    fn inventory(&self) -> Result<koru_store_wire::ProfileLifecycleInventory, ServiceError> {
        Ok(koru_store_wire::ProfileLifecycleInventory {
            profiles: self.inventory.lock().unwrap().clone(),
        })
    }
    fn identities(&self) -> Result<Vec<lifecycle::LocalProfileIdentity>, ServiceError> {
        Ok(self.identities.lock().unwrap().clone())
    }
    fn provision(
        &self,
        profile: ProfileId,
        initial: &koru_store_wire::ProvisionAccountProfile,
    ) -> Result<(), ServiceError> {
        if self.refuse.load(Ordering::Acquire) {
            return Err(ServiceError::retryable("profile_provisioning_unavailable"));
        }
        assert_eq!(initial.identity_id, authority_a().owner.account.to_string());
        assert_eq!(initial.locale.as_deref(), Some("en-US"));
        assert_eq!(
            initial.name,
            if profile.to_string() == FIRST {
                "Work"
            } else {
                "Personal"
            }
        );
        self.committed.lock().unwrap().insert(profile);
        if let Some(cancel) = self.cancel_after_commit.lock().unwrap().as_ref() {
            cancel.store(true, Ordering::Release);
        }
        Ok(())
    }
}

struct HostedProfiles {
    authority: EnrollmentAuthorization,
    session: Mutex<SessionSummaryV1>,
    profiles: Mutex<Vec<ProfileSummaryV1>>,
    refreshes: Mutex<Vec<RefreshRequestV1>>,
}
impl HostedProfiles {
    fn new(authority: EnrollmentAuthorization) -> Self {
        Self {
            session: Mutex::new(SessionSummaryV1 {
                version: Version1,
                session_id: authority.session_id.parse().unwrap(),
                koru_identity_id: authority.owner.account,
                auth_identity_id: authority.owner.auth_identity,
                device_id: Some(signed_descriptor().device_id),
                expires_at_millis: 2_000_000_000_000,
                recent_auth_at_millis: 1,
            }),
            profiles: Mutex::new(
                [FIRST, SECOND]
                    .into_iter()
                    .map(|id| ProfileSummaryV1 {
                        version: Version1,
                        koru_identity_id: authority.owner.account,
                        profile_id: ProfileId::parse(id).unwrap(),
                        name_snapshot: Some(if id == FIRST { "Work" } else { "Personal" }.into()),
                        locale_snapshot: Some("en-US".into()),
                        auth_identities: vec![AuthIdentitySummaryV1 {
                            version: Version1,
                            auth_identity_id: authority.owner.auth_identity,
                            provider: koru_account_core::AuthProvider::Google,
                            verified_email_snapshot: None,
                        }],
                        lifecycle_revision: koru_account_core::ProfileLifecycleRevision::INITIAL,
                    })
                    .collect(),
            ),
            authority,
            refreshes: Mutex::new(Vec::new()),
        }
    }
}
impl Transport for HostedProfiles {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let body = if request.path_and_query.ends_with("/refresh") {
            self.refreshes
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&request.body).unwrap());
            serde_json::to_vec(&TokenPairV1 {
                version: Version1,
                session_id: self.authority.session_id.parse().unwrap(),
                access_token: AccessToken("new-access".into()),
                access_expires_at_millis: 2_000_000_000_000,
                refresh_token: koru_account_wire::RefreshToken("new-refresh".into()),
                refresh_idle_expires_at_millis: 2_000_000_000_000,
                refresh_absolute_expires_at_millis: 2_000_000_000_000,
            })
            .unwrap()
        } else {
            assert!(request.headers.iter().any(|(key, value)| {
                key.eq_ignore_ascii_case("authorization")
                    && (value == &format!("Bearer {}", self.authority.access.0)
                        || value == "Bearer new-access")
            }));
            match request.path_and_query.as_str() {
                "/v1/oauth/session" => serde_json::to_vec(&*self.session.lock().unwrap()).unwrap(),
                "/v1/account/profiles" => serde_json::to_vec(&ProfileListResponseV1 {
                    version: Version1,
                    profiles: self.profiles.lock().unwrap().clone(),
                })
                .unwrap(),
                "/v1/account/devices" => {
                    serde_json::to_vec(&crate::account::tests::current_device_list()).unwrap()
                }
                "/v1/account" => serde_json::to_vec(&AccountSummaryV1 {
                    version: Version1,
                    koru_identity_id: self.authority.owner.account,
                    display_name: "PII not projected".into(),
                    auth_identities: Vec::new(),
                    terms_version_at_creation: "test".into(),
                    created_at_millis: 1,
                })
                .unwrap(),
                route => panic!("unexpected selection route: {route}"),
            }
        };
        Ok(HttpResponse { status: 200, body })
    }
}

fn service(port: Arc<SetPort>, authority: EnrollmentAuthorization) -> ProductionAccountService {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(HostedProfiles::new(authority.clone()))),
        signed_descriptor(),
        "test".into(),
    )
    .with_local_profile_inventory(port.local.clone())
    .with_enrollment_driver(port);
    acknowledge(&mut service, authority);
    service
}

fn execute(
    service: &mut ProductionAccountService,
    command: AccountCommand,
) -> Result<ServiceOutcome, ServiceError> {
    let restored = SessionCredential::new(
        service.durable_session_id.clone().unwrap(),
        "refresh-only".into(),
    )
    .unwrap();
    service.execute(
        73,
        command,
        Some(&restored),
        &Cancel(Arc::new(AtomicBool::new(false))),
        &mut |_| {},
    )
}

fn ids(values: &[&str]) -> BTreeSet<ProfileId> {
    values
        .iter()
        .map(|value| ProfileId::parse(value).unwrap())
        .collect()
}

#[test]
fn selection_returns_the_whole_committed_snapshot_and_empty_clears_only_its_account() {
    let port = Arc::new(SetPort::default());
    let b = authority_b();
    port.targets
        .lock()
        .unwrap()
        .insert(b.owner.account.to_string(), ids(&[SECOND]));
    let mut service = service(port.clone(), authority_a());
    let outcome = execute(
        &mut service,
        AccountCommand::SelectProfiles {
            profile_ids: vec![FIRST.into(), SECOND.into()],
        },
    )
    .unwrap();
    let AccountOperationResult::ProfilesSelected { profiles } = outcome.result else {
        panic!("expected committed rows")
    };
    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles[0].profile_id, FIRST);
    assert_eq!(profiles[0].state, EnrollmentState::Waiting);
    assert_eq!(
        profiles[0].failure_code.as_deref(),
        Some(crate::enrollment::NO_PEER_ONLINE)
    );
    assert_eq!(profiles[0].retry_at_millis, Some(450));
    assert_eq!(profiles[1].profile_id, SECOND);
    assert_eq!(profiles[1].state, EnrollmentState::Complete);
    assert_eq!(profiles[1].failure_code, None);
    assert_eq!(profiles[1].retry_at_millis, None);
    assert!(matches!(outcome.credential, CredentialChange::Keep));
    assert_eq!(outcome.state.stage, AccountStage::SignedIn);
    let empty = execute(
        &mut service,
        AccountCommand::SelectProfiles {
            profile_ids: vec![],
        },
    )
    .unwrap();
    assert!(
        matches!(empty.result, AccountOperationResult::ProfilesSelected { profiles } if profiles.is_empty())
    );
    let targets = port.targets.lock().unwrap();
    assert_eq!(
        targets[&authority_a().owner.account.to_string()],
        BTreeSet::new()
    );
    assert_eq!(targets[&b.owner.account.to_string()], ids(&[SECOND]));
    let calls = port.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_authority(&calls[0].0, &authority_a());
    assert_eq!(calls[0].1, Invocation::Select(ids(&[FIRST, SECOND])));
    assert_eq!(calls[1].1, Invocation::Select(BTreeSet::new()));
}

#[test]
fn failed_atomic_selection_does_not_publish_success_or_replace_existing_members() {
    let port = Arc::new(SetPort::default());
    port.targets
        .lock()
        .unwrap()
        .insert(authority_a().owner.account.to_string(), ids(&[FIRST]));
    port.reject_selection.store(true, Ordering::Release);
    let mut service = service(port.clone(), authority_a());
    let error = execute(
        &mut service,
        AccountCommand::SelectProfiles {
            profile_ids: vec![SECOND.into()],
        },
    )
    .err()
    .expect("port refused commit");
    assert_eq!(error.code, crate::enrollment::PERSISTENCE_FAILED);
    assert_eq!(
        port.targets.lock().unwrap()[&authority_a().owner.account.to_string()],
        ids(&[FIRST])
    );
    assert_eq!(port.calls.lock().unwrap().len(), 1);
}

#[test]
fn invalid_profile_sets_are_refused_before_the_enrollment_port() {
    for values in [
        vec![FIRST.into(), FIRST.into()],
        vec![SECOND.into(), FIRST.into()],
        vec![FIRST.to_uppercase()],
        vec![FIRST.replace('-', "")],
        vec!["10000000-0000-1000-8000-00000000000a".into()],
        vec!["not-a-profile".into()],
    ] {
        let port = Arc::new(SetPort::default());
        port.targets
            .lock()
            .unwrap()
            .insert(authority_a().owner.account.to_string(), ids(&[SECOND]));
        let mut service = service(port.clone(), authority_a());
        assert!(
            execute(
                &mut service,
                AccountCommand::SelectProfiles {
                    profile_ids: values
                }
            )
            .is_err()
        );
        assert!(port.calls.lock().unwrap().is_empty());
        assert_eq!(
            port.targets.lock().unwrap()[&authority_a().owner.account.to_string()],
            ids(&[SECOND])
        );
    }
}

#[test]
fn invalid_member_retry_is_refused_before_the_enrollment_port() {
    for profile_id in [
        FIRST.to_uppercase(),
        FIRST.replace('-', ""),
        "10000000-0000-1000-8000-00000000000a".into(),
        "not-a-profile".into(),
    ] {
        let port = Arc::new(SetPort::default());
        port.targets.lock().unwrap().insert(
            authority_a().owner.account.to_string(),
            ids(&[FIRST, SECOND]),
        );
        let mut service = service(port.clone(), authority_a());
        assert!(
            execute(
                &mut service,
                AccountCommand::RetryEnrollment {
                    profile_id: Some(profile_id)
                }
            )
            .is_err()
        );
        assert!(port.calls.lock().unwrap().is_empty());
        assert_eq!(
            port.targets.lock().unwrap()[&authority_a().owner.account.to_string()],
            ids(&[FIRST, SECOND])
        );
    }
}

#[test]
fn held_retry_and_cancel_keep_exact_a_authority_after_b_becomes_visible() {
    for invocation in [
        Invocation::Retry(None),
        Invocation::Retry(Some(ProfileId::parse(FIRST).unwrap())),
        Invocation::Cancel,
    ] {
        let (entered, waiting) = mpsc::channel();
        let (resume, continuation) = mpsc::channel();
        let port = Arc::new(SetPort {
            gate: Some(AdmissionGate {
                entered,
                resume: Mutex::new(continuation),
            }),
            ..SetPort::default()
        });
        let a = authority_a();
        let b = authority_b();
        port.targets.lock().unwrap().extend([
            (a.owner.account.to_string(), ids(&[FIRST])),
            (b.owner.account.to_string(), ids(&[SECOND])),
        ]);
        let mut service_a = service(port.clone(), a.clone());
        let service_b = service(port.clone(), b.clone());
        let visible_a = Arc::new(AtomicBool::new(true));
        service_a.visible = Some(visible_a.clone());
        let command = match invocation {
            Invocation::Retry(profile) => AccountCommand::RetryEnrollment {
                profile_id: profile.map(|id| id.to_string()),
            },
            Invocation::Cancel => AccountCommand::CancelEnrollment,
            _ => unreachable!(),
        };
        let work = std::thread::spawn(move || execute(&mut service_a, command));
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        // The A command has reached the shared port but has not performed its
        // effect. Selecting B must not replace that captured authority.
        visible_a.store(false, Ordering::Release);
        assert_authority(&service_b.enrollment_authorization().unwrap(), &b);
        resume.send(()).unwrap();
        let outcome = work.join().unwrap().unwrap();
        assert!(matches!(outcome.credential, CredentialChange::Keep));
        let calls = port.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_authority(&calls[0].0, &a);
        assert_eq!(calls[0].1, invocation);
        let targets = port.targets.lock().unwrap();
        assert_eq!(targets[&b.owner.account.to_string()], ids(&[SECOND]));
        if invocation == Invocation::Cancel {
            assert!(!targets.contains_key(&a.owner.account.to_string()));
        } else {
            assert_eq!(targets[&a.owner.account.to_string()], ids(&[FIRST]));
        }
    }
}

#[test]
fn issued_unacknowledged_access_never_borrows_the_previous_enrollment_owner() {
    for command in [
        AccountCommand::SelectProfiles {
            profile_ids: vec![FIRST.into()],
        },
        AccountCommand::RetryEnrollment { profile_id: None },
        AccountCommand::CancelEnrollment,
    ] {
        let port = Arc::new(SetPort::default());
        let mut service = service(port.clone(), authority_a());
        service.access = Some(authority_b().access);
        service.unacknowledged_session_id = Some(authority_b().session_id);
        let error = execute(&mut service, command)
            .err()
            .expect("new access is not acknowledged");
        assert_eq!(error.code, "credential_owner_unavailable");
        assert!(port.calls.lock().unwrap().is_empty());
    }
}

#[test]
fn selection_validates_every_member_and_exact_session_before_any_local_commit() {
    for fault in 0..7 {
        let port = Arc::new(SetPort::default());
        let hosted = Arc::new(HostedProfiles::new(authority_a()));
        match fault {
            0 => hosted.session.lock().unwrap().koru_identity_id = authority_b().owner.account,
            1 => {
                hosted.session.lock().unwrap().auth_identity_id = authority_b().owner.auth_identity
            }
            2 => {
                hosted.session.lock().unwrap().session_id =
                    authority_b().session_id.parse().unwrap()
            }
            3 => hosted.session.lock().unwrap().device_id = None,
            4 => {
                hosted.profiles.lock().unwrap().pop();
            }
            5 => hosted.profiles.lock().unwrap()[1].auth_identities.clear(),
            6 => hosted.profiles.lock().unwrap()[1].koru_identity_id = authority_b().owner.account,
            _ => unreachable!(),
        }
        let mut service = service(port.clone(), authority_a());
        service.client = AccountClient::with_transport(hosted);
        assert!(
            execute(
                &mut service,
                AccountCommand::SelectProfiles {
                    profile_ids: vec![FIRST.into(), SECOND.into()]
                }
            )
            .is_err()
        );
        assert!(port.local.committed.lock().unwrap().is_empty());
        assert!(port.calls.lock().unwrap().is_empty());
    }
}

/// **A Profile removed from this computer is brought back when it is chosen
/// again.** It is still this account's, and the person just picked it under
/// *Import Profiles*. It used to refuse the whole selection as
/// `profile_removed`, which no surface explained: *Connect selected* simply
/// did nothing, and the refusal failed the account into looking signed out.
#[test]
fn a_member_removed_from_this_computer_is_restored_then_connected() {
    let port = Arc::new(SetPort::default());
    port.local
        .inventory
        .lock()
        .unwrap()
        .push(koru_store_wire::ProfileLifecycleInventoryItem {
            profile_id: ProfileId::parse(SECOND).unwrap(),
            account_binding: format!("koru:{}", authority_a().owner.account),
            removed_at_millis: Some(1),
        });
    let mut service = service(port.clone(), authority_a());
    assert!(
        execute(
            &mut service,
            AccountCommand::SelectProfiles {
                profile_ids: vec![FIRST.into(), SECOND.into()]
            }
        )
        .is_ok()
    );
    assert_eq!(
        *port.local.restored.lock().unwrap(),
        [ProfileId::parse(SECOND).unwrap()],
        "only the removed member is restored"
    );
    assert_eq!(
        *port.local.committed.lock().unwrap(),
        ids(&[FIRST, SECOND]),
        "and every member is provisioned"
    );
}

/// Bringing a Profile back is a local commit like any other: nothing is
/// restored until the whole set has validated.
#[test]
fn nothing_is_restored_when_another_member_refuses_the_selection() {
    let port = Arc::new(SetPort::default());
    let mut inventory = port.local.inventory.lock().unwrap();
    inventory.push(koru_store_wire::ProfileLifecycleInventoryItem {
        profile_id: ProfileId::parse(FIRST).unwrap(),
        account_binding: format!("koru:{}", authority_a().owner.account),
        removed_at_millis: Some(1),
    });
    inventory.push(koru_store_wire::ProfileLifecycleInventoryItem {
        profile_id: ProfileId::parse(SECOND).unwrap(),
        account_binding: format!("koru:{}", authority_b().owner.account),
        removed_at_millis: None,
    });
    drop(inventory);
    let mut service = service(port.clone(), authority_a());
    assert!(
        execute(
            &mut service,
            AccountCommand::SelectProfiles {
                profile_ids: vec![FIRST.into(), SECOND.into()]
            }
        )
        .is_err()
    );
    assert!(port.local.restored.lock().unwrap().is_empty());
    assert!(port.local.committed.lock().unwrap().is_empty());
    assert!(port.calls.lock().unwrap().is_empty());
}

#[test]
fn a_differently_bound_local_member_refuses_the_entire_selection() {
    for (binding, removed) in [
        (format!("koru:{}", authority_b().owner.account), None),
        (format!("koru:{}", authority_b().owner.account), Some(1)),
        (String::new(), None),
    ] {
        let port = Arc::new(SetPort::default());
        port.local
            .inventory
            .lock()
            .unwrap()
            .push(koru_store_wire::ProfileLifecycleInventoryItem {
                profile_id: ProfileId::parse(SECOND).unwrap(),
                account_binding: binding,
                removed_at_millis: removed,
            });
        let mut service = service(port.clone(), authority_a());
        assert!(
            execute(
                &mut service,
                AccountCommand::SelectProfiles {
                    profile_ids: vec![FIRST.into(), SECOND.into()]
                }
            )
            .is_err()
        );
        assert!(port.local.committed.lock().unwrap().is_empty());
        assert!(port.calls.lock().unwrap().is_empty());
    }
}

#[test]
fn unavailable_local_commit_never_admits_enrollment_or_reports_selected() {
    let port = Arc::new(SetPort::default());
    port.local.refuse.store(true, Ordering::Release);
    let mut service = service(port.clone(), authority_a());
    let error = execute(
        &mut service,
        AccountCommand::SelectProfiles {
            profile_ids: vec![FIRST.into()],
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "profile_provisioning_unavailable");
    assert!(error.retryable);
    assert!(port.calls.lock().unwrap().is_empty());
}

#[test]
fn cancellation_after_first_local_commit_stops_remaining_members_and_enrollment() {
    let port = Arc::new(SetPort::default());
    let cancelled = Arc::new(AtomicBool::new(false));
    *port.local.cancel_after_commit.lock().unwrap() = Some(cancelled.clone());
    let mut service = service(port.clone(), authority_a());
    let error = service
        .execute(
            1,
            AccountCommand::SelectProfiles {
                profile_ids: vec![FIRST.into(), SECOND.into()],
            },
            None,
            &Cancel(cancelled),
            &mut |_| {},
        )
        .err()
        .unwrap();
    assert_eq!(error.code, "cancelled");
    assert_eq!(*port.local.committed.lock().unwrap(), ids(&[FIRST]));
    assert!(port.calls.lock().unwrap().is_empty());
}

#[test]
fn expired_selection_and_retry_refresh_exactly_and_wait_for_credential_acknowledgement() {
    for command in [
        AccountCommand::SelectProfiles {
            profile_ids: vec![FIRST.into()],
        },
        AccountCommand::RetryEnrollment {
            profile_id: Some(FIRST.into()),
        },
    ] {
        let port = Arc::new(SetPort::default());
        let hosted = Arc::new(HostedProfiles::new(authority_a()));
        let mut service = service(port.clone(), authority_a());
        service.client = AccountClient::with_transport(hosted.clone());
        service.access_expires_at_millis = Some(1);
        // Nothing is sent until the key this rotation would be replayed under
        // is durable, because a rotation that cannot be replayed is a session
        // that cannot be recovered.
        let unarmed = execute(&mut service, command.clone()).err().unwrap();
        assert_eq!(unarmed.code, "credential_unavailable");
        assert!(unarmed.retryable);
        assert!(hosted.refreshes.lock().unwrap().is_empty());
        let armed = [7u8; 32];
        service.arm_refresh_retry_key(Some(armed));
        let outcome = execute(&mut service, command.clone()).unwrap();
        assert_eq!(outcome.follow_up, Some(command.clone()));
        let CredentialChange::Replace(credential) = outcome.credential else {
            panic!("refresh must be durably acknowledged")
        };
        assert_eq!(credential.refresh_token, "new-refresh");
        assert!(port.calls.lock().unwrap().is_empty());
        assert!(port.local.committed.lock().unwrap().is_empty());
        assert!(service.enrollment_authorization().is_none());
        assert!(service.access_refresh_at_millis().is_none());
        let error = execute(&mut service, command.clone()).err().unwrap();
        assert_eq!(error.code, "credential_owner_unavailable");
        let authority = authority_a();
        service.credential_owner_acknowledged(Some(DirectoryRecord {
            owner: authority.owner,
            state: DirectoryState::Ready,
            version: authority.record_version + 1,
        }));
        assert_eq!(
            service.access_refresh_at_millis(),
            Some(2_000_000_000_000 - 60_000)
        );
        let outcome = service
            .execute(
                73,
                command,
                Some(&credential),
                &Cancel(Arc::new(AtomicBool::new(false))),
                &mut |_| {},
            )
            .unwrap();
        assert!(matches!(outcome.credential, CredentialChange::Keep));
        let calls = port.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.access, AccessToken("new-access".into()));
        assert_eq!(calls[0].0.record_version, authority.record_version + 1);
        let refreshes = hosted.refreshes.lock().unwrap();
        assert_eq!(refreshes.len(), 1);
        assert_eq!(refreshes[0].refresh_token.0, "refresh-only");
        assert_eq!(
            refreshes[0].idempotency_key,
            RefreshIdempotencyKey::from_bytes(armed),
            "the refresh presents exactly the armed durable key"
        );
    }
}

/// The defect this closes: **a Profile rename never reaches the account's copy
/// of its name.** Nothing updates `discovery_name_snapshot` after the Profile is
/// created, and there is deliberately no route that would, so a surface reading
/// the account's copy showed the creation-time name for the life of the Profile
/// — the maintainers renamed one and its account row still carried the name from six
/// days earlier.
///
/// Where this device holds the Profile's own identity document, that wins. The
/// account's copy is what a brand new device shows before anything has synced,
/// and nothing more.
#[test]
fn an_account_row_shows_the_identity_document_name_and_falls_back_only_without_one() {
    let port = Arc::new(SetPort::default());
    // FIRST was renamed after it was created, so document and account disagree.
    // SECOND holds no document here — the first-run case the account copy serves.
    *port.local.identities.lock().unwrap() = vec![lifecycle::LocalProfileIdentity {
        profile_id: ProfileId::parse(FIRST).unwrap(),
        name: "Renamed at the desk".into(),
        locale: Some("fr-CH".into()),
    }];
    let mut service = service(port.clone(), authority_a());

    let AccountOperationResult::Profiles { profiles } =
        execute(&mut service, AccountCommand::ListProfiles)
            .unwrap()
            .result
    else {
        panic!("expected the account Profile rows")
    };
    assert_eq!(profiles[0].profile_id, FIRST);
    assert_eq!(profiles[0].name.as_deref(), Some("Renamed at the desk"));
    assert_eq!(profiles[0].locale.as_deref(), Some("fr-CH"));
    assert_eq!(profiles[1].profile_id, SECOND);
    assert_eq!(profiles[1].name.as_deref(), Some("Personal"));
    assert_eq!(profiles[1].locale.as_deref(), Some("en-US"));

    // The onboarding chooser reads the same rows through the choices route, and
    // a Profile already on this device is exactly the one it used to mis-name.
    let AccountOperationResult::ProfileChoices { choices, .. } =
        execute(&mut service, AccountCommand::ListProfileChoices)
            .unwrap()
            .result
    else {
        panic!("expected the account Profile choices")
    };
    assert_eq!(
        choices[0].profile.name.as_deref(),
        Some("Renamed at the desk")
    );
    assert_eq!(choices[1].profile.name.as_deref(), Some("Personal"));

    // A Store this thread cannot reach is not a reason to show nothing: the
    // account's copy is still better than a list of nameless Profiles.
    port.local.identities.lock().unwrap().clear();
    let AccountOperationResult::Profiles { profiles } =
        execute(&mut service, AccountCommand::ListProfiles)
            .unwrap()
            .result
    else {
        panic!("expected the account Profile rows")
    };
    assert_eq!(profiles[0].name.as_deref(), Some("Work"));
}
