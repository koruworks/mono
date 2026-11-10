// SPDX-License-Identifier: MPL-2.0

//! The waiting pane uses one element frame for paint, input and accessibility.

use super::*;
use koru_draw::el::{self, El, Frame, FrameCx, Interaction, TextCache, boxed, col, row, text};
use koru_draw::theme::{Ink, Space, Surface, metrics, roles};
use koru_i18n::{Key as Words, fill, t};
use koru_workspace_host::accessibility::{ControlPart, Occurrence, SemanticNode};
use koru_workspace_host::profile_wait::{ProfileWait, WaitReading};
use std::cell::RefCell;
use winit::keyboard::NamedKey;

pub(super) const SURFACE: &str = "profile-wait";
const CANCEL: el::Key = el::Key::surface(SURFACE, "cancel");
const RETRY: el::Key = el::Key::surface(SURFACE, "retry");
const MANAGE: el::Key = el::Key::surface(SURFACE, "manage");
const CLOUD: el::Key = el::Key::surface(SURFACE, "cloud");
const RESUME: el::Key = el::Key::surface(SURFACE, "resume-removal");
const KEEP: el::Key = el::Key::surface(SURFACE, "keep-profile");
/// An empty Profile's one way out (root-catalog §3.10): the name prompt.
const CREATE: el::Key = el::Key::surface(SURFACE, "new-workspace");
const DOTS: el::Key = el::Key::surface(SURFACE, "dots");

pub(super) struct WaitPane {
    pub waiting: Option<ProfileWait>,
    cache: RefCell<TextCache>,
    focus: el::Key,
    hot: Option<el::Key>,
    dots: koru_ui::PulsingDots,
    pub revision: u64,
    lease_due: Option<Instant>,
}

impl Default for WaitPane {
    fn default() -> Self {
        Self {
            waiting: None,
            cache: RefCell::default(),
            focus: CANCEL,
            hot: None,
            dots: koru_ui::PulsingDots::new(Instant::now()),
            revision: 0,
            lease_due: None,
        }
    }
}

impl WaitPane {
    pub fn set(&mut self, waiting: Option<ProfileWait>) -> bool {
        if self.waiting == waiting {
            return false;
        }
        if self.waiting.as_ref().map(|w| w.attempt) != waiting.as_ref().map(|w| w.attempt)
            || waiting
                .as_ref()
                .is_some_and(|wait| matches!(wait.reading, WaitReading::Removing { .. }))
            || self
                .waiting
                .as_ref()
                .map(|w| std::mem::discriminant(&w.reading))
                != waiting.as_ref().map(|w| std::mem::discriminant(&w.reading))
        {
            self.focus = CANCEL;
            self.hot = None;
        }
        self.lease_due = waiting.as_ref().and_then(|wait| {
            let WaitReading::Removing { mark } = &wait.reading else {
                return None;
            };
            let now = koru_store_core::Timestamp::now();
            mark.lease_live(now).then(|| {
                Instant::now()
                    + std::time::Duration::from_millis(
                        mark.last_step_at
                            .millis()
                            .saturating_add(60_001)
                            .saturating_sub(now.millis())
                            .max(1) as u64,
                    )
            })
        });
        self.waiting = waiting;
        self.revision = self.revision.wrapping_add(1);
        true
    }
}

pub(super) fn message(reading: &WaitReading, lang: koru_i18n::Lang) -> String {
    let key = match reading {
        WaitReading::Unknown => Words::ProfileWaitUnknown,
        WaitReading::Cloud { .. } => Words::ProfileWaitCloud,
        WaitReading::Preparing => Words::ProfileWaitPreparing,
        WaitReading::Pending { .. } => Words::ProfileWaitPending,
        WaitReading::Uncertain { .. } => Words::ProfileWaitUncertain,
        WaitReading::ListingPending { .. } => Words::ProfileWaitListing,
        WaitReading::Removing { mark } => {
            if mark.kind == koru_store_core::profile_removal::RemovalKind::HostedCleanup {
                Words::ProfileRemovalHosted
            } else if mark.lease_live(koru_store_core::Timestamp::now()) {
                Words::ProfileWaitRemoving
            } else {
                Words::ProfileRemovalInterrupted
            }
        }
        WaitReading::Unavailable { .. } => Words::ProfileWaitUnavailable,
        WaitReading::Empty => Words::ProfileEmptyWorkspaces,
        WaitReading::EnrollmentFailed { code, .. } | WaitReading::ContentFailed { code, .. } => {
            match code.as_deref() {
                Some("session_revoked" | "cloud_access_revoked" | "authentication_required") => {
                    Words::ProfileWaitSignIn
                }
                Some(
                    "profile_access_revoked" | "profile_not_authorized" | "cloud_selection_denied",
                ) => Words::ProfileWaitAccess,
                Some("device_verification_failed" | "attestation_invalid") => {
                    Words::ProfileWaitVerify
                }
                Some("older_peer" | "enrollment_protocol_unsupported") => {
                    Words::ProfileWaitPeerUpdate
                }
                Some("cloud_offline") => Words::ProfileWaitOffline,
                Some("cloud_dataset_mismatch") => Words::ProfileWaitDataset,
                Some("cloud_descriptor_not_ready" | "cloud_bootstrap_failed") => {
                    Words::ProfileWaitProvisioning
                }
                Some("cloud_rate_limited" | "enrollment_rate_limited") => {
                    Words::ProfileWaitRateLimited
                }
                Some("credential_expired") => Words::ProfileWaitCredential,
                Some(
                    "trust_persistence_failed"
                    | "connection_save_failed"
                    | "storage_failed"
                    | "store_unavailable",
                ) => Words::ProfileWaitStorage,
                _ => Words::CloudUnavailable,
            }
        }
    };
    t(lang, key).into()
}

impl MainView {
    pub(super) fn profile_wait_deadline(&self) -> Option<Instant> {
        self.profile_wait_visible()
            .then_some(self.profile_wait.lease_due)
            .flatten()
    }

    pub(super) fn maintain_profile_wait(&mut self, now: Instant) -> bool {
        if self.profile_wait_visible() && self.profile_wait.lease_due.is_some_and(|due| now >= due)
        {
            self.profile_wait.lease_due = None;
            self.profile_wait.focus = CANCEL;
            self.profile_wait.revision = self.profile_wait.revision.wrapping_add(1);
            // Lease expiry changes both recovery choices and the rail/menu
            // sentence even though no Store mark or catalog fact changed.
            let label = self
                .profile_wait
                .waiting
                .as_ref()
                .map(|wait| message(&wait.reading, crate::format::lang()));
            self.explore.set_workspace_waiting(label);
            return true;
        }
        false
    }

    pub(super) fn profile_wait_visible(&self) -> bool {
        self.profile_wait.waiting.is_some()
            && !matches!(
                self.tabs().active_tab().view,
                TabView::Manage(_) | TabView::Settings(_) | TabView::Feedback(_)
            )
    }

    fn wait_actions(&self) -> Vec<(el::Key, Words)> {
        let Some(wait) = &self.profile_wait.waiting else {
            return vec![];
        };
        let mut actions = vec![];
        match &wait.reading {
            WaitReading::Removing { mark }
                if !mark.lease_live(koru_store_core::Timestamp::now()) =>
            {
                if mark.kind == koru_store_core::profile_removal::RemovalKind::HostedCleanup {
                    actions.push((RESUME, Words::ProfileRemovalFinish));
                } else {
                    actions.push((RESUME, Words::ProfileRemovalResume));
                    actions.push((KEEP, Words::ProfileRemovalKeep));
                }
            }
            WaitReading::EnrollmentFailed { .. }
            | WaitReading::Unavailable { .. }
            | WaitReading::Uncertain { .. }
            | WaitReading::ListingPending { .. } => {
                actions.push((RETRY, Words::ProfileChooserRetry));
            }
            WaitReading::ContentFailed {
                automatic: false, ..
            } => actions.push((CLOUD, Words::ProfileWaitSignIn)),
            // Empty is a state, not a failure: nothing to retry, and the one
            // explicit action is to make a workspace (root-catalog §3.10).
            WaitReading::Empty => actions.push((CREATE, Words::TerminalNewWorkspace)),
            _ => {}
        }
        let owner = crate::manage::workspaces::catalog_profile(wait.attempt.profile);
        if self
            .workspace_controller
            .borrow()
            .notice_for(owner)
            .is_some_and(|notice| notice.is_retryable())
            && !actions.iter().any(|(key, _)| *key == RETRY)
        {
            actions.push((RETRY, Words::ProfileChooserRetry));
        }
        actions.push((MANAGE, Words::ManageWorkspaces));
        actions.push((CANCEL, Words::WorkspacesCancel));
        actions
    }

    fn wait_tree(&self) -> El {
        let wait = self.profile_wait.waiting.as_ref().expect("waiting pane");
        let lang = crate::format::lang();
        let label = message(&wait.reading, lang);
        let mut tree = col("Profile availability")
            .id(el::Key::surface(SURFACE, "root"))
            .bg(Surface::Ground)
            .pad(Space::S6)
            .gap(Space::S3)
            .semantics(el::Role::Group)
            .name_text(&label)
            .child(
                text(&label)
                    .id(el::Key::surface(SURFACE, "status"))
                    .role(roles::SUBTITLE)
                    .wrap()
                    .semantics(el::Role::Heading)
                    .name_text(&label),
            );
        if self.profile_wait_animating() {
            tree = tree.child(
                boxed("Pending indicator")
                    .id(DOTS)
                    .w(metrics::BUTTON_MIN)
                    .h(metrics::BUTTON_MIN),
            );
        }
        if let WaitReading::Cloud {
            counts: Some((admitted, discovered)),
            ..
        } = &wait.reading
        {
            let counts = fill(
                t(lang, Words::ProfileWaitAdmitted),
                &[
                    &koru_i18n::number::decimal(lang, *admitted as u64),
                    &koru_i18n::number::decimal(lang, *discovered as u64),
                ],
            );
            tree = tree.child(
                text(&counts)
                    .id(el::Key::surface(SURFACE, "counts"))
                    .wrap()
                    .semantics(el::Role::Group)
                    .name_text(&counts),
            );
        }
        if let WaitReading::ContentFailed {
            automatic: true,
            retry_at_millis,
            ..
        } = &wait.reading
        {
            let status = match retry_at_millis {
                Some(at) => fill(
                    t(lang, Words::ProfileWaitRetryAt),
                    &[&crate::format::absolute_local(lang, *at)],
                ),
                None => t(lang, Words::ProfileWaitRetrying).into(),
            };
            tree = tree.child(
                text(&status)
                    .id(el::Key::surface(SURFACE, "retry-time"))
                    .wrap()
                    .semantics(el::Role::Group)
                    .name_text(&status),
            );
        }
        if let WaitReading::Uncertain { issues } | WaitReading::ListingPending { issues } =
            &wait.reading
        {
            for issue in issues {
                let id = crate::manage::workspaces::app_workspace(issue.workspace);
                let name = self
                    .workspace_facts(id)
                    .map_or_else(|| id.to_storage(), |row| row.name);
                tree = tree.child(
                    text(&name)
                        .id(el::Key::model(
                            issue.workspace.as_uuid().as_u128(),
                            "affected",
                        ))
                        .wrap()
                        .semantics(el::Role::ListItem)
                        .name_text(&name),
                );
            }
        }
        let owner = crate::manage::workspaces::catalog_profile(wait.attempt.profile);
        if let Some(notice) = self.workspace_controller.borrow().notice_for(owner) {
            tree = tree.child(
                text(notice.text())
                    .id(el::Key::surface(SURFACE, "notice"))
                    .wrap()
                    .semantics(el::Role::Group)
                    .name_text(notice.text()),
            );
        }
        tree.child(row("Availability actions").gap(Space::S2).children(
            self.wait_actions().into_iter().map(|(key, label)| {
                boxed("Availability action")
                    .id(key)
                    .fill()
                    .min_h(metrics::BUTTON_MIN)
                    .pad_x(Space::S3)
                    .hit()
                    .focusable()
                    .semantics(el::Role::Button)
                    .name(label)
                    .action(el::Action::Click)
                    .states(&crate::panes::sidebar::rail_elements::RAIL_SQUARE_STATES)
                    .child(text(t(lang, label)).wrap())
            }),
        ))
    }

    fn with_wait_frame<R>(&self, read: impl FnOnce(&Frame) -> R) -> R {
        let tree = self.wait_tree();
        crate::input_engine::with(|engine| {
            let frame = tree.layout(
                engine,
                &mut self.profile_wait.cache.borrow_mut(),
                self.content(),
                FrameCx::root(),
            );
            read(&frame)
        })
    }

    pub(super) fn draw_profile_wait(&mut self, scene: &mut Scene, root: Affine) {
        let tree = self.wait_tree();
        let area = self.content();
        let frame = tree.layout(
            &mut self.engine,
            &mut self.profile_wait.cache.borrow_mut(),
            area,
            FrameCx::root().with_interaction(Interaction {
                hot: self.profile_wait.hot,
                focused: Some(self.profile_wait.focus),
                ..Interaction::default()
            }),
        );
        frame.paint(&tree, scene, root, &mut self.engine);
        if self.profile_wait_animating()
            && let Some(dots) = frame.node(DOTS)
        {
            self.profile_wait.dots.draw(
                scene,
                root,
                dots.rect,
                Instant::now(),
                Ink::Secondary.of(theme::p()),
            );
        }
    }

    pub(super) fn profile_wait_animating(&self) -> bool {
        self.profile_wait_visible()
            && self.profile_wait.waiting.as_ref().is_some_and(|wait| {
                matches!(
                    wait.reading,
                    WaitReading::Cloud { .. }
                        | WaitReading::Preparing
                        | WaitReading::Pending { .. }
                )
            })
    }

    fn activate_wait(&mut self, key: el::Key) -> bool {
        if !self.profile_wait_visible()
            || !self.wait_actions().iter().any(|(action, _)| *action == key)
        {
            return false;
        }
        let attempt = self.profile_wait.waiting.as_ref().unwrap().attempt;
        match key {
            RESUME => {
                let WaitReading::Removing { mark } =
                    &self.profile_wait.waiting.as_ref().unwrap().reading
                else {
                    return false;
                };
                if mark.kind == koru_store_core::profile_removal::RemovalKind::HostedCleanup {
                    self.ask_pane(PaneRequest::FinishHostedProfileDeletion {
                        profile: attempt.profile,
                        discovered_after_offline: true,
                    })
                } else {
                    self.ask_pane(PaneRequest::RemoveLocalProfile {
                        profile: attempt.profile,
                        delete_workspaces: true,
                        preview: None,
                    })
                }
            }
            KEEP => {
                let WaitReading::Removing { mark } =
                    &self.profile_wait.waiting.as_ref().unwrap().reading
                else {
                    return false;
                };
                self.workspace_controller.borrow_mut().keep_profile(
                    crate::manage::workspaces::catalog_profile(attempt.profile),
                    mark.attempt,
                );
                self.arm_workspaces_poll();
                true
            }
            CANCEL => self.ask_pane(PaneRequest::CancelProfileWait(attempt)),
            RETRY => {
                let owner = crate::manage::workspaces::catalog_profile(attempt.profile);
                let lifecycle_retry = self
                    .workspace_controller
                    .borrow()
                    .notice_for(owner)
                    .is_some_and(|notice| notice.is_retryable());
                if lifecycle_retry {
                    self.workspace_controller
                        .borrow_mut()
                        .perform(koru_workspaces_ui::Action::Retry);
                    self.arm_workspaces_poll();
                    true
                } else {
                    self.ask_pane(PaneRequest::RetryProfileWait(attempt))
                }
            }
            CLOUD => self.open_manage(crate::manage::Page::Cloud),
            MANAGE => self.open_manage_workspaces(Some(attempt.profile)),
            CREATE => self.prompt_new_workspace(),
            _ => false,
        }
    }

    pub(super) fn profile_wait_mouse_down(&mut self, x: f32, y: f32) -> bool {
        if !self.profile_wait_visible() || !self.content().contains(x, y) {
            return false;
        }
        if let Some(key) = self.with_wait_frame(|frame| frame.hit_at(x, y)) {
            self.profile_wait.focus = key;
            self.activate_wait(key);
        }
        true
    }

    pub(super) fn profile_wait_mouse_move(&mut self, x: f32, y: f32) -> bool {
        if !self.profile_wait_visible() {
            return false;
        }
        let hot = self.with_wait_frame(|frame| frame.hit_at(x, y));
        let changed = self.profile_wait.hot != hot;
        self.profile_wait.hot = hot;
        changed
    }

    pub(super) fn profile_wait_key(&mut self, key: &Key, mods: ModifiersState) -> bool {
        if !self.profile_wait_visible() {
            return false;
        }
        if mods.control_key() || mods.super_key() || mods.alt_key() {
            return true;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.activate_wait(CANCEL);
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                self.activate_wait(self.profile_wait.focus);
            }
            Key::Named(NamedKey::Tab) => {
                let controls = self.with_wait_frame(|frame| {
                    frame
                        .nodes
                        .iter()
                        .filter(|node| node.focusable)
                        .filter_map(|node| node.key)
                        .collect::<Vec<_>>()
                });
                if !controls.is_empty() {
                    let at = controls
                        .iter()
                        .position(|key| *key == self.profile_wait.focus)
                        .unwrap_or(0);
                    let next = if mods.shift_key() {
                        (at + controls.len() - 1) % controls.len()
                    } else {
                        (at + 1) % controls.len()
                    };
                    self.profile_wait.focus = controls[next];
                    self.profile_wait.revision = self.profile_wait.revision.wrapping_add(1);
                }
            }
            _ => {}
        }
        true
    }

    pub(super) fn profile_wait_semantics(&self, cx: &accessibility::Shell) -> Option<SemanticNode> {
        if !self.profile_wait_visible() {
            return None;
        }
        let sequence = self.profile_wait.waiting.as_ref()?.attempt.sequence;
        self.with_wait_frame(|frame| {
            frame
                .semantics()
                .first()
                .map(|node| semantic_node(cx, sequence, node))
        })
    }

    pub(super) fn profile_wait_focus(&self, cx: &accessibility::Shell) -> Option<Occurrence> {
        if !self.profile_wait_visible() {
            return None;
        }
        Some(cx.instance(
            SURFACE,
            self.profile_wait.waiting.as_ref()?.attempt.sequence,
            ControlPart::new(self.profile_wait.focus.part),
        ))
    }

    pub(super) fn profile_wait_action(
        &mut self,
        target: Occurrence,
        action: accesskit::Action,
    ) -> bool {
        let Some(wait) = &self.profile_wait.waiting else {
            return false;
        };
        if !self.profile_wait_visible()
            || accessibility::instance_of(target, SURFACE) != Some(wait.attempt.sequence)
        {
            return false;
        }
        let key = self
            .wait_actions()
            .into_iter()
            .map(|(key, _)| key)
            .find(|key| ControlPart::new(key.part) == target.part);
        let Some(key) = key else {
            return false;
        };
        match action {
            accesskit::Action::Focus => {
                self.profile_wait.focus = key;
                self.profile_wait.revision = self.profile_wait.revision.wrapping_add(1);
                true
            }
            accesskit::Action::Click => self.activate_wait(key),
            _ => false,
        }
    }
}

fn semantic_node(cx: &accessibility::Shell, sequence: u64, node: &el::SemNode) -> SemanticNode {
    let key = node.key.unwrap_or(el::Key::surface(SURFACE, "root"));
    let occurrence = match key.object {
        el::Object::Model(id) => Occurrence::model(
            cx.window,
            cx.view,
            uuid::Uuid::from_u128(id),
            ControlPart::new(key.part),
        ),
        el::Object::Surface(_) => cx.instance(SURFACE, sequence, ControlPart::new(key.part)),
    };
    let role = match node.role {
        el::Role::Button => accesskit::Role::Button,
        el::Role::Heading => accesskit::Role::Heading,
        el::Role::ListItem => accesskit::Role::ListItem,
        _ => accesskit::Role::Group,
    };
    let mut result = SemanticNode::new(occurrence, role).with_bounds(node.bounds);
    result = match &node.name {
        Some(el::Name::Key(key)) => result.with_label(t(cx.lang, *key)),
        Some(el::Name::Text(text)) => result.with_label(text.clone()),
        None => result,
    };
    result
        .with_actions(node.actions.iter().map(|action| match action {
            el::Action::Click => accesskit::Action::Click,
            el::Action::Focus => accesskit::Action::Focus,
        }))
        .with_children(
            node.children
                .iter()
                .map(|child| semantic_node(cx, sequence, child))
                .collect(),
        )
}

#[cfg(test)]
mod tests;
