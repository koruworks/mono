// SPDX-License-Identifier: MPL-2.0

//! A whole account snapshot and one validity deadline; no policy evaluation,
//! local byte estimates, network I/O or recurring polling.
use super::*;
use koru_account_wire::cloud_plan::CloudPlanResponseV1;
use koru_agent_wire::CloudPlanPayload;
use std::time::{Duration, Instant};

pub(crate) struct Snapshot {
    pub plan: CloudPlanResponseV1,
    deadline: Option<Instant>,
    pub stale: bool,
}
impl Snapshot {
    fn decode(payload: &CloudPlanPayload, elapsed: Duration, now: Instant) -> Option<Self> {
        let plan: CloudPlanResponseV1 = serde_json::from_str(payload.as_str()).ok()?;
        let deadline = match plan.valid_until_millis {
            None => None,
            Some(until) => {
                let lifetime = until.get().checked_sub(plan.evaluated_at_millis.get())?;
                let remaining =
                    Duration::from_millis(u64::try_from(lifetime).ok()?).saturating_sub(elapsed);
                Some(now.checked_add(remaining)?)
            }
        };
        let stale = deadline.is_some_and(|deadline| now >= deadline);
        Some(Self {
            plan,
            deadline,
            stale,
        })
    }
}
struct Request {
    pending: Pending,
    started: Instant,
}
pub(crate) struct Storage {
    pub snapshot: Option<Snapshot>,
    pub failed: bool,
    due: bool,
    expiry_refresh_used: bool,
    pending: Option<Request>,
    discard_pending: bool,
}
impl Default for Storage {
    fn default() -> Self {
        Self {
            snapshot: None,
            failed: false,
            due: true,
            expiry_refresh_used: false,
            pending: None,
            discard_pending: false,
        }
    }
}
impl Storage {
    pub(super) fn usage_changed(&mut self) {
        self.snapshot = None;
        self.failed = false;
        self.due = true;
        self.expiry_refresh_used = false;
        self.discard_pending = self.pending.is_some();
    }
    pub fn refresh(&mut self) {
        if self.pending.is_none() {
            self.due = true;
            self.expiry_refresh_used = false;
        }
    }
    fn refresh_expired(&mut self) {
        if !self.expiry_refresh_used {
            self.expiry_refresh_used = true;
            self.due = true;
        }
    }
    pub fn checking(&self) -> bool {
        self.pending.is_some() || self.due
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.snapshot
            .as_ref()
            .filter(|snapshot| !snapshot.stale)
            .and_then(|snapshot| snapshot.deadline)
    }
    pub fn maintain(&mut self, now: Instant) -> bool {
        if !self.deadline().is_some_and(|deadline| now >= deadline) {
            return false;
        }
        if let Some(snapshot) = self.snapshot.as_mut() {
            snapshot.stale = true;
        }
        self.refresh_expired();
        true
    }
    pub fn take(&mut self, now: Instant) -> Option<AccountCommand> {
        self.maintain(now);
        if !self.due || self.pending.is_some() {
            return None;
        }
        let id = next_request_id();
        self.due = false;
        self.pending = Some(Request {
            pending: Pending {
                id,
                submission: CommandSubmissionId::next(),
                generation: None,
            },
            started: now,
        });
        Some(AccountCommand::InspectCloudPlan { request_id: id })
    }
    pub fn submission(&self, id: u64) -> Option<CommandSubmissionId> {
        self.pending
            .as_ref()
            .filter(|request| request.pending.id == id)
            .map(|request| request.pending.submission)
    }
    pub fn admit(&mut self, admission: CommandAdmission) -> bool {
        let Some(request) = self
            .pending
            .as_mut()
            .filter(|request| request.pending.submission == admission.submission)
        else {
            return false;
        };
        request.pending.generation = Some(admission.generation);
        true
    }
    pub fn fail(&mut self, failure: CommandFailure) -> bool {
        if !self
            .pending
            .as_ref()
            .is_some_and(|request| request.pending.submission == failure.submission)
        {
            return false;
        }
        self.pending = None;
        self.discard_pending = false;
        self.failed = true;
        if let Some(snapshot) = self.snapshot.as_mut() {
            snapshot.stale = true;
        }
        true
    }
    pub fn result(
        &mut self,
        generation: u64,
        id: u64,
        payload: Option<&CloudPlanPayload>,
        elapsed_millis: u64,
        now: Instant,
    ) -> bool {
        let Some(request) = self.pending.as_ref() else {
            return false;
        };
        if request.pending.id != id
            || !request
                .pending
                .generation
                .is_none_or(|expected| expected == generation)
        {
            return false;
        }
        // Includes UI queueing, HTTP and result delivery. The worker duration
        // prevents a platform adapter from accidentally shortening that budget.
        if self.discard_pending {
            self.pending = None;
            self.discard_pending = false;
            return true;
        }
        let elapsed = now
            .saturating_duration_since(request.started)
            .max(Duration::from_millis(elapsed_millis));
        let snapshot = payload.and_then(|payload| Snapshot::decode(payload, elapsed, now));
        self.pending = None;
        self.failed = snapshot.is_none();
        match snapshot {
            Some(snapshot) => {
                // One automatic refresh for a response that expired in transit;
                // repeated expired replies stay stale until explicit retry.
                if snapshot.stale {
                    self.refresh_expired();
                } else {
                    self.expiry_refresh_used = false;
                }
                self.snapshot = Some(snapshot);
            }
            None => {
                if let Some(snapshot) = self.snapshot.as_mut() {
                    snapshot.stale = true;
                }
            }
        }
        true
    }
}

impl ProfilesController {
    pub(crate) fn cloud_storage(&self) -> &Storage {
        &self.cloud.storage
    }
    pub fn refresh_cloud_storage(&mut self) {
        if self.cloud.storage.checking() {
            return;
        }
        self.cloud.storage.refresh();
    }
    pub(in crate::manage::profiles) fn maintain_cloud_storage(&mut self, now: Instant) -> bool {
        self.cloud_visible && self.cloud.storage.maintain(now)
    }
    pub(in crate::manage::profiles) fn cloud_storage_deadline(&self) -> Option<Instant> {
        self.cloud_visible
            .then(|| self.cloud.storage.deadline())
            .flatten()
    }
    pub(crate) fn cloud_storage_words(&self, lang: Lang) -> String {
        use koru_account_wire::cloud_plan::{AllowanceV1, UsageReason, UsageState};
        let storage = &self.cloud.storage;
        let t = |key| koru_i18n::t(lang, key);
        let size = |value: koru_account_wire::cloud_plan::Decimal| {
            koru_i18n::number::decimal_bytes(lang, value.get() as u64)
        };
        let mut lines = Vec::new();
        if let Some(snapshot) = &storage.snapshot {
            let plan = &snapshot.plan;
            if snapshot.stale
                || snapshot
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
            {
                lines.push(koru_i18n::fill(
                    t(TextKey::CloudStorageStale),
                    &[&crate::format::absolute_local(
                        lang,
                        plan.evaluated_at_millis.get(),
                    )],
                ));
            }
            match plan.policy.allowance {
                AllowanceV1::Finite {
                    effective_bytes, ..
                } => {
                    lines.push(match plan.usage.available_bytes {
                        Some(available) => koru_i18n::fill(
                            t(TextKey::CloudStorageAvailable),
                            &[&size(available), &size(effective_bytes)],
                        ),
                        None => koru_i18n::fill(
                            t(TextKey::CloudStorageTotal),
                            &[&size(effective_bytes)],
                        ),
                    });
                }
                AllowanceV1::Unbounded => lines.push(t(TextKey::CloudStorageNoLimit).into()),
                AllowanceV1::Unavailable { .. } => {
                    lines.push(t(TextKey::CloudStorageAllowanceUnknown).into())
                }
            }
            if let Some(used) = plan.usage.used_bytes {
                lines.push(koru_i18n::fill(
                    t(TextKey::CloudStorageUsed),
                    &[&size(used)],
                ));
            }
            match plan.usage.state {
                UsageState::Building => lines.push(t(TextKey::CloudStorageCalculating).into()),
                UsageState::Unknown => lines.push(
                    t(
                        if plan.usage.reason == Some(UsageReason::CloudSchemaUpdateRequired) {
                            TextKey::CloudStorageSchema
                        } else {
                            TextKey::CloudStorageUnknown
                        },
                    )
                    .into(),
                ),
                UsageState::Known => {}
            }
        } else {
            lines.push(
                t(if storage.checking() {
                    TextKey::CloudStorageChecking
                } else {
                    TextKey::CloudStorageFailed
                })
                .into(),
            );
        }
        lines.push(t(TextKey::CloudStorageBasis).into());
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> serde_json::Value {
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/koru-account-wire/src/cloud_plan.fixtures.json"
        )))
        .unwrap();
        fixtures["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .find(|fixture| fixture["name"] == name)
            .unwrap()["response"]
            .clone()
    }
    fn payload() -> CloudPlanPayload {
        let mut value = fixture("finite_plus");
        value["evaluated_at_millis"] = "1000".into();
        value["valid_until_millis"] = "11000".into();
        CloudPlanPayload::new(serde_json::to_string(&value).unwrap()).unwrap()
    }
    #[test]
    fn complete_server_states_keep_exact_bytes_and_explicit_unknowns_in_every_locale() {
        use koru_account_wire::cloud_plan::{AllowanceV1, UsageReason, UsageState};
        for name in [
            "finite_plus",
            "unbounded",
            "building_before_reservation_reconciliation",
            "unknown_usage",
            "policy_unavailable_usage_known",
            "known_local_no_cloud",
            "known_over_limit",
        ] {
            let mut value = fixture(name);
            value["valid_until_millis"] = serde_json::Value::Null;
            let payload = CloudPlanPayload::new(value.to_string()).unwrap();
            let mut controller = ProfilesController::new();
            controller.cloud.storage.snapshot =
                Some(Snapshot::decode(&payload, Duration::ZERO, Instant::now()).unwrap());
            for lang in [
                Lang::En,
                Lang::De,
                Lang::Fr,
                Lang::Es,
                Lang::PtBr,
                Lang::ZhHans,
                Lang::Ja,
                Lang::Ar,
            ] {
                let words = controller.cloud_storage_words(lang);
                let t = |key| koru_i18n::t(lang, key);
                let size = |bytes: koru_account_wire::cloud_plan::Decimal| {
                    koru_i18n::number::decimal_bytes(lang, bytes.get() as u64)
                };
                let plan = &controller.cloud_storage().snapshot.as_ref().unwrap().plan;
                if let Some(used) = plan.usage.used_bytes {
                    assert!(
                        words.contains(&koru_i18n::fill(
                            t(TextKey::CloudStorageUsed),
                            &[&size(used)]
                        )),
                        "{name}"
                    );
                }
                match plan.policy.allowance {
                    AllowanceV1::Unbounded => {
                        assert!(words.contains(t(TextKey::CloudStorageNoLimit)))
                    }
                    AllowanceV1::Unavailable { .. } => {
                        assert!(words.contains(t(TextKey::CloudStorageAllowanceUnknown)))
                    }
                    AllowanceV1::Finite {
                        effective_bytes, ..
                    } => {
                        assert!(words.contains(&size(effective_bytes)));
                        if let Some(available) = plan.usage.available_bytes {
                            assert!(words.contains(&koru_i18n::fill(
                                t(TextKey::CloudStorageAvailable),
                                &[&size(available), &size(effective_bytes)]
                            )));
                        }
                    }
                }
                match plan.usage.state {
                    UsageState::Building => {
                        assert!(words.contains(t(TextKey::CloudStorageCalculating)))
                    }
                    UsageState::Unknown => assert!(words.contains(t(
                        if plan.usage.reason == Some(UsageReason::CloudSchemaUpdateRequired) {
                            TextKey::CloudStorageSchema
                        } else {
                            TextKey::CloudStorageUnknown
                        }
                    ))),
                    UsageState::Known => {}
                }
                assert!(words.contains(t(TextKey::CloudStorageBasis)));
            }
        }
        // Values above floating-point integer precision stay accurate through integer scaling.
        let mut value = fixture("unbounded");
        value["usage"]["used_bytes"] = "9007199254740993".into();
        value["usage"]["files_bytes"] = "9007199254740993".into();
        let payload = CloudPlanPayload::new(value.to_string()).unwrap();
        let mut controller = ProfilesController::new();
        controller.cloud.storage.snapshot =
            Some(Snapshot::decode(&payload, Duration::ZERO, Instant::now()).unwrap());
        assert!(
            controller
                .cloud_storage_words(Lang::En)
                .contains("9,007,199 GB")
        );
    }

    #[test]
    fn differing_revision_expired_replies_refresh_once_and_wrong_generation_is_refused() {
        let now = Instant::now();
        let mut storage = Storage::default();
        for attempt in 0..2 {
            let AccountCommand::InspectCloudPlan { request_id } = storage.take(now).unwrap() else {
                panic!()
            };
            let submission = storage.submission(request_id).unwrap();
            assert!(storage.admit(CommandAdmission {
                submission,
                owner_scope: 1,
                generation: 5
            }));
            assert!(!storage.result(4, request_id, Some(&payload()), 0, now));
            let mut changed: serde_json::Value = serde_json::from_str(payload().as_str()).unwrap();
            changed["revision"] = (42 + attempt).to_string().into();
            let changed = CloudPlanPayload::new(changed.to_string()).unwrap();
            assert!(storage.result(5, request_id, Some(&changed), 10001, now));
            assert!(storage.snapshot.as_ref().unwrap().stale);
            assert_eq!(storage.checking(), attempt == 0);
        }
        assert!(storage.take(now + Duration::from_secs(100)).is_none());
    }
    #[test]
    fn slow_delivery_and_trial_boundary_invalidate_whole_snapshot_without_polling() {
        let now = Instant::now();
        let mut storage = Storage::default();
        let AccountCommand::InspectCloudPlan { request_id } = storage.take(now).unwrap() else {
            panic!()
        };
        assert!(storage.result(
            1,
            request_id,
            Some(&payload()),
            2000,
            now + Duration::from_secs(3)
        ));
        assert_eq!(storage.deadline(), Some(now + Duration::from_secs(10)));
        assert!(!storage.maintain(now + Duration::from_secs(9)));
        assert!(storage.maintain(now + Duration::from_secs(10)));
        assert!(storage.snapshot.as_ref().unwrap().stale);
        assert_eq!(storage.deadline(), None);
        let AccountCommand::InspectCloudPlan { request_id } =
            storage.take(now + Duration::from_secs(10)).unwrap()
        else {
            panic!()
        };
        assert!(storage.result(2, request_id, None, 0, now + Duration::from_secs(11)));
        assert!(storage.snapshot.as_ref().unwrap().stale);
        assert!(storage.failed);
        assert!(storage.take(now + Duration::from_secs(100)).is_none());
    }
    #[test]
    fn reset_rejects_old_account_result_and_malformed_never_becomes_zero() {
        let now = Instant::now();
        let mut storage = Storage::default();
        let AccountCommand::InspectCloudPlan { request_id } = storage.take(now).unwrap() else {
            panic!()
        };
        storage = Storage::default();
        assert!(!storage.result(1, request_id, Some(&payload()), 0, now));
        let AccountCommand::InspectCloudPlan { request_id } = storage.take(now).unwrap() else {
            panic!()
        };
        assert!(storage.result(
            2,
            request_id,
            Some(&CloudPlanPayload::new("{}".into()).unwrap()),
            0,
            now
        ));
        assert!(storage.snapshot.is_none());
        assert!(storage.failed);
    }

    #[test]
    fn actual_controller_clears_on_account_switch_and_rejects_late_owned_results() {
        let mut controller = ProfilesController::new();
        let account = AccountState {
            active_scope: 1,
            active_account_id: Some("first".into()),
            stage: AccountStage::SignedIn,
            ..Default::default()
        };
        controller.set_account(account.clone());
        controller.set_cloud_visible(true);
        let AccountCommand::InspectCloudPlan { request_id } =
            controller.cloud.storage.take(Instant::now()).unwrap()
        else {
            panic!()
        };
        assert!(controller.complete_scoped(
            1,
            1,
            AccountOperationResult::CloudPlan {
                request_id,
                payload: Some(payload()),
                request_elapsed_millis: 0
            }
        ));
        assert!(controller.cloud_storage().snapshot.is_some());
        controller.refresh_cloud_storage();
        let AccountCommand::InspectCloudPlan { request_id } =
            controller.cloud.storage.take(Instant::now()).unwrap()
        else {
            panic!()
        };
        controller.set_account(AccountState {
            active_scope: 2,
            active_account_id: Some("second".into()),
            stage: AccountStage::SignedIn,
            ..Default::default()
        });
        assert!(controller.cloud_storage().snapshot.is_none());
        assert!(!controller.complete_scoped(
            1,
            2,
            AccountOperationResult::CloudPlan {
                request_id,
                payload: Some(payload()),
                request_elapsed_millis: 0
            }
        ));
        assert!(controller.cloud_storage().snapshot.is_none());
        controller.set_account(account);
        assert!(controller.cloud_storage().snapshot.is_none());
        assert_eq!(controller.cloud_storage_deadline(), None);
    }
}
