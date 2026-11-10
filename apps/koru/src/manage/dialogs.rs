// SPDX-License-Identifier: MPL-2.0

//! The questions a Profile's page asks, drawn over the whole tab with the
//! shared dialog (`koru_ui::Confirm`).
//!
//! Two kinds, in one enum:
//!
//! - **The page's own**: *Sign out…*, *Unlink…* of a Google Account that is
//!   not the last, and *Linked devices*' *Unlink…* of a device. Nothing is
//!   sent until one is answered.
//! - **The controller's questions, shown as dialogs**: deleting a Profile
//!   (the shared batch) or removing it from this device, making a Profile
//!   offline-only and connecting a Google Account are states of the
//!   controller (`confirming_remove`, `confirming_offline`,
//!   `pending_add_google`) that Manage Profiles drew in its detail pane.
//!   [`ManageView::settle_dialogs`] keeps a dialog up exactly while the
//!   controller is asking, so the controller stays the one place that knows
//!   a question is open, and answering is the same `Intent` it always was.
//!
//! **The footer's two removals are two actions, not one with a sub-menu**
//! (root-catalog QUESTIONS Q4, decided 2026-11-09): *Delete profile and its
//! workspaces…* asks the Store for the batch's preview and the question opens
//! over it when it arrives; *Remove from this device…* opens the device-local
//! question at once. Both are the controller's `confirming_remove`, told
//! apart by whether a preview is under it, and the same `Intent::ConfirmRemove`
//! answers either — the preview travelling with the request is what tells
//! the host which Store operation to run.
//!
//! The controller's own modal prompts — *Delete from Koru?*, the D258 Cloud
//! question and the attach question — are drawn by the tab over these and
//! take input first; [`ManageView::no_question_up`] keeps a page dialog from
//! opening under one.
//!
//! **Enter is Cancel** on every confirmation here, as it was: each is the
//! shared `Confirm`, whose declared default is the safe answer (ADR-265).
//! Escape cancels.

use super::*;
use koru_i18n::Key as I18n;

#[cfg(test)]
mod tests;

/// One question over the tab.
pub(crate) enum Dialog {
    /// *Sign out…*: this computer leaves the account.
    SignOut(Confirm),
    /// *Unlink…* on a Google Account that is not the Profile's last.
    Unlink {
        profile: ProfileId,
        index: usize,
        confirm: Confirm,
    },
    /// *Linked devices*' *Unlink…* on a directly linked device.
    UnlinkDevice { device_id: String, confirm: Confirm },
    /// The controller's deletion or remove-from-this-device question.
    Remove(Confirm),
    /// The controller's offline-only question.
    Offline(Confirm),
    /// The controller's *Connect this Google Account?*.
    AddGoogle(Confirm),
}

/// Which controller question a dialog mirrors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mirror {
    Remove,
    Offline,
    AddGoogle,
}

impl Dialog {
    /// What the inspector calls it.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::SignOut(_) => "Manage sign-out dialog",
            Self::Unlink { .. } => "Manage unlink dialog",
            Self::UnlinkDevice { .. } => "Manage unlink device dialog",
            Self::Remove(_) => "Manage remove dialog",
            Self::Offline(_) => "Manage offline-only dialog",
            Self::AddGoogle(_) => "Manage add Google dialog",
        }
    }

    pub(crate) fn title(&self) -> &str {
        self.confirm().title()
    }

    /// The shared confirmation every question here is.
    fn confirm(&self) -> &Confirm {
        match self {
            Self::SignOut(confirm)
            | Self::Unlink { confirm, .. }
            | Self::UnlinkDevice { confirm, .. }
            | Self::Remove(confirm)
            | Self::Offline(confirm)
            | Self::AddGoogle(confirm) => confirm,
        }
    }

    fn confirm_mut(&mut self) -> &mut Confirm {
        match self {
            Self::SignOut(confirm)
            | Self::Unlink { confirm, .. }
            | Self::UnlinkDevice { confirm, .. }
            | Self::Remove(confirm)
            | Self::Offline(confirm)
            | Self::AddGoogle(confirm) => confirm,
        }
    }

    fn mirror(&self) -> Option<Mirror> {
        match self {
            Self::Remove(_) => Some(Mirror::Remove),
            Self::Offline(_) => Some(Mirror::Offline),
            Self::AddGoogle(_) => Some(Mirror::AddGoogle),
            _ => None,
        }
    }
}

const ADD_GOOGLE_BODY: &str = "This Google Account will be able to help connect new devices to this Profile. Your Profile content stays on your devices.";
const OFFLINE_BODY: &str = "This removes the last sign-in method. Local content and established trusted-device sync remain. Connect a Google Account again before adding a new device.";
const SIGN_OUT_TITLE: &str = "Sign out of Koru on this computer?";
const SIGN_OUT_BODY: &str = "Every signed-in profile on this computer stops syncing until you sign in again. Your profiles and their workspaces stay on this computer.";
const UNLINK_DEVICE_BODY: &str =
    "It will stop receiving updates immediately. Link it again later to restore access.";

impl ManageView {
    /// Whether nothing is being asked: no page dialog and no controller
    /// prompt. A second question never opens under the first.
    pub(super) fn no_question_up(&self) -> bool {
        self.dialog.is_none()
            && self.c.prompts().next().is_none()
            && !self.w.borrow().is_confirming()
    }

    fn profile_name(&self, profile: ProfileId) -> String {
        self.c
            .profiles()
            .iter()
            .find(|row| row.id == profile)
            .map_or_else(|| "this Profile".to_owned(), |row| row.name.clone())
    }

    /// *Delete profile and its workspaces…*: the shared, restorable batch.
    /// The controller reads the preview off this thread and raises its
    /// question over it, which [`Self::settle_dialogs`] then shows.
    pub(super) fn ask_to_delete(&mut self, profile: ProfileId) {
        if !self.no_question_up() || self.c.selected() != Some(profile) {
            return;
        }
        self.perform(Intent::DeleteProfile);
    }

    /// *Remove from this device…*: the device-local removal, with the
    /// Profile's workspaces when it has any. An unknown count takes the
    /// cascade, which is also right for an empty Profile
    /// (`selected_profile_removal`).
    pub(super) fn ask_to_remove_from_device(&mut self, profile: ProfileId) {
        if !self.no_question_up() || self.c.selected() != Some(profile) {
            return;
        }
        self.perform(Intent::RemoveFromDevice {
            cascade: self.c.selected_profile_removal().unwrap_or(true),
        });
    }

    /// *Delete from Koru…*: the account's deletion, the controller's own
    /// prompt. Offered only where it allows it (`removal::can_delete`) and
    /// in a build that shows Koru Cloud (koru-features).
    pub(super) fn ask_to_delete_from_koru(&mut self, profile: ProfileId) {
        if !self.no_question_up()
            || self.c.selected() != Some(profile)
            || !Self::offers_delete_from_koru(&self.c)
        {
            return;
        }
        self.perform(Intent::DeleteFromKoru(profile));
    }

    /// Whether the selected Profile's page offers *Delete from Koru…*.
    pub(super) fn offers_delete_from_koru(c: &ProfilesController) -> bool {
        koru_features::cloud_sync() && c.selected_profile_can_delete_from_koru()
    }

    /// *Sign out…* (maintainers' decision 1): the session is the device's, so the
    /// question says what that means for every Profile here.
    pub(super) fn ask_to_sign_out(&mut self) {
        if self.no_question_up() {
            self.dialog = Some(Dialog::SignOut(Confirm::new(
                SIGN_OUT_TITLE,
                SIGN_OUT_BODY,
                "Sign out",
            )));
        }
    }

    /// *Unlink…* on one Google Account. The last one is the controller's
    /// offline-only question, which it raises itself; any other is asked
    /// here first.
    pub(super) fn ask_to_unlink(&mut self, profile: ProfileId, index: usize) {
        if !self.no_question_up() {
            return;
        }
        let identities = self.c.identities_of(profile);
        let Some(identity) = identities.get(index) else {
            return;
        };
        if identities.len() == 1 {
            self.perform(Intent::Unlink(profile, index));
            return;
        }
        let email = identity
            .verified_email
            .clone()
            .unwrap_or_else(|| "this Google Account".into());
        let name = self.profile_name(profile);
        let confirm = Confirm::new(
            format!("Unlink {email}?"),
            format!(
                "It will no longer sign in to \u{201c}{name}\u{201d}. The profile keeps its other Google Accounts."
            ),
            "Unlink",
        );
        self.dialog = Some(Dialog::Unlink {
            profile,
            index,
            confirm,
        });
    }

    /// *Unlink…* on a linked device: the question the Link dialog asked, in
    /// the shared confirmation. A device that arrived through another has no
    /// *Unlink…* to press, and this computer is never in the list.
    pub(super) fn ask_to_unlink_device(&mut self, index: usize) {
        if !self.no_question_up() {
            return;
        }
        let Some(device) = self
            .devices
            .devices()
            .get(index)
            .filter(|device| device.introduced_by.is_none())
        else {
            return;
        };
        self.dialog = Some(Dialog::UnlinkDevice {
            device_id: device.device_id.clone(),
            confirm: Confirm::new(
                format!("Unlink {}?", device.display_name),
                UNLINK_DEVICE_BODY,
                "Unlink device",
            ),
        });
    }

    /// Keep a dialog up exactly while the controller is asking its question,
    /// and take it down when it stops. A page dialog already up stays; the
    /// controller's question follows it.
    pub(super) fn settle_dialogs(&mut self) {
        let wanted = if self.c.confirming_remove().is_some() {
            Some(Mirror::Remove)
        } else if self.c.confirming_offline() {
            Some(Mirror::Offline)
        } else if self
            .c
            .pending_add_google()
            .is_some_and(|flow| !flow.finishing)
        {
            Some(Mirror::AddGoogle)
        } else {
            None
        };
        let showing = self.dialog.as_ref().map(Dialog::mirror);
        match (showing, wanted) {
            (Some(Some(up)), Some(wanted)) if up == wanted => {}
            (Some(Some(_)), None) => self.dialog = None,
            (Some(None), _) | (None, None) => {}
            (Some(Some(_)) | None, Some(wanted)) => self.dialog = Some(self.mirrored(wanted)),
        }
    }

    fn mirrored(&self, mirror: Mirror) -> Dialog {
        let name = self
            .c
            .selected_profile()
            .map_or_else(|| "this Profile".to_owned(), |row| row.name.clone());
        match mirror {
            Mirror::Remove => {
                let lang = crate::format::lang();
                let t = |key: I18n| koru_i18n::t(lang, key);
                let hosted = self
                    .c
                    .selected_profile()
                    .is_some_and(|row| row.account.is_some());
                let (title, mut body, targets, label) =
                    if let Some(preview) = self.c.deletion_preview() {
                        // The shared batch (root-catalog §6.1), over the captured
                        // list: what it will trash, by name, from the Store's
                        // preview — never from a guess at the catalog.
                        let names: Vec<String> = {
                            let w = self.w.borrow();
                            preview
                                .targets
                                .iter()
                                .map(|id| {
                                    w.row(*id)
                                        .map(|row| row.name.clone())
                                        .unwrap_or_else(|| id.to_storage())
                                })
                                .collect()
                        };
                        let (title, targets) = match names.len() {
                            0 => (
                                I18n::ProfileDeleteEmptyTitle,
                                t(I18n::ProfileDeleteTargetsNone).to_owned(),
                            ),
                            1 => (
                                I18n::ProfileDeleteTitle,
                                koru_i18n::fill(t(I18n::ProfileDeleteTargetsOne), &[&names[0]]),
                            ),
                            n => (
                                I18n::ProfileDeleteTitle,
                                koru_i18n::fill(
                                    t(I18n::ProfileDeleteTargetsMany),
                                    &[
                                        &koru_i18n::number::decimal(lang, n as u64),
                                        &names.join(", "),
                                    ],
                                ),
                            ),
                        };
                        (
                            koru_i18n::fill(t(title), &[&name]),
                            t(I18n::ProfileDeleteBody).to_owned(),
                            Some(targets),
                            I18n::ProfileDeleteConfirm,
                        )
                    } else {
                        // Device-local: the Profile leaves this device's lists;
                        // its workspaces go to Trash when it has any.
                        let body = if self.c.confirming_remove() == Some(true) {
                            I18n::ProfileRemoveCascadeBody
                        } else {
                            I18n::ProfileRemoveEmptyBody
                        };
                        (
                            koru_i18n::fill(t(I18n::ProfileRemoveTitle), &[&name]),
                            t(body).to_owned(),
                            None,
                            I18n::ProfileRemoveConfirm,
                        )
                    };
                if hosted {
                    body = format!("{body} {}", t(I18n::ProfileRemoveHostedBody));
                }
                if let Some(targets) = targets {
                    body = format!("{body}\n\n{targets}");
                }
                Dialog::Remove(Confirm::localized(
                    title,
                    body,
                    t(label),
                    t(I18n::ProfileRemoveCancel),
                ))
            }
            Mirror::Offline => {
                let name = self.c.pending_unlink_name().map_or(name, str::to_owned);
                Dialog::Offline(Confirm::new(
                    format!("Make {name} offline-only?"),
                    OFFLINE_BODY,
                    "Make offline-only",
                ))
            }
            Mirror::AddGoogle => {
                let title = self
                    .c
                    .pending_add_google()
                    .map_or_else(String::new, |flow| {
                        format!("Connect {} to {}?", flow.verified_email, flow.profile_name)
                    });
                Dialog::AddGoogle(Confirm::asking(
                    title,
                    ADD_GOOGLE_BODY,
                    "Connect Google Account",
                    "Cancel",
                ))
            }
        }
    }

    // ---- input -------------------------------------------------------------

    /// A key while a dialog is up; every key is the dialog's.
    pub(super) fn dialog_key(&mut self, key: &Key) -> bool {
        let Some(mut dialog) = self.dialog.take() else {
            return false;
        };
        let outcome = dialog.confirm_mut().key(key);
        self.answer(dialog, outcome);
        true
    }

    pub(super) fn dialog_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let Some(mut dialog) = self.dialog.take() else {
            return false;
        };
        let viewport = self.viewport;
        let outcome = dialog
            .confirm_mut()
            .mouse_down(&mut self.engine, viewport, x, y);
        self.answer(dialog, outcome);
        true
    }

    pub(super) fn dialog_mouse_move(&mut self, x: f32, y: f32) -> bool {
        let viewport = self.viewport;
        match self.dialog.as_mut() {
            Some(dialog) => dialog
                .confirm_mut()
                .mouse_move(&mut self.engine, viewport, x, y),
            None => false,
        }
    }

    /// Settle a dialog taken out of its slot: put it back while it is still
    /// open, or perform what its answer means.
    fn answer(&mut self, dialog: Dialog, outcome: ConfirmOutcome) {
        if outcome == ConfirmOutcome::Keep {
            self.dialog = Some(dialog);
            return;
        }
        let confirmed = outcome == ConfirmOutcome::Confirm;
        // A device's unlink is Agent's, not the Profiles controller's.
        if let Dialog::UnlinkDevice { device_id, .. } = &dialog {
            if confirmed {
                self.requests
                    .push(Request::Device(DeviceRequest::Unlink(device_id.clone())));
            }
            return;
        }
        let intent = match dialog {
            Dialog::SignOut(_) if confirmed => Intent::SignOut,
            Dialog::Unlink { profile, index, .. } if confirmed => Intent::Unlink(profile, index),
            Dialog::SignOut(_) | Dialog::Unlink { .. } | Dialog::UnlinkDevice { .. } => return,
            Dialog::Remove(_) if confirmed => Intent::ConfirmRemove,
            Dialog::Remove(_) => Intent::CancelRemove,
            Dialog::Offline(_) if confirmed => Intent::ConfirmOffline,
            Dialog::Offline(_) => Intent::CancelOffline,
            Dialog::AddGoogle(_) if confirmed => Intent::ConfirmAddGoogle,
            Dialog::AddGoogle(_) => Intent::CancelAddGoogle,
        };
        self.perform(intent);
    }

    // ---- paint -------------------------------------------------------------

    pub(super) fn paint_dialog(&mut self, canvas: &mut Scene, root: Affine) {
        let (w, h) = self.viewport;
        let Self {
            dialog,
            engine,
            viewport,
            ..
        } = self;
        let Some(dialog) = dialog.as_ref() else {
            return;
        };
        let _dialog = inspect::scope(
            dialog.name(),
            "part",
            root,
            scene::rect(0.0, 0.0, w, h),
            |r| r.property("title", dialog.title(), "manage::dialogs"),
        );
        dialog.confirm().draw(canvas, root, engine, *viewport);
    }
}
