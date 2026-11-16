// SPDX-License-Identifier: MPL-2.0

//! Sync choices composed from the existing CardRow and action buttons.
use super::profile_page::{Control, Look, Placed, Row};
use super::*;
use koru_draw::geometry::Rect;
use koru_i18n::{Key, t};
use koru_store_core::{TransportMode, ids::WorkspaceId};
use koru_ui::{
    button::{ActionButton, ActionStyle},
    card::CardRow,
};

impl ManageView {
    pub(super) fn lay_sync_modes(
        &mut self,
        profile: &ProfileSummary,
        column: Rect,
        cy: &mut f32,
        rows: &mut Vec<Row>,
        controls: &mut Vec<Placed>,
    ) {
        let settings = self.c.sync_settings().cloned();
        let lang = crate::format::lang();
        let hint = self.c.sync_notice().map(str::to_owned).unwrap_or_else(|| {
            if settings.is_none() {
                t(lang, Key::SyncModeReading).into()
            } else if self.c.cloud_sync_availability(profile) != profiles::SyncAvailability::Enabled
            {
                t(lang, Key::SyncModePeerHint).into()
            } else {
                t(lang, Key::SyncModeCloudHint).into()
            }
        });
        self.lay_row(
            column,
            cy,
            rows,
            controls,
            t(lang, Key::SyncSettingsTab),
            Some(hint),
            None,
        );
        self.lay_sync_choices(
            column,
            cy,
            rows,
            controls,
            None,
            settings.as_ref().map(|s| s.mode.as_str()),
        );
        if self.c.cloud_status() == profiles::CloudStatus::Subscribe {
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
        if let Some(settings) = settings {
            for workspace in settings.workspaces {
                let Some(id) = WorkspaceId::parse(&workspace.id) else {
                    continue;
                };
                self.lay_row(
                    column,
                    cy,
                    rows,
                    controls,
                    &workspace.name,
                    Some(t(lang, Key::SyncModeCeiling).into()),
                    None,
                );
                self.lay_sync_choices(column, cy, rows, controls, Some(id), Some(&workspace.mode));
            }
        }
    }

    fn lay_sync_choices(
        &mut self,
        column: Rect,
        cy: &mut f32,
        rows: &mut Vec<Row>,
        controls: &mut Vec<Placed>,
        workspace: Option<WorkspaceId>,
        selected: Option<&str>,
    ) {
        let left = column.x + CardRow::PAD_X;
        let right = column.x + column.w - CardRow::PAD_X;
        let mut x = left;
        let mut y = *cy + CardRow::PAD_Y;
        for (mode, label) in [
            (
                TransportMode::LocalOnly,
                t(crate::format::lang(), Key::SyncModeOff),
            ),
            (TransportMode::P2p, "P2P"),
            (TransportMode::Cloud, "Cloud"),
        ] {
            let style = if selected == Some(mode.as_storage()) {
                ActionStyle::Primary
            } else {
                ActionStyle::Ghost
            };
            let width = ActionButton::new(label, style, None)
                .width(&mut self.engine)
                .min(right - left);
            if x > left && x + width > right {
                x = left;
                y += ActionButton::H + CardRow::PAD_Y;
            }
            let draw_x = if crate::format::lang().is_rtl() {
                right - (x - left) - width
            } else {
                x
            };
            controls.push(Placed {
                control: Control::SyncMode(workspace, mode),
                rect: Rect::new(draw_x, y, width, ActionButton::H),
                label: label.into(),
                look: Look::Action(style),
            });
            x += width + CardRow::PAD_Y;
        }
        let height = y + ActionButton::H + CardRow::PAD_Y - *cy;
        rows.push(Row {
            rect: Rect::new(column.x, *cy, column.w, height),
            label: None,
            hint: None,
            text_w: 0.0,
        });
        *cy += height;
    }
}
