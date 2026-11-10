// SPDX-License-Identifier: MPL-2.0

//! **`NeedsProfile` recovery** (root-catalog TECHNICAL.final §6.3): beside
//! *Create profile*, the restorable Profile deletions this device holds and
//! the live workspaces no Profile lists.
//!
//! The first-run window reaches this when the Profile catalog is empty — the
//! last usable Profile was deleted, here or on another device — so recovery
//! must not depend on an active Profile's menu. The host reads both lists
//! off the winit thread and pushes them in with [`OnboardingView::show_recovery`];
//! this screen draws them and raises one typed action, the batch restore.
//! Moving an unassigned workspace needs a destination, and `NeedsProfile`
//! has none, so the unassigned list is named and the one line under it says
//! what to do first. Nothing here creates a Profile or a workspace.

use super::*;

/// One restorable Profile deletion, as the host read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverableProfile {
    pub profile: koru_core::ProfileId,
    pub deletion: koru_store_core::ids::DeletionId,
    pub name: String,
    /// When the deletion was committed, epoch milliseconds.
    pub at_ms: i64,
    /// The workspaces the deletion captured.
    pub workspaces: usize,
    /// Of them, how many are past retention or purged: said, not recreated.
    pub unavailable: usize,
}

/// One live workspace no active Profile lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnassignedWorkspaceRow {
    pub workspace: koru_core::WorkspaceId,
    pub name: String,
}

const ROW_PITCH: f32 = 58.0;
const UNASSIGNED_PITCH: f32 = 28.0;
const HEADING_GAP: f32 = 30.0;

pub(super) struct RecoveryLayout {
    pub title: Rect,
    pub deleted_heading: Rect,
    /// Per deleted Profile: its row, and its *Restore* button.
    pub deleted: Vec<(Rect, Rect)>,
    /// The sentence shown instead of rows when none can be restored.
    pub none: Option<Rect>,
    pub unassigned_heading: Option<Rect>,
    pub unassigned: Vec<Rect>,
    pub note: Option<Rect>,
    pub error: Option<Rect>,
    pub back: Rect,
}

impl OnboardingView {
    /// The host read the recovery lists. Offered from the Profile form only
    /// while there is something in them; an empty answer offers nothing.
    pub fn show_recovery(
        &mut self,
        deleted: Vec<RecoverableProfile>,
        unassigned: Vec<UnassignedWorkspaceRow>,
    ) {
        self.recovery_deleted = deleted;
        self.recovery_unassigned = unassigned;
        if self.screen == Screen::Recovery && !self.recovery_offered() {
            self.show_profile();
        }
    }

    /// A restore refused or did not answer: back on this screen, with the
    /// sentence, and nothing changed.
    pub fn recovery_failed(&mut self, message: impl Into<String>) {
        self.screen = Screen::Recovery;
        self.focus = 0;
        self.error = Some(message.into());
    }

    /// Whether the Profile form offers the way to this screen: never in an
    /// embedded form (Manage has its own recovery sections), and only while
    /// there is something to recover.
    pub(super) fn recovery_offered(&self) -> bool {
        !self.embedded && !(self.recovery_deleted.is_empty() && self.recovery_unassigned.is_empty())
    }

    pub(super) fn recovery_deleted(&self) -> &[RecoverableProfile] {
        &self.recovery_deleted
    }

    pub(super) fn recovery_unassigned(&self) -> &[UnassignedWorkspaceRow] {
        &self.recovery_unassigned
    }

    /// The link on the Profile form, beside *Back* (or where *Back* would
    /// be when the form has none).
    pub(super) fn recovery_link_rect(&mut self) -> Rect {
        let (primary, back) = self.form_action_buttons("Continue");
        let label = koru_i18n::t(self.language, koru_i18n::Key::OnboardingRecoveryLink);
        let w = ActionButton::new(label, ActionStyle::Ghost, None).width(&mut self.engine);
        let x = if Self::profile_has_back() {
            back.x + back.w + BUTTON_GAP
        } else {
            back.x
        };
        Rect::new(x, primary.y, w, primary.h)
    }

    pub(super) fn recovery_controls(&self) -> Vec<Hit> {
        let mut controls: Vec<Hit> = (0..self.recovery_deleted.len())
            .map(Hit::RestoreDeleted)
            .collect();
        controls.push(Hit::Back);
        controls
    }

    pub(super) fn open_recovery(&mut self) {
        self.profile_appearance = None;
        self.screen = Screen::Recovery;
        self.focus = 0;
        self.error = None;
    }

    pub(super) fn activate_recovery(&mut self, hit: Hit) {
        match hit {
            Hit::Back => self.show_profile(),
            Hit::RestoreDeleted(index) => {
                let Some(row) = self.recovery_deleted.get(index) else {
                    return;
                };
                self.pending = Some(Action::RestoreDeletedProfile {
                    profile: row.profile,
                    deletion: row.deletion,
                });
                self.error = None;
                // The restore is the host's; while it runs the form shows the
                // same wait the hosted restore does, and offers nothing.
                self.screen = Screen::RestoringProfile;
                self.focus = 0;
            }
            _ => {}
        }
    }

    pub(super) fn recovery_layout(&mut self) -> RecoveryLayout {
        let c = self.content();
        let lang = self.language;
        let meta = theme::role::meta();
        let meta_line = meta.metrics.line_height;
        let title = Rect::new(
            c.x,
            c.y + 24.0,
            c.w,
            theme::role::subtitle().metrics.line_height,
        );
        let mut y = title.y + title.h + HEADING_GAP;
        let deleted_heading = Rect::new(c.x, y, c.w, meta_line);
        y += meta_line + 8.0;
        let restore_label = koru_i18n::t(lang, koru_i18n::Key::WorkspacesRestoreDeleted);
        let restore_w =
            ActionButton::new(restore_label, ActionStyle::Secondary, None).width(&mut self.engine);
        let mut deleted = Vec::new();
        for _ in &self.recovery_deleted {
            let row = Rect::new(c.x, y, c.w, ROW_PITCH);
            let button = Rect::new(
                c.x + c.w - restore_w,
                y + (ROW_PITCH - ActionButton::H) * 0.5,
                restore_w,
                ActionButton::H,
            );
            deleted.push((row, button));
            y += ROW_PITCH;
        }
        let none = self.recovery_deleted.is_empty().then(|| {
            let rect = Rect::new(c.x, y, c.w, meta_line);
            y += meta_line;
            rect
        });
        y += HEADING_GAP;
        let (unassigned_heading, unassigned, note) = if self.recovery_unassigned.is_empty() {
            (None, Vec::new(), None)
        } else {
            let heading = Rect::new(c.x, y, c.w, meta_line);
            y += meta_line + 8.0;
            let rows: Vec<Rect> = (0..self.recovery_unassigned.len())
                .map(|i| Rect::new(c.x, y + i as f32 * UNASSIGNED_PITCH, c.w, UNASSIGNED_PITCH))
                .collect();
            y += rows.len() as f32 * UNASSIGNED_PITCH + 8.0;
            let text = koru_i18n::t(lang, koru_i18n::Key::OnboardingUnassignedNeedsProfile);
            let h =
                koru_ui::text_paragraph::KoruTextParagraph::new(text, meta, "", theme::p().text)
                    .measure(&mut self.engine, c.w);
            let note = Rect::new(c.x, y, c.w, h);
            y += h;
            (Some(heading), rows, Some(note))
        };
        let error = self.error.clone().map(|message| {
            y += 12.0;
            let h = koru_ui::text_paragraph::KoruTextParagraph::new(
                &message,
                meta,
                "",
                theme::p().text,
            )
            .measure(&mut self.engine, c.w);
            let rect = Rect::new(c.x, y, c.w, h);
            y += h;
            rect
        });
        let _ = y;
        let back_w = ActionButton::new("Back", ActionStyle::Ghost, None).width(&mut self.engine);
        let back = Rect::new(c.x, c.y + c.h - 64.0, back_w, 42.0);
        RecoveryLayout {
            title,
            deleted_heading,
            deleted,
            none,
            unassigned_heading,
            unassigned,
            note,
            error,
            back,
        }
    }

    pub(super) fn recovery_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        let layout = self.recovery_layout();
        if layout.back.contains(x, y) {
            return Some(Hit::Back);
        }
        layout
            .deleted
            .iter()
            .position(|(_, button)| button.contains(x, y))
            .map(Hit::RestoreDeleted)
    }

    /// A deleted Profile's line: when, how many workspaces, and how many of
    /// them are gone.
    pub(super) fn recovery_deleted_line(&self, row: &RecoverableProfile) -> String {
        use koru_i18n::{Key as K, fill, number::decimal, t};
        let lang = self.language;
        let when = local_moment(lang, row.at_ms);
        let mut line = match row.workspaces {
            1 => fill(t(lang, K::WorkspacesDeletedProfileLineOne), &[&when]),
            n => fill(
                t(lang, K::WorkspacesDeletedProfileLineMany),
                &[&when, &decimal(lang, n as u64)],
            ),
        };
        match row.unavailable {
            0 => {}
            1 => {
                line.push_str(" · ");
                line.push_str(t(lang, K::WorkspacesDeletedProfileExpiredOne));
            }
            n => {
                line.push_str(" · ");
                line.push_str(&fill(
                    t(lang, K::WorkspacesDeletedProfileExpiredMany),
                    &[&decimal(lang, n as u64)],
                ));
            }
        }
        line
    }

    pub(super) fn draw_recovery(&mut self, canvas: &mut Scene, root: Affine) {
        self.draw_recovery_screen(canvas, root);
    }
}

/// The deletion's local moment, dated the reader's way; the year only when
/// it is not this one. Empty for an instant chrono cannot represent.
fn local_moment(lang: koru_i18n::Lang, ms: i64) -> String {
    use chrono::{Datelike, Local, Timelike};
    let Some(moment) = chrono::DateTime::from_timestamp_millis(ms) else {
        return String::new();
    };
    let moment = moment.with_timezone(&Local);
    let parts = koru_i18n::time::DateTimeParts {
        year: moment.year(),
        month: moment.month() as u8,
        day: moment.day() as u8,
        hour: moment.hour() as u8,
        minute: moment.minute() as u8,
        second: moment.second() as u8,
    };
    koru_i18n::time::absolute(lang, parts, parts.year != Local::now().year())
}

impl OnboardingView {
    fn draw_recovery_screen(&mut self, canvas: &mut Scene, root: Affine) {
        use koru_i18n::{Key as K, t};
        let p = theme::p();
        let lang = self.language;
        let layout = self.recovery_layout();
        let controls = self.controls();
        let focused = controls.get(self.focus).copied();
        draw_text(
            &mut self.engine,
            canvas,
            root,
            t(lang, K::OnboardingRecoveryTitle),
            layout.title.x,
            layout.title.y,
            theme::role::subtitle(),
            "theme::role::subtitle()",
            p.text,
        );
        draw_text(
            &mut self.engine,
            canvas,
            root,
            t(lang, K::OnboardingRestoreDeleted),
            layout.deleted_heading.x,
            layout.deleted_heading.y,
            theme::role::meta(),
            "theme::role::meta()",
            p.text_dim,
        );
        let deleted = self.recovery_deleted.clone();
        for (index, (row, (rect, button))) in deleted.iter().zip(&layout.deleted).enumerate() {
            let _row = inspect::scope(
                "RecoverableProfileRow",
                "part",
                root,
                scene::rect(rect.x, rect.y, rect.w, rect.h),
                |record| record.property("profile", row.profile.to_storage(), "Store catalog"),
            );
            let name_style = theme::role::row();
            let room = (button.x - rect.x - 12.0).max(1.0);
            let name = self.engine.elided(&row.name, name_style, room);
            draw_text(
                &mut self.engine,
                canvas,
                root,
                &name,
                rect.x,
                rect.y + 6.0,
                name_style,
                "theme::role::row()",
                p.text,
            );
            let line = self.recovery_deleted_line(row);
            let line = self.engine.elided(&line, theme::role::meta(), room);
            draw_text(
                &mut self.engine,
                canvas,
                root,
                &line,
                rect.x,
                rect.y + 6.0 + name_style.metrics.line_height,
                theme::role::meta(),
                "theme::role::meta()",
                p.text_dim,
            );
            ActionButton::new(
                t(lang, K::WorkspacesRestoreDeleted),
                ActionStyle::Secondary,
                None,
            )
            .draw_focused(
                canvas,
                root,
                &mut self.engine,
                *button,
                self.hover == Some(Hit::RestoreDeleted(index)),
                focused == Some(Hit::RestoreDeleted(index)),
            );
        }
        if let Some(none) = layout.none {
            draw_text(
                &mut self.engine,
                canvas,
                root,
                t(lang, K::OnboardingNoDeletedProfiles),
                none.x,
                none.y,
                theme::role::meta(),
                "theme::role::meta()",
                p.text_dim,
            );
        }
        if let Some(heading) = layout.unassigned_heading {
            draw_text(
                &mut self.engine,
                canvas,
                root,
                t(lang, K::OnboardingUnassignedWorkspaces),
                heading.x,
                heading.y,
                theme::role::meta(),
                "theme::role::meta()",
                p.text_dim,
            );
        }
        let unassigned = self.recovery_unassigned.clone();
        for (row, rect) in unassigned.iter().zip(&layout.unassigned) {
            let name = self.engine.elided(&row.name, theme::role::row(), rect.w);
            draw_text(
                &mut self.engine,
                canvas,
                root,
                &name,
                rect.x,
                rect.y + 4.0,
                theme::role::row(),
                "theme::role::row()",
                p.text,
            );
        }
        if let Some(note) = layout.note {
            draw_wrapped_text(
                &mut self.engine,
                canvas,
                root,
                t(lang, K::OnboardingUnassignedNeedsProfile),
                note.x,
                note.y,
                note.w,
                theme::role::meta(),
                "theme::role::meta()",
                p.text,
            );
        }
        if let (Some(error), Some(message)) = (layout.error, self.error.clone()) {
            draw_wrapped_text(
                &mut self.engine,
                canvas,
                root,
                &message,
                error.x,
                error.y,
                error.w,
                theme::role::meta(),
                "theme::role::meta()",
                p.notice_fg,
            );
        }
        ActionButton::new("Back", ActionStyle::Ghost, None).draw_focused(
            canvas,
            root,
            &mut self.engine,
            layout.back,
            self.hover == Some(Hit::Back),
            focused == Some(Hit::Back),
        );
    }
}
