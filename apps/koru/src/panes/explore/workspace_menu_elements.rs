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
        koru_i18n::fill(
            t(lang, Words::SyncControlDescription),
            &[
                self.profile()
                    .map_or(t(lang, Words::WorkspacesProfiles), |p| p.name.as_str()),
                t(lang, self.sync_status().key()),
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
