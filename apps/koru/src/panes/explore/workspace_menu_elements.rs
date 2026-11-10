// SPDX-License-Identifier: MPL-2.0

//! Workspace menu chrome, composed beside the shared workspace rows.

use super::popover_elements::{self, TileLeaf, WORKSPACE_ROW_STATES};
use super::{ExploreView, Outcome};
use koru_draw::el::{Action, El, Key, Role, col, divider, icon, leaf, row, text};
use koru_draw::theme::{IconSize, Ink, Line, Radius, Space, alias, metrics, roles};
use koru_i18n::{Key as Words, t};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    Sync,
    Cloud,
    New,
    Import,
    Trash,
    Retry,
}

impl MenuAction {
    pub(crate) fn key(self) -> Key {
        Key::surface(
            popover_elements::SURFACE,
            match self {
                Self::Sync => "workspace-sync",
                Self::Cloud => "workspace-cloud",
                Self::New => "new-workspace",
                Self::Import => "workspace-import",
                Self::Trash => "workspace-trash",
                Self::Retry => "workspace-retry",
            },
        )
    }

    pub(crate) fn words(self) -> Words {
        match self {
            Self::Sync => Words::SyncChannelDevices,
            Self::Cloud => Words::CloudTitle,
            Self::New => Words::TerminalNewWorkspace,
            Self::Import => Words::WorkspacesImport,
            Self::Trash => Words::WorkspacesTrash,
            Self::Retry => Words::TryAgain,
        }
    }

    pub(crate) fn outcome(self) -> Outcome {
        match self {
            Self::Sync => Outcome::OpenSyncSettings,
            Self::Cloud => Outcome::ManageCloud,
            Self::New => Outcome::NewWorkspace,
            Self::Import => Outcome::ImportContent,
            Self::Trash => Outcome::OpenTrash,
            Self::Retry => Outcome::RetryWorkspaceOperation,
        }
    }

    pub(crate) fn from_key(key: Key) -> Option<Self> {
        [
            Self::Sync,
            Self::Cloud,
            Self::New,
            Self::Import,
            Self::Trash,
            Self::Retry,
        ]
        .into_iter()
        .find(|action| action.key() == key)
    }
}

pub(crate) const MANAGE: Key = Key::surface(popover_elements::SURFACE, "manage-workspaces");
pub(crate) const NOTICE: Key = Key::surface(popover_elements::SURFACE, "workspace-notice");
pub(crate) const HEADER: Key = Key::surface(popover_elements::SURFACE, "workspace-header");
/// The empty Profile's one line (root-catalog §3.10).
pub(crate) const EMPTY: Key = Key::surface(popover_elements::SURFACE, "workspaces-empty");
/// The Unassigned group's heading (§3.6), and the parts of its rows: a row
/// is keyed by its workspace, like every other workspace row.
pub(crate) const UNASSIGNED_HEADING: Key =
    Key::surface(popover_elements::SURFACE, "unassigned-heading");
pub(crate) const UNASSIGNED: &str = "unassigned";
pub(crate) const UNASSIGNED_MOVE: &str = "unassigned-move";
pub(crate) const UNASSIGNED_RESTORE: &str = "unassigned-restore";

pub(crate) fn unassigned(id: koru_core::WorkspaceId) -> Key {
    Key::model(id.as_uuid().as_u128(), UNASSIGNED)
}

pub(crate) fn unassigned_move(id: koru_core::WorkspaceId) -> Key {
    Key::model(id.as_uuid().as_u128(), UNASSIGNED_MOVE)
}

pub(crate) fn unassigned_restore(id: koru_core::WorkspaceId) -> Key {
    Key::model(id.as_uuid().as_u128(), UNASSIGNED_RESTORE)
}

/// Configured channels, independent of their current connection health.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyncChannels {
    pub devices: bool,
    pub cloud: bool,
}

impl SyncChannels {
    pub(crate) fn controls(self) -> Vec<(MenuAction, char)> {
        use koru_draw::icons;
        match (self.devices, self.cloud) {
            (true, true) => vec![
                (MenuAction::Sync, icons::DEVICES),
                (MenuAction::Cloud, icons::CLOUD),
            ],
            (true, false) => vec![(MenuAction::Sync, icons::DEVICES)],
            (false, true) => vec![(MenuAction::Cloud, icons::CLOUD)],
            (false, false) => vec![(MenuAction::Sync, icons::DESKTOP)],
        }
    }

    fn words(self, lang: koru_i18n::Lang) -> String {
        let mut channels = Vec::new();
        if self.devices {
            channels.push(t(lang, Words::SyncChannelDevices));
        }
        if self.cloud {
            channels.push(t(lang, Words::CloudTitle));
        }
        if channels.is_empty() {
            channels.push(t(lang, Words::AccountSwitcherLocalOnly));
        }
        channels.join(", ")
    }
}

pub(crate) fn rule() -> El {
    col("Menu divider")
        .pad_y(Space::S1)
        .child(divider(Line::Divider))
}

impl ExploreView {
    pub(crate) fn sync_channels(&self) -> SyncChannels {
        SyncChannels {
            devices: self.linked_count > 0,
            cloud: self.current_profile.is_some_and(|id| {
                self.account.as_ref().is_some_and(|account| {
                    account
                        .cloud_profiles
                        .iter()
                        .any(|row| row.profile_id == id.to_string())
                })
            }),
        }
    }

    pub(crate) fn sync_control_name(&self) -> String {
        let lang = crate::format::lang();
        let status = self.sync_status();
        // An organization reading carries its sentence for a reader: what
        // it means and, for a stop, where to change it (Plan 2.2).
        let status_words = match status.hint() {
            Some(hint) => format!("{}. {}", t(lang, status.key()), t(lang, hint)),
            None => t(lang, status.key()).to_owned(),
        };
        koru_i18n::fill(
            t(lang, Words::SyncControlDescription),
            &[
                self.profile()
                    .map_or(t(lang, Words::WorkspacesProfiles), |p| p.name.as_str()),
                &status_words,
                &self.sync_channels().words(lang),
            ],
        )
    }

    pub(crate) fn sync_channel_controls(&self) -> El {
        row("Sync channels").gap(Space::S1).children(
            self.sync_channels()
                .controls()
                .into_iter()
                .map(|(action, glyph)| {
                    row("Sync channel")
                        .id(action.key())
                        .hit()
                        .focusable()
                        .action(Action::Click)
                        .action(Action::Focus)
                        .semantics(Role::MenuItem)
                        .name(action.words())
                        .min_h(metrics::ROW_MIN)
                        .pad_x(Space::S1)
                        .radius(Radius::M)
                        .states(&WORKSPACE_ROW_STATES)
                        .child(icon(glyph, IconSize::L).decorative().ink(Ink::Secondary))
                }),
        )
    }
    pub(crate) fn workspace_import_anchor(
        &self,
        area: koru_draw::geometry::Rect,
        viewport: (f32, f32),
    ) -> Option<(f32, f32)> {
        if self.live()?.kind != super::PopoverKind::Summary {
            return None;
        }
        let panel = self.settled_panel(area, viewport)?;
        self.with_page(area, viewport, |frame| {
            frame
                .node(MenuAction::Import.key())
                .map(|node| (panel.x + panel.w + super::POPOVER_GAP, node.rect.y))
        })
        .flatten()
    }
    pub(crate) fn set_workspace_notice(&mut self, notice: Option<String>) -> bool {
        if self.workspace_notice == notice {
            return false;
        }
        self.workspace_notice = notice;
        true
    }

    pub(crate) fn set_workspace_waiting(&mut self, waiting: Option<String>) -> bool {
        if self.workspace_waiting == waiting {
            return false;
        }
        self.workspace_waiting = waiting;
        if self.workspace_waiting.is_some() {
            self.empty_cta = None;
            self.empty_subscribe = None;
        }
        true
    }

    /// **The Unassigned group** (root-catalog §3.6), at the top of the menu
    /// and only while there is something in it: a heading, then per
    /// workspace its row (the existing row chrome; it opens Manage on the
    /// workspace) and, indented under it, *Move to…* and — for a workspace
    /// from a deleted Profile this device can restore — *Restore Profile*.
    pub(crate) fn unassigned_group(&self) -> Option<El> {
        let rows = self.unassigned_rows();
        if rows.is_empty() {
            return None;
        }
        let lang = crate::format::lang();
        let mut group = col("Unassigned group").child(
            row("Unassigned heading")
                .pad_x(alias::NAV_PAD)
                .min_h(metrics::ROW_MIN)
                .child(
                    text(t(lang, Words::WorkspacesUnassignedHeading).to_uppercase())
                        .id(UNASSIGNED_HEADING)
                        .semantics(Role::Heading)
                        .name(Words::WorkspacesUnassignedHeading)
                        .role(roles::POPOVER_HEADING)
                        .ink(Ink::Hint)
                        .fill(),
                ),
        );
        for entry in &rows {
            let id = crate::manage::workspaces::app_workspace(entry.id);
            let hint = entry.hint_key();
            group = group.child(
                row("Unassigned row")
                    .id(unassigned(id))
                    .min_h(metrics::ROW_MIN)
                    .pad_x(alias::NAV_PAD)
                    .gap(Space::S2)
                    .hit()
                    .focusable()
                    .action(Action::Click)
                    .action(Action::Focus)
                    .semantics(Role::MenuItem)
                    .name_text(format!("{}, {}", entry.name, t(lang, hint)))
                    .radius(Radius::M)
                    .states(&WORKSPACE_ROW_STATES)
                    .child(
                        leaf(
                            "Unassigned tile",
                            TileLeaf {
                                name: entry.name.clone(),
                                icon: entry.icon.clone(),
                                color: entry.color.clone(),
                            },
                        )
                        .decorative(),
                    )
                    .child(text(&entry.name).role(roles::ROW).fill().truncate()),
            );
            let action_row = |key: Key, glyph: char, words: Words| {
                row("Unassigned action")
                    .id(key)
                    .min_h(metrics::ROW_MIN)
                    .pad_x(alias::NAV_PAD)
                    .gap(Space::S2)
                    .hit()
                    .focusable()
                    .action(Action::Click)
                    .action(Action::Focus)
                    .semantics(Role::MenuItem)
                    .name_text(format!("{}: {}", t(lang, words), entry.name))
                    .radius(Radius::M)
                    .states(&WORKSPACE_ROW_STATES)
                    .child(icon(glyph, IconSize::L).decorative().ink(Ink::Secondary))
                    .child(
                        text(t(lang, words))
                            .role(roles::ROW)
                            .ink(Ink::Secondary)
                            .fill(),
                    )
            };
            group = group.child(action_row(
                unassigned_move(id),
                koru_draw::icons::ARROW_RIGHT,
                Words::WorkspacesMoveTo,
            ));
            if self.unassigned_restorable(entry).is_some() {
                group = group.child(action_row(
                    unassigned_restore(id),
                    koru_draw::icons::ARROWS_CLOCKWISE,
                    Words::WorkspacesRestoreProfile,
                ));
            }
        }
        Some(group.child(rule()))
    }

    /// Where the window's workspace menu opens for an Unassigned row's
    /// *Move to…*: beside the page, level with the row — as the import
    /// menu anchors.
    pub(crate) fn unassigned_move_anchor(
        &self,
        area: koru_draw::geometry::Rect,
        viewport: (f32, f32),
        id: koru_core::WorkspaceId,
    ) -> Option<(f32, f32)> {
        if self.live()?.kind != super::PopoverKind::Summary {
            return None;
        }
        let panel = self.settled_panel(area, viewport)?;
        self.with_page(area, viewport, |frame| {
            frame
                .node(unassigned_move(id))
                .map(|node| (panel.x + panel.w + super::POPOVER_GAP, node.rect.y))
        })
        .flatten()
    }

    /// The empty Profile's line (§3.10), where its rows would be.
    pub(crate) fn workspaces_empty_line(&self) -> El {
        let lang = crate::format::lang();
        text(t(lang, Words::ProfileEmptyWorkspaces))
            .id(EMPTY)
            .semantics(Role::Group)
            .name(Words::ProfileEmptyWorkspaces)
            .role(roles::HINT)
            .ink(Ink::Secondary)
            .pad_x(alias::NAV_PAD)
            .min_h(metrics::ROW_MIN)
            .wrap()
    }

    pub(crate) fn workspace_menu_header(&self) -> El {
        let lang = crate::format::lang();
        let profile = self.profile();
        let name = profile.map_or(t(lang, Words::WorkspacesProfiles), |profile| {
            profile.name.as_str()
        });
        row("Workspace menu profile")
            .id(HEADER)
            .min_h(metrics::PROFILE_ROW_MIN)
            .pad_x(alias::NAV_PAD)
            .gap(Space::S2)
            .child(
                leaf(
                    "Workspace menu profile tile",
                    TileLeaf {
                        name: name.to_owned(),
                        icon: profile.and_then(|p| p.icon.clone()),
                        color: profile.and_then(|p| p.color.clone()),
                    },
                )
                .decorative(),
            )
            .child(
                col("Workspace menu profile words")
                    .fill()
                    .gap(Space::S0_5)
                    .child(text(name).role(roles::ROW).truncate())
                    .child(
                        text(profile.map_or_else(
                            || t(lang, Words::OnThisComputer).to_owned(),
                            |p| self.profile_detail(p, lang),
                        ))
                        .role(roles::HINT)
                        .ink(Ink::Secondary)
                        .truncate(),
                    ),
            )
            .child(self.sync_channel_controls())
    }

    pub(crate) fn workspace_menu_heading(&self) -> El {
        let lang = crate::format::lang();
        row("Workspaces heading")
            .id(popover_elements::HEADING)
            .pad_x(alias::NAV_PAD)
            .min_h(metrics::ROW_MIN)
            .gap(Space::S2)
            .child(
                text(t(lang, Words::Workspaces).to_uppercase())
                    .id(Key::surface(popover_elements::SURFACE, "workspaces-title"))
                    .semantics(Role::Heading)
                    .name(Words::Workspaces)
                    .role(roles::POPOVER_HEADING)
                    .ink(Ink::Hint)
                    .fill(),
            )
            .child(
                text(t(lang, Words::ManageWorkspaces))
                    .role(roles::HINT)
                    .ink(Ink::Secondary)
                    .id(MANAGE)
                    .semantics(Role::MenuItem)
                    .name(Words::ManageWorkspaces)
                    .hit()
                    .focusable()
                    .action(Action::Click)
                    .action(Action::Focus)
                    .pad_x(Space::S1)
                    .radius(Radius::M)
                    .states(&WORKSPACE_ROW_STATES),
            )
    }

    pub(crate) fn workspace_menu_action(&self, action: MenuAction) -> El {
        let lang = crate::format::lang();
        let glyph = match action {
            MenuAction::New => koru_draw::icons::PLUS,
            MenuAction::Import => koru_draw::icons::IMPORT,
            MenuAction::Trash => koru_draw::icons::TRASH,
            MenuAction::Sync => koru_draw::icons::ARROWS_CLOCKWISE,
            MenuAction::Cloud => koru_draw::icons::CLOUD,
            MenuAction::Retry => koru_draw::icons::ARROWS_CLOCKWISE,
        };
        let mut item = row("Workspace menu action")
            .id(action.key())
            .min_h(metrics::ROW_MIN)
            .pad_x(alias::NAV_PAD)
            .gap(Space::S2)
            .hit()
            .focusable()
            .action(Action::Click)
            .action(Action::Focus)
            .semantics(Role::MenuItem)
            .name(action.words())
            .radius(Radius::M)
            .states(&WORKSPACE_ROW_STATES)
            .child(icon(glyph, IconSize::L).decorative().ink(Ink::Secondary))
            .child(text(t(lang, action.words())).role(roles::ROW).fill());
        if action == MenuAction::Trash {
            item = item.child(
                text(self.trash_count.to_string())
                    .role(roles::HINT)
                    .ink(Ink::Hint),
            );
        } else if action == MenuAction::Import {
            item = item.child(
                icon(koru_draw::icons::CARET_RIGHT, IconSize::L)
                    .decorative()
                    .ink(Ink::Hint),
            );
        }
        item
    }

    pub(crate) fn workspace_menu_footer(&self) -> El {
        let mut foot = col("Workspace menu footer")
            .child(rule())
            .child(self.workspace_menu_action(MenuAction::New))
            .child(self.workspace_menu_action(MenuAction::Import))
            .child(rule())
            .child(self.workspace_menu_action(MenuAction::Trash));
        if let Some(notice) = self
            .workspace_waiting
            .as_ref()
            .or(self.workspace_notice.as_ref())
        {
            foot = foot.child(
                text(notice)
                    .id(NOTICE)
                    .semantics(Role::Group)
                    .name_text(notice)
                    .role(roles::HINT)
                    .ink(Ink::Secondary)
                    .pad_x(alias::NAV_PAD)
                    .min_h(metrics::ROW_MIN)
                    .wrap(),
            );
        }
        if self.workspace_retryable() {
            foot = foot.child(self.workspace_menu_action(MenuAction::Retry));
        }
        foot
    }

    pub(crate) fn workspace_retryable(&self) -> bool {
        self.workspace_controller
            .as_ref()
            .is_some_and(|controller| {
                let controller = controller.borrow();
                self.current_profile
                    .and_then(|id| koru_store_core::ids::ProfileId::parse(&id.to_storage()))
                    .and_then(|id| controller.notice_for(id))
                    .is_some_and(|notice| notice.is_retryable())
            })
    }
}

#[cfg(test)]
mod tests;
