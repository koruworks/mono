// SPDX-License-Identifier: MPL-2.0

//! Account menu: scrollable identities and fixed navigation actions.

use koru_core::{ProfileId, ProfileSummary};
use koru_draw::el::{
    Action, Align, El, ElState, FrameCx, Key, Leaf, Look, Role, StateRecipe, col, icon, leaf, row,
    scroll_y, spacer, text,
};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, IconSize, Ink, Radius, Space, Surface, alias, metrics, roles};
use koru_i18n::{Key as Words, fill, t};
use vello::{Scene, kurbo::Affine, peniko::ImageBrush};

use super::{MANAGE, PAGE, PRIMARY, SURFACE, WORKSPACE_ROW_STATES};
use crate::panes::Outcome;
use crate::panes::explore::{ExploreView, label_fit};

pub(crate) const ROW: &str = "Profile row";
pub(crate) const LIST: &str = "Profile list";
pub(crate) const NAME: &str = "Profile name";
pub(crate) const MANAGE_PROFILES: Key = Key::surface(SURFACE, "manage-profiles");

static PROFILE_STATES: StateRecipe = StateRecipe {
    name: "profile identity",
    selected: Look::bg(Surface::Hover),
    selected_hover: Look::bg(Surface::Hover),
    selected_pressed: Look::bg(Surface::Hover),
    ..WORKSPACE_ROW_STATES
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    New,
    Sync,
    Settings,
    Help,
}

impl MenuAction {
    pub(crate) fn key(self) -> Key {
        Key::surface(
            SURFACE,
            match self {
                Self::New => "new-profile",
                Self::Sync => "global-sync",
                Self::Settings => "profile-settings",
                Self::Help => "profile-help",
            },
        )
    }
    pub(crate) fn words(self) -> Words {
        match self {
            Self::New => Words::ProfileAddNew,
            Self::Sync => Words::CloudTitle,
            Self::Settings => Words::SettingsTab,
            Self::Help => Words::ProfileHelpFeedback,
        }
    }
    pub(crate) fn outcome(self) -> Outcome {
        match self {
            Self::New => Outcome::NewProfile,
            Self::Sync => Outcome::ManageCloud,
            Self::Settings => Outcome::OpenSettings,
            Self::Help => Outcome::OpenHelp,
        }
    }
    pub(crate) fn from_key(key: Key) -> Option<Self> {
        [Self::New, Self::Sync, Self::Settings, Self::Help]
            .into_iter()
            .find(|action| action.key() == key)
    }
}

pub(crate) fn primary(id: ProfileId) -> Key {
    Key::model(id.as_uuid().as_u128(), PRIMARY)
}

// Reuse the switcher's trailing-action lane; profile rows open a new window.
pub(crate) fn manage(id: ProfileId) -> Key {
    Key::model(id.as_uuid().as_u128(), MANAGE)
}

pub(crate) struct ProfileTileLeaf {
    pub icon: Option<String>,
    pub color: Option<String>,
    pub name: String,
    pub avatar: Option<ImageBrush>,
}

impl Leaf for ProfileTileLeaf {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
        let side = metrics::PROFILE_AVATAR.value;
        (side, side)
    }
    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _: ElState,
    ) {
        koru_ui::profile_picker::draw_identity(
            scene,
            root,
            engine,
            at,
            koru_ui::profile_picker::ProfileIdentity {
                avatar: self.avatar.as_ref(),
                name: &self.name,
                icon: self.icon.as_deref(),
                color: self.color.as_deref(),
            },
            theme::role::row(),
            cx.palette(),
        );
    }
}

impl ExploreView {
    pub(crate) fn profile_description(&self, profile: &ProfileSummary) -> String {
        let lang = crate::format::lang();
        let rows = self
            .catalog
            .get(&profile.id)
            .map(Vec::as_slice)
            .or_else(|| {
                (self.current_profile == Some(profile.id)).then_some(self.workspaces.as_slice())
            });
        let count = self
            .profile_workspace_counts
            .get(&profile.id)
            .copied()
            .or_else(|| rows.map(|rows| rows.iter().filter(|w| w.deleted_at.is_none()).count()));
        let Some(count) = count else {
            return String::new();
        };
        let words = if count == 1 {
            Words::ProfileWorkspaceOne
        } else {
            Words::ProfileWorkspaceMany
        };
        fill(t(lang, words), &[&count.to_string()])
    }

    pub(crate) fn profiles_page(&self, scroll: f32, avatar: Option<&ImageBrush>) -> El {
        col("Profiles page")
            .id(PAGE)
            .semantics(Role::Menu)
            .name(Words::TerminalSwitchProfile)
            .pad(alias::PANEL_PAD)
            .child(
                scroll_y(LIST)
                    .fill()
                    .gap(Space::S0_5)
                    .offset(scroll)
                    .children(self.profiles.iter().map(|profile| {
                        let current = self.current_profile == Some(profile.id);
                        self.profile_row(profile, current, avatar.filter(|_| current))
                    })),
            )
            .child(self.profile_menu_action(MenuAction::New))
            .child(crate::panes::explore::workspace_menu_elements::rule())
            .child(self.profile_menu_action(MenuAction::Sync))
            .child(self.profile_menu_action(MenuAction::Settings))
            .child(self.profile_menu_action(MenuAction::Help))
    }

    fn profile_menu_action(&self, action: MenuAction) -> El {
        let lang = crate::format::lang();
        let glyph = match action {
            MenuAction::New => icons::PLUS,
            MenuAction::Sync => icons::CLOUD,
            MenuAction::Settings => icons::FADERS,
            MenuAction::Help => icons::QUESTION,
        };
        let mut labels = col("Account action labels").fill().gap(Space::S0_5).child(
            text(t(lang, action.words()))
                .role(roles::ROW)
                .ink(Ink::Primary)
                .truncate(),
        );
        if action == MenuAction::Sync {
            let count = self.linked_count;
            let key = if count == 1 {
                Words::DevicesCountOne
            } else {
                Words::DevicesCountMany
            };
            let detail = format!(
                "{} · {}",
                t(lang, self.sync_status().key()),
                fill(t(lang, key), &[&count.to_string()])
            );
            labels = labels.child(
                text(detail)
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .truncate(),
            );
        }
        let mut action_row = super::menu_action_row("Account action")
            .id(action.key())
            .hit()
            .focusable()
            .action(Action::Click)
            .action(Action::Focus)
            .semantics(Role::MenuItem)
            .name(action.words())
            .child(
                icon(glyph, koru_ui::menu::ActionRow::ICON)
                    .decorative()
                    .ink(Ink::Secondary)
                    .align_self(Align::Centre),
            )
            .child(labels);
        if action == MenuAction::Settings {
            action_row = action_row.children(
                koru_keys::show_hint(koru_keys::ShortcutId::Settings, koru_keys::Section::Menu)
                    .map(|hint| {
                        text(hint)
                            .role(roles::HINT)
                            .ink(Ink::Hint)
                            .align_self(Align::Centre)
                    }),
            );
        } else if action == MenuAction::Help {
            action_row = action_row.child(
                icon(icons::CARET_RIGHT, IconSize::M)
                    .decorative()
                    .ink(Ink::Secondary)
                    .align_self(Align::Centre),
            );
        }
        action_row
    }

    pub(crate) fn profile_row(
        &self,
        profile: &ProfileSummary,
        current: bool,
        avatar: Option<&ImageBrush>,
    ) -> El {
        let primary = primary(profile.id);
        let trailing = manage(profile.id);
        let description = self.profile_description(profile);
        let mut plate = row(ROW)
            .hit_as(primary)
            .min_h(metrics::PROFILE_ROW_MIN)
            .pad_x(Space::S2)
            .gap(Space::S2)
            .radius(Radius::M)
            .states(&PROFILE_STATES)
            .selected(current)
            .activity([primary, trailing])
            .child(
                row("Profile primary action")
                    .fill()
                    .pad_y(Space::S2)
                    .gap(Space::S3)
                    .id(primary)
                    .focusable()
                    .action(Action::Click)
                    .action(Action::Focus)
                    .semantics(Role::MenuItem)
                    .name_text(&profile.name)
                    .selected(current)
                    .child(
                        leaf(
                            "Profile avatar",
                            ProfileTileLeaf {
                                icon: profile.icon.clone(),
                                color: profile.color.clone(),
                                name: profile.name.clone(),
                                avatar: avatar.cloned(),
                            },
                        )
                        .decorative()
                        .align_self(Align::Centre),
                    )
                    .child(
                        col("Profile identity")
                            .fill()
                            .gap(Space::S0_5)
                            .child(
                                text(label_fit(&profile.name))
                                    .named(NAME)
                                    .role(roles::ROW)
                                    .ink(Ink::Primary)
                                    .truncate(),
                            )
                            .child(
                                text(description)
                                    .named("Profile detail")
                                    .role(roles::META)
                                    .ink(Ink::Secondary)
                                    .truncate(),
                            ),
                    ),
            );
        if current {
            plate = plate.child(
                icon(icons::CHECK, IconSize::M)
                    .decorative()
                    .w(metrics::ROW_MIN)
                    .align_self(Align::Centre),
            );
        } else {
            plate = plate.child(
                row("Open profile in new window")
                    .id(trailing)
                    .hit()
                    .focusable()
                    .action(Action::Click)
                    .action(Action::Focus)
                    .semantics(Role::MenuItem)
                    .name(Words::ProfileOpenNewWindow)
                    .w(metrics::ROW_MIN)
                    .h(metrics::ROW_MIN)
                    .items_center()
                    .radius(Radius::M)
                    .states(&WORKSPACE_ROW_STATES)
                    .align_self(Align::Centre)
                    .child(spacer())
                    .child(
                        icon(icons::POP_OUT, IconSize::M)
                            .decorative()
                            .ink(Ink::Secondary),
                    )
                    .child(spacer()),
            );
        }
        plate
    }
}
