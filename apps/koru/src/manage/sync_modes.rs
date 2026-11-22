// SPDX-License-Identifier: MPL-2.0

//! The sync choice — *Sync off*, *P2P*, *Cloud* — as one strip of three text
//! buttons, the chosen one filled. The Profile's strip sits at the right end
//! of its page's head; each Workspace's sits in its row. Both are laid by
//! [`ManageView::lay_sync_strip`], so they read and press alike, and both
//! perform the choice they always did (`ProfilesController::choose_sync_mode`).
use super::profile_page::{Control, Look, Placed, Row};
use super::*;
use koru_draw::geometry::Rect;
use koru_i18n::{Key, t};
use koru_store_core::{TransportMode, ids::WorkspaceId};
use koru_ui::button::{ActionButton, ActionStyle};

/// Between two choices of one strip.
const CHOICE_GAP: f32 = 4.0;

/// Under the Profile's strip: what a choice there does to the Workspaces
/// below it. A consequence the person needs before pressing, so it is a line
/// of text and not a tooltip.
pub(super) const APPLIES_TO_ALL: &str = "Applies to all workspaces";
/// The link beside it, to the page where Koru Cloud itself is set up.
pub(super) const CLOUD_SETTINGS: &str = "Koru Cloud settings";

impl ManageView {
    /// The three choices in reading order. *Sync off* is a sentence of the
    /// product and is localized; *P2P* and *Cloud* are names.
    fn sync_choices() -> [(TransportMode, &'static str); 3] {
        [
            (
                TransportMode::LocalOnly,
                t(crate::format::lang(), Key::SyncModeOff),
            ),
            (TransportMode::P2p, "P2P"),
            (TransportMode::Cloud, "Cloud"),
        ]
    }

    fn choice_style(mode: TransportMode, selected: Option<&str>) -> ActionStyle {
        if selected == Some(mode.as_storage()) {
            ActionStyle::Primary
        } else {
            ActionStyle::Ghost
        }
    }

    /// How wide a strip showing `selected` is.
    pub(super) fn sync_strip_width(&mut self, selected: Option<&str>) -> f32 {
        let mut width = 0.0;
        for (index, (mode, label)) in Self::sync_choices().into_iter().enumerate() {
            if index > 0 {
                width += CHOICE_GAP;
            }
            width += ActionButton::new(label, Self::choice_style(mode, selected), None)
                .width(&mut self.engine);
        }
        width
    }

    /// Lay one strip with its right edge at `right` and its top at `y`, and
    /// push its three controls in reading order (so mirrored inside the strip
    /// for a right-to-left language). `workspace` is whose choice it is;
    /// `None` is the Profile's. Answers the strip's rect.
    pub(super) fn lay_sync_strip(
        &mut self,
        right: f32,
        y: f32,
        workspace: Option<WorkspaceId>,
        selected: Option<&str>,
        controls: &mut Vec<Placed>,
    ) -> Rect {
        let width = self.sync_strip_width(selected);
        let left = right - width;
        let rtl = crate::format::lang().is_rtl();
        let mut offset = 0.0;
        for (mode, label) in Self::sync_choices() {
            let style = Self::choice_style(mode, selected);
            let w = ActionButton::new(label, style, None).width(&mut self.engine);
            let x = if rtl {
                right - offset - w
            } else {
                left + offset
            };
            controls.push(Placed {
                control: Control::SyncMode(workspace, mode),
                rect: Rect::new(x, y, w, ActionButton::H),
                label: label.into(),
                look: Look::Action(style),
            });
            offset += w + CHOICE_GAP;
        }
        Rect::new(left, y, width, ActionButton::H)
    }

    /// The line under the Profile's strip: what is happening to the settings
    /// while something is (reading, saving, a failure), else what a choice
    /// there does.
    pub(super) fn sync_strip_hint(&self) -> String {
        match self.c.sync_notice() {
            Some(notice) => notice.to_owned(),
            None if self.c.sync_settings().is_none() => {
                t(crate::format::lang(), Key::SyncModeReading).into()
            }
            None => APPLIES_TO_ALL.into(),
        }
    }

    /// *Get Koru Cloud…* in *Sign-in and sync*, for an account that has no
    /// subscription: the way to the page that sells it stays where it was.
    pub(super) fn lay_get_cloud(
        &mut self,
        column: Rect,
        cy: &mut f32,
        rows: &mut Vec<Row>,
        controls: &mut Vec<Placed>,
    ) {
        if self.c.cloud_status() != profiles::CloudStatus::Subscribe {
            return;
        }
        let label = "Get Koru Cloud…";
        let width = koru_ui::button::button_width(&mut self.engine, label);
        self.lay_row(
            column,
            cy,
            rows,
            controls,
            label,
            None,
            Some((
                Control::GetCloud,
                label.into(),
                Look::Button(koru_ui::button::ButtonRole::Secondary),
                width,
                koru_ui::button::H,
            )),
        );
    }
}
