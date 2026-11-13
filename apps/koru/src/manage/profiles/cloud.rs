// SPDX-License-Identifier: MPL-2.0

//! Account-scoped Koru Cloud state: the subscription and each Profile's sync
//! switch. No layout, no credentials, no Stripe IDs and no I/O.
//!
//! **The switch is the whole act** (maintainer, 2026-11-11). Turning it on
//! used to raise D258's question about the Profile's Workspaces, and the
//! answer was what opened this device's Store to Cloud. A switch left on
//! with the question dismissed therefore synced nothing. The question is
//! gone: the Agent opens the Store for every Profile whose Cloud sync is on
//! (`koru-account-runtime`'s `cloud_sync`, ADR-000-createdwithaccount).
//!
//! Two surfaces live here because they are one fact seen twice: the card says
//! whether this account has Cloud at all, and the switch on each Profile row
//! says whether this device syncs that Profile. The card's "syncing n of m
//! profiles" is counted from the very switches below it, which is why the
//! subscription read and the per-Profile read share a state struct rather than
//! sitting in two modules that would have to agree.
use super::*;
use crate::mainview::cloud_sync::Connection;
use koru_agent_wire::CloudSyncProfile;
use koru_i18n::{Key as TextKey, Lang};
use koru_workspace_host::account_ui::{CommandAdmission, CommandFailure, CommandSubmissionId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Unknown,
    Loading,
    Subscribe,
    Manage,
    Active,
    Unavailable,
}
pub(super) struct Pending {
    id: u64,
    submission: CommandSubmissionId,
    generation: Option<u64>,
}
/// An `InspectCloudSync` or a `SetProfileCloudSync`. **Both are answered by
/// `CloudSync` carrying the same `request_id`**, so one slot correlates both and
/// only one is ever in flight; `set` is what tells the result which of the two
/// it was finishing.
pub(super) struct SyncPending {
    id: u64,
    submission: CommandSubmissionId,
    generation: Option<u64>,
    set: Option<(String, bool)>,
}
pub(super) struct State {
    pub(super) status: Status,
    access: Option<bool>,
    due: bool,
    pending: Option<Pending>,
    /// Per-Profile Cloud sync as the Agent last reported it. `None` until the
    /// first answer arrives: **unknown is not the same as "nothing is on"**, and
    /// a card that counted `None` as zero would say "syncing 0 of 3" about a
    /// device that is syncing all three.
    pub(super) sync: Option<Vec<CloudSyncProfile>>,
    sync_due: bool,
    pub(super) sync_pending: Option<SyncPending>,
    /// A switch that has been pressed and not yet drained onto the wire. Held
    /// separately so the row can show the pending state on the very next frame
    /// rather than after the command lane gets a turn.
    pub(super) set_request: Option<(String, bool)>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            status: Status::Unknown,
            access: None,
            due: true,
            pending: None,
            sync: None,
            sync_due: true,
            sync_pending: None,
            set_request: None,
        }
    }
}
impl State {
    pub fn invalidate(&mut self) {
        *self = Self::default();
    }
}

impl ProfilesController {
    /// What the account's subscription is, as far as this form knows.
    pub fn cloud_status(&self) -> Status {
        self.cloud.status
    }
    pub fn cloud_access(&self) -> Option<bool> {
        match self.cloud.status {
            Status::Unknown | Status::Loading | Status::Unavailable => None,
            Status::Active => Some(true),
            Status::Manage => Some(self.cloud.access.unwrap_or(true)),
            Status::Subscribe => Some(false),
        }
    }
    pub fn set_cloud_visible(&mut self, visible: bool) {
        if visible && !self.cloud_visible {
            self.refresh_cloud();
        }
        self.cloud_visible = visible;
    }
    pub fn refresh_cloud(&mut self) {
        if self.cloud.pending.is_none() {
            self.cloud.due = true;
            self.cloud.status = Status::Unknown;
        }
        if self.cloud.sync_pending.is_none() {
            self.cloud.sync_due = true;
        }
    }
    fn cloud_readable(&self) -> bool {
        self.cloud_visible
            && self.account.stage == AccountStage::SignedIn
            && self.account.active_account_id.is_some()
    }
    pub(super) fn take_cloud_request(&mut self) -> Option<AccountCommand> {
        if !self.cloud_readable() {
            return None;
        }
        if self.cloud.due && self.cloud.pending.is_none() {
            let id = next_request_id();
            self.cloud.due = false;
            self.cloud.status = Status::Loading;
            self.cloud.pending = Some(Pending {
                id,
                submission: CommandSubmissionId::next(),
                generation: None,
            });
            return Some(AccountCommand::InspectCloudSubscription { request_id: id });
        }
        if self.cloud.sync_pending.is_some() {
            return None;
        }
        // A write the person has already made is more urgent than a re-read, and
        // a re-read issued first would answer with the state they just changed.
        if let Some((profile_id, enabled)) = self.cloud.set_request.take() {
            let id = next_request_id();
            self.cloud.sync_pending = Some(SyncPending {
                id,
                submission: CommandSubmissionId::next(),
                generation: None,
                set: Some((profile_id.clone(), enabled)),
            });
            return Some(AccountCommand::SetProfileCloudSync {
                request_id: id,
                profile_id,
                enabled,
            });
        }
        // Per-Profile sync waits for confirmed account access.
        if self.cloud.sync_due && self.cloud_access() == Some(true) {
            let id = next_request_id();
            self.cloud.sync_due = false;
            self.cloud.sync_pending = Some(SyncPending {
                id,
                submission: CommandSubmissionId::next(),
                generation: None,
                set: None,
            });
            return Some(AccountCommand::InspectCloudSync { request_id: id });
        }
        None
    }
    pub(super) fn cloud_submission(&self, command: &AccountCommand) -> Option<CommandSubmissionId> {
        match command {
            AccountCommand::InspectCloudSubscription { request_id } => self
                .cloud
                .pending
                .as_ref()
                .filter(|p| p.id == *request_id)
                .map(|p| p.submission),
            AccountCommand::InspectCloudSync { request_id }
            | AccountCommand::SetProfileCloudSync { request_id, .. } => self
                .cloud
                .sync_pending
                .as_ref()
                .filter(|p| p.id == *request_id)
                .map(|p| p.submission),
            _ => None,
        }
    }
    pub(super) fn cloud_admitted(&mut self, admission: CommandAdmission) -> bool {
        if admission.owner_scope != self.account.active_scope {
            return false;
        }
        if let Some(pending) = self
            .cloud
            .pending
            .as_mut()
            .filter(|p| p.submission == admission.submission)
        {
            pending.generation = Some(admission.generation);
            return true;
        }
        if let Some(pending) = self
            .cloud
            .sync_pending
            .as_mut()
            .filter(|p| p.submission == admission.submission)
        {
            pending.generation = Some(admission.generation);
            return true;
        }
        false
    }
    pub(super) fn cloud_failed(&mut self, failure: CommandFailure) -> bool {
        if self
            .cloud
            .pending
            .as_ref()
            .is_some_and(|p| p.submission == failure.submission)
        {
            self.cloud.pending = None;
            self.cloud.status = Status::Unavailable;
            return true;
        }
        let Some(pending) = self
            .cloud
            .sync_pending
            .take_if(|p| p.submission == failure.submission)
        else {
            return false;
        };
        // A write that never reached the Agent must not leave the switch stuck
        // mid-flight. The row goes back to the last state the Agent confirmed.
        if let Some((profile_id, _)) = pending.set {
            self.note_cloud_sync_error(&profile_id, "cloud_sync_unavailable");
        }
        self.cloud.sync_due = true;
        true
    }
    fn note_cloud_sync_error(&mut self, profile_id: &str, code: &str) {
        let rows = self.cloud.sync.get_or_insert_with(Vec::new);
        match rows.iter_mut().find(|row| row.profile_id == profile_id) {
            Some(row) => row.error = Some(code.to_owned()),
            None => rows.push(CloudSyncProfile {
                profile_id: profile_id.to_owned(),
                enabled: false,
                workspaces: 0,
                synced_workspaces: 0,
                error: Some(code.to_owned()),
            }),
        }
    }
    pub(super) fn cloud_result(
        &mut self,
        generation: u64,
        result: &AccountOperationResult,
    ) -> bool {
        match result {
            AccountOperationResult::CloudSubscription {
                request_id,
                current,
                access,
            } => {
                let Some(pending) = self.cloud.pending.as_ref() else {
                    return false;
                };
                if *request_id != pending.id
                    || !pending
                        .generation
                        .is_none_or(|expected| expected == generation)
                {
                    return false;
                }
                self.cloud.access = access.or(*current);
                self.cloud.status = match current {
                    Some(true) => Status::Manage,
                    Some(false) if self.cloud.access == Some(true) => Status::Active,
                    Some(false) => Status::Subscribe,
                    None => Status::Unavailable,
                };
                self.cloud.pending = None;
                // Confirmed access makes the per-Profile state worth reading.
                if self.cloud_access() == Some(true) {
                    self.cloud.sync_due = true;
                } else {
                    self.cloud.sync = None;
                }
                true
            }
            AccountOperationResult::CloudSync {
                request_id,
                profiles,
            } => {
                let Some(pending) = self.cloud.sync_pending.as_ref() else {
                    return false;
                };
                if *request_id != pending.id
                    || !pending
                        .generation
                        .is_none_or(|expected| expected == generation)
                {
                    return false;
                }
                let was_write = pending.set.is_some();
                self.cloud.sync = Some(profiles.clone());
                self.cloud.sync_pending = None;
                // A write is answered with the state it produced; the re-read
                // after it is what catches a Workspace that was still uploading.
                self.cloud.sync_due = was_write;
                true
            }
            AccountOperationResult::Failed { code } => {
                let Some(pending) = self.cloud.sync_pending.as_ref() else {
                    return false;
                };
                // `Failed` carries no request id, so an *admitted* generation is
                // the only correlation there is. A write that has not been
                // admitted yet claims nothing: it would swallow a refusal that
                // belonged to some other flow entirely.
                if pending.generation != Some(generation) || pending.set.is_none() {
                    return false;
                }
                let set = self.cloud.sync_pending.take().and_then(|p| p.set);
                if let Some((profile_id, _)) = set {
                    let code = code.clone();
                    self.note_cloud_sync_error(&profile_id, &code);
                }
                self.cloud.sync_due = true;
                true
            }
            _ => false,
        }
    }

    // ---- the card ---------------------------------------------------------

    /// The Profiles this account owns on this device — the `m` in "syncing n of
    /// m profiles", and the only rows that may carry a switch.
    fn account_profile_ids(&self) -> Vec<String> {
        let Some(active) = self.account.active_account_id.as_deref() else {
            return Vec::new();
        };
        self.profiles
            .iter()
            .filter(|row| {
                matches!(
                    koru_store_core::profile_account::LocalProfileAttribution::from_binding(
                        row.account.as_deref().unwrap_or(""),
                    ),
                    koru_store_core::profile_account::LocalProfileAttribution::Account(binding)
                        if binding.identity_id() == active
                )
            })
            // One this sign-in is not linked to cannot sync from here, so
            // it is neither the `n` nor the `m`.
            .filter(|row| !self.profile_unlinked_from_sign_in(row))
            .map(|row| row.id.to_storage())
            .collect()
    }
    pub fn cloud_profile_counts(&self) -> Option<(usize, usize)> {
        let sync = self.cloud.sync.as_ref()?;
        let ids = self.account_profile_ids();
        let on = ids
            .iter()
            .filter(|id| sync.iter().any(|row| row.profile_id == **id && row.enabled))
            .count();
        Some((on, ids.len()))
    }
    pub fn cloud_message(&self) -> TextKey {
        // Not an outage: there is no Koru Cloud in this build to check.
        if self.account.stage == AccountStage::NotConfigured {
            return TextKey::CloudNotConfiguredBody;
        }
        if matches!(
            self.account.stage,
            AccountStage::Restoring
                | AccountStage::Refreshing
                | AccountStage::WaitingForBrowser
                | AccountStage::Exchanging
                | AccountStage::AwaitingAccountCreation
        ) {
            return TextKey::CloudLoading;
        }
        if self.account.stage != AccountStage::SignedIn && self.cloud.pending.is_none() {
            return TextKey::CloudSignIn;
        }
        match self.cloud.status {
            Status::Unknown | Status::Loading => TextKey::CloudLoading,
            Status::Unavailable => TextKey::CloudUnavailable,
            Status::Subscribe => TextKey::CloudSubscribeIntro,
            Status::Manage if self.cloud_access() == Some(false) => TextKey::CloudAccessInactive,
            Status::Manage | Status::Active if self.cloud_profile_counts().is_some() => {
                TextKey::CloudSubscribedSyncing
            }
            Status::Manage | Status::Active => TextKey::CloudSubscribed,
        }
    }
    pub fn cloud_body_text(&self, lang: Lang) -> String {
        let key = self.cloud_message();
        match (key, self.cloud_profile_counts()) {
            (TextKey::CloudSubscribedSyncing, Some((on, total))) => koru_i18n::fill(
                koru_i18n::t(lang, key),
                &[&on.to_string(), &total.to_string()],
            ),
            _ => koru_i18n::t(lang, key).to_owned(),
        }
    }
    pub fn cloud_actionable(&self) -> bool {
        self.cloud.pending.is_none()
            && match self.account.stage {
                AccountStage::SignedIn => !matches!(
                    self.cloud.status,
                    Status::Loading | Status::Unknown | Status::Active
                ),
                AccountStage::SignedOut
                | AccountStage::Cancelled
                | AccountStage::Failed
                | AccountStage::Unavailable => true,
                _ => false,
            }
    }
    /// A current subscription is not something to press — the only thing left to
    /// do with it is go and change the billing, so it reads as a link.
    pub fn cloud_action_is_link(&self) -> bool {
        self.account.stage == AccountStage::SignedIn && self.cloud.status == Status::Manage
    }
    pub fn cloud_learn_more_visible(&self) -> bool {
        self.cloud_actionable()
            && self.account.stage == AccountStage::SignedIn
            && self.cloud.status == Status::Subscribe
    }
    pub fn cloud_action_label(&self) -> &'static str {
        let key = if self.account.stage != AccountStage::SignedIn {
            TextKey::CloudSignInAction
        } else {
            match self.cloud.status {
                Status::Manage => TextKey::CloudManageSubscription,
                Status::Subscribe => TextKey::CloudSubscribe,
                _ => TextKey::TryAgain,
            }
        };
        koru_i18n::t(crate::format::lang(), key)
    }
    pub(super) fn cloud_activate(&mut self) {
        if !self.cloud_actionable() {
            return;
        }
        if self.account.stage != AccountStage::SignedIn {
            self.perform(Intent::Session);
            return;
        }
        match self.cloud.status {
            Status::Manage | Status::Subscribe => {
                let page = if self.cloud.status == Status::Manage {
                    crate::website::Page::Dashboard
                } else {
                    crate::website::Page::Pricing
                };
                self.open_cloud_page(page);
            }
            _ => self.refresh_cloud(),
        }
    }
    pub(super) fn cloud_learn_more_activate(&mut self) {
        if self.cloud_learn_more_visible() {
            self.open_cloud_page(crate::website::Page::Pricing);
        }
    }
    fn open_cloud_page(&mut self, page: crate::website::Page) {
        self.cloud_uri = Some(crate::website::page_url(
            &crate::website::origin(),
            crate::format::lang(),
            page,
        ));
    }

    // ---- the per-Profile switch -------------------------------------------

    /// Whether this row carries a switch, and what to say when it does not work.
    ///
    /// A Profile with no account binding is not a Cloud thing at all, so it gets
    /// no switch rather than a dead one — an affordance that can never do
    /// anything is worse than no affordance.
    pub fn cloud_sync_availability(&self, profile: &ProfileSummary) -> SyncAvailability {
        use koru_store_core::profile_account::LocalProfileAttribution as Attribution;
        let Attribution::Account(binding) =
            Attribution::from_binding(profile.account.as_deref().unwrap_or(""))
        else {
            return SyncAvailability::Hidden;
        };
        if self.account.stage == AccountStage::NotConfigured {
            return SyncAvailability::Disabled(TextKey::CloudNotConfigured);
        }
        if self.account.stage != AccountStage::SignedIn {
            return SyncAvailability::Disabled(TextKey::CloudSyncSignInReason);
        }
        // Before the subscription: with or without one, this sign-in cannot
        // sync a Profile it is not linked to.
        if self.profile_unlinked_from_sign_in(profile) {
            return SyncAvailability::Disabled(TextKey::CloudSyncNotLinkedReason);
        }
        if self.cloud_access() != Some(true) {
            if self.cloud_access().is_none() {
                return SyncAvailability::Disabled(self.cloud_message());
            }
            if self.cloud.status == Status::Manage {
                return SyncAvailability::Disabled(TextKey::CloudAccessInactive);
            }
            return SyncAvailability::Disabled(TextKey::CloudSyncSubscribeReason);
        }
        match self.account.active_account_id.as_deref() {
            Some(active) if binding.identity_id() == active => SyncAvailability::Enabled,
            _ => SyncAvailability::Disabled(TextKey::CloudSyncSubscribeReason),
        }
    }
    fn cloud_sync_row(&self, profile: ProfileId) -> Option<&CloudSyncProfile> {
        let id = profile.to_storage();
        self.cloud
            .sync
            .as_ref()?
            .iter()
            .find(|row| row.profile_id == id)
    }
    /// `Some(target)` while a `SetProfileCloudSync` for this Profile is in
    /// flight, whether or not it has reached the wire yet.
    pub fn cloud_sync_pending_for(&self, profile: ProfileId) -> Option<bool> {
        let id = profile.to_storage();
        self.cloud
            .set_request
            .as_ref()
            .or_else(|| {
                self.cloud
                    .sync_pending
                    .as_ref()
                    .and_then(|pending| pending.set.as_ref())
            })
            .filter(|(pending_id, _)| *pending_id == id)
            .map(|(_, enabled)| *enabled)
    }
    /// What the switch shows. A pending change shows its *target*, so the switch
    /// moves under the finger and the dimming is what says it is not settled.
    pub fn cloud_sync_on(&self, profile: ProfileId) -> bool {
        // A Profile this sign-in is not linked to does not sync from here,
        // whatever this device remembers from before it was unlinked: its
        // switch reads off, and its line says why instead of *Koru Cloud
        // unavailable*.
        if self
            .profiles
            .iter()
            .any(|row| row.id == profile && self.profile_unlinked_from_sign_in(row))
        {
            return false;
        }
        self.cloud_sync_pending_for(profile).unwrap_or_else(|| {
            self.cloud_sync_row(profile).map_or_else(
                || {
                    self.account
                        .cloud_profiles
                        .iter()
                        .any(|row| row.profile_id == profile.to_string())
                },
                |row| row.enabled,
            )
        })
    }
    /// **This Profile's Koru Cloud connection**, the one value its row
    /// subtitle and every workspace row under its sync progress read, so the
    /// two cannot disagree. A refusal the Agent gave this Profile's Cloud sync
    /// row counts as unavailable.
    pub fn cloud_connection(&self, profile: ProfileId) -> Connection {
        Connection::of(
            &self.account,
            &profile.to_string(),
            self.cloud_sync_row(profile)
                .is_some_and(|row| row.error.is_some()),
        )
    }
    /// The Cloud half of a Profile row's subtitle, appended after the account
    /// label. `None` when the row has nothing Cloud-related to say.
    pub fn cloud_sync_subtitle(&self, profile: &ProfileSummary) -> Option<String> {
        let lang = crate::format::lang();
        if self.cloud_sync_on(profile.id) && self.cloud_sync_pending_for(profile.id).is_none() {
            return Some(self.cloud_connection(profile.id).sentence(lang));
        }
        match self.cloud_sync_availability(profile) {
            SyncAvailability::Hidden => return None,
            SyncAvailability::Disabled(reason) => {
                return Some(koru_i18n::t(lang, reason).to_owned());
            }
            SyncAvailability::Enabled => {}
        }
        if let Some(target) = self.cloud_sync_pending_for(profile.id) {
            let key = if target {
                TextKey::CloudSyncTurningOn
            } else {
                TextKey::CloudSyncTurningOff
            };
            return Some(koru_i18n::t(lang, key).to_owned());
        }
        let row = self.cloud_sync_row(profile.id)?;
        // A code is shown verbatim: it is a stable identifier the support
        // conversation can search for, not a sentence to be translated into one.
        if let Some(code) = row.error.as_deref() {
            return Some(koru_i18n::fill(
                koru_i18n::t(lang, TextKey::CloudSyncError),
                &[code],
            ));
        }
        None
    }
    pub(super) fn cloud_sync_activate(&mut self, profile: ProfileId) {
        let Some(row) = self.profiles.iter().find(|row| row.id == profile) else {
            return;
        };
        if self.cloud_sync_availability(row) != SyncAvailability::Enabled {
            return;
        }
        // One write at a time. A second press while the first is unanswered is
        // dropped rather than queued: the Agent's answer is the truth, and two
        // writes in flight would make the second one race it.
        if self.cloud.set_request.is_some() || self.cloud.sync_pending.is_some() {
            return;
        }
        let enabled = !self.cloud_sync_on(profile);
        // Nothing is asked either way: on is the Profile and its Workspaces,
        // and the Agent opens the Store for it; off is D227's immediate stop.
        self.cloud.set_request = Some((profile.to_storage(), enabled));
    }
}

/// Whether a Profile row carries a Cloud sync switch, and why it is inert.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyncAvailability {
    Hidden,
    Disabled(TextKey),
    Enabled,
}

fn next_request_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests;
