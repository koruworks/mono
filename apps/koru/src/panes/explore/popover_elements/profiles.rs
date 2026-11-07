// SPDX-License-Identifier: MPL-2.0

//! The Profile menu: one-line identity rows and fixed navigation actions.

use koru_core::{ProfileId, ProfileSummary};
use koru_draw::el::{
    Action, Align, El, ElState, FrameCx, Key, Leaf, Role, col, icon, leaf, row, scroll_y, text,
};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, IconSize, Ink, Radius, Space, alias, metrics, roles};
use koru_i18n::{Key as Words, t};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::ImageBrush;

use super::{HEADING, MANAGE, PAGE, PRIMARY, SURFACE, TrailingAdapter, WORKSPACE_ROW_STATES};
use crate::panes::Outcome;
use crate::panes::explore::{ExploreView, PopoverKind, PopoverRow, label_fit};

pub(crate) const ROW: &str = "Profile row";
pub(crate) const LIST: &str = "Profile list";
pub(crate) const NAME: &str = "Profile name";
pub(crate) const MANAGE_PROFILES: Key = Key::surface(SURFACE, "manage-profiles");

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    New,
    Sync,
    Settings,
}

impl MenuAction {
    pub(crate) fn key(self) -> Key {
        Key::surface(
            SURFACE,
            match self {
                Self::New => "new-profile",
                Self::Sync => "global-sync",
                Self::Settings => "app-settings",
            },
        )
    }
    pub(crate) fn words(self) -> Words {
        match self {
            Self::New => Words::ProfileAddNew,
            Self::Sync => Words::ProfileGlobalSync,
            Self::Settings => Words::ProfileAppSettings,
        }
    }
    pub(crate) fn outcome(self) -> Outcome {
        match self {
            Self::New => Outcome::NewProfile,
            Self::Sync => Outcome::OpenSyncSettings,
            Self::Settings => Outcome::OpenSettings,
        }
    }
    pub(crate) fn from_key(key: Key) -> Option<Self> {
        [Self::New, Self::Sync, Self::Settings]
            .into_iter()
            .find(|action| action.key() == key)
    }
}

pub(crate) fn primary(id: ProfileId) -> Key {
    Key::model(id.as_uuid().as_u128(), PRIMARY)
}

pub(crate) fn manage(id: ProfileId) -> Key {
    Key::model(id.as_uuid().as_u128(), MANAGE)
}

/// A Profile's tile: its picture when it is the open Profile and one is
/// decoded, the lettered tile otherwise.
pub(crate) struct ProfileTileLeaf {
    pub icon: Option<String>,
    pub color: Option<String>,
    pub name: String,
    pub email: String,
    /// Only the open Profile has a decoded picture to hand; the others are
    /// rows in an index whose blobs live in folders not opened here.
    pub avatar: Option<ImageBrush>,
}

impl Leaf for ProfileTileLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        let side = metrics::WORKSPACE_TILE.value;
        (side, side)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        if self.avatar.is_some() && self.icon.is_none() {
            koru_ui::profile_picker::draw_avatar(
                scene,
                root,
                engine,
                at,
                self.avatar.as_ref(),
                &self.name,
                &self.email,
                theme::role::row(),
            );
        } else {
            koru_draw::tile::Tile {
                icon: self.icon.as_deref(),
                color: self.color.as_deref(),
                name: &self.name,
            }
            .draw_in(scene, root, engine, at, cx.palette());
        }
    }
}

impl ExploreView {
    pub(crate) fn profile_description(&self, profile: &ProfileSummary) -> String {
        if profile.email.trim().is_empty() {
            t(crate::format::lang(), Words::OnThisComputer).to_owned()
        } else {
            profile.email.clone()
        }
    }

    pub(crate) fn profiles_page(&self, scroll: f32, avatar: Option<&ImageBrush>) -> El {
        let lang = crate::format::lang();
        col("Profiles page")
            .id(PAGE)
            .semantics(Role::Menu)
            .name(Words::TerminalSwitchProfile)
            .pad_y(alias::PANEL_PAD)
            .child(
                row("Profiles heading")
                    .id(HEADING)
                    .pad_x(alias::NAV_PAD)
                    .min_h(metrics::ROW_MIN)
                    .gap(Space::S2)
                    .child(
                        text(t(lang, Words::WorkspacesProfiles).to_uppercase())
                            .id(Key::surface(SURFACE, "profiles-title"))
                            .semantics(Role::Heading)
                            .name(Words::WorkspacesProfiles)
                            .role(roles::POPOVER_HEADING)
                            .ink(Ink::Hint)
                            .fill(),
                    )
                    .child(
                        text(t(lang, Words::ProfileManageProfiles))
                            .role(roles::HINT)
                            .ink(Ink::Secondary)
                            .id(MANAGE_PROFILES)
                            .semantics(Role::MenuItem)
                            .name(Words::ProfileManageProfiles)
                            .hit()
                            .focusable()
                            .action(Action::Click)
                            .action(Action::Focus)
                            .pad_x(Space::S1)
                            .radius(Radius::M)
                            .states(&WORKSPACE_ROW_STATES),
                    ),
            )
            .child(
                scroll_y(LIST).fill().offset(scroll).children(
                    self.rows_for(PopoverKind::Profiles)
                        .iter()
                        .filter_map(|row| match row {
                            PopoverRow::Profile(i) => self.profiles.get(*i).map(|profile| {
                                let current = self.current_profile == Some(profile.id);
                                self.profile_row(profile, current, avatar.filter(|_| current))
                            }),
                            _ => None,
                        }),
                ),
            )
            .child(
                col("Profile menu actions")
                    .child(self.profile_menu_action(MenuAction::New))
                    .child(super::super::workspace_menu_elements::rule())
                    .child(self.profile_menu_action(MenuAction::Sync))
                    .child(self.profile_menu_action(MenuAction::Settings)),
            )
    }

    pub(crate) fn profile_row(
        &self,
        profile: &ProfileSummary,
        current: bool,
        avatar: Option<&ImageBrush>,
    ) -> El {
        let primary = primary(profile.id);
        let gear = manage(profile.id);
        row(ROW)
            .min_h(metrics::ROW_MIN)
            .pad_x(alias::ROW_PLATE_INSET)
            .hit_as(primary)
            .child(
                row("Profile plate")
                    .fill()
                    .pad_x(alias::ROW_PLATE_INSET)
                    .gap(Space::S1)
                    .radius(Radius::Metric(metrics::WORKSPACE_GEAR_RADIUS))
                    .states(&WORKSPACE_ROW_STATES)
                    .activity([primary, gear])
                    .child(
                        row("Profile primary action")
                            .fill()
                            .gap(Space::S2)
                            .id(primary)
                            .focusable()
                            .action(Action::Click)
                            .action(Action::Focus)
                            .semantics(Role::MenuItem)
                            .name_text(&profile.name)
                            .selected(current)
                            .child(
                                leaf(
                                    "Profile tile",
                                    ProfileTileLeaf {
                                        icon: profile.icon.clone(),
                                        color: profile.color.clone(),
                                        name: profile.name.clone(),
                                        email: profile.email.clone(),
                                        avatar: avatar.cloned(),
                                    },
                                )
                                .decorative()
                                .align_self(Align::Centre),
                            )
                            .child(
                                text(label_fit(&profile.name))
                                    .named(NAME)
                                    .role(roles::ROW)
                                    .ink(if current {
                                        Ink::Primary
                                    } else {
                                        Ink::Secondary
                                    })
                                    .emphasis(current)
                                    .fill()
                                    .truncate()
                                    .ink_centred(),
                            ),
                    )
                    .child(
                        leaf(super::GEAR, TrailingAdapter { current, key: gear })
                            .semantic()
                            .w(metrics::TRAILING_LANE_W)
                            .id(gear)
                            .activity([primary])
                            .hit()
                            .focusable()
                            .action(Action::Click)
                            .action(Action::Focus)
                            .semantics(Role::MenuItem)
                            .name(Words::ManageProfile),
                    ),
            )
    }

    fn profile_menu_action(&self, action: MenuAction) -> El {
        let mut item = row("Profile menu action")
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
            .states(&WORKSPACE_ROW_STATES);
        if action == MenuAction::Sync {
            item = item.child(self.sync_channel_glyphs());
        } else {
            item = item.child(
                icon(
                    if action == MenuAction::New {
                        icons::PLUS
                    } else {
                        icons::FADERS
                    },
                    IconSize::L,
                )
                .decorative()
                .ink(Ink::Secondary),
            );
        }
        item.child(
            text(t(crate::format::lang(), action.words()))
                .role(roles::ROW)
                .fill(),
        )
    }
}
