// SPDX-License-Identifier: MPL-2.0

//! **The Profiles page as elements** (ELEMENTS-PLAN P5): the heading, a
//! two-line row per Profile, and the foot's three items.
//!
//! A Profile row is the workspace row's recipe with a taller minimum
//! (`PROFILE_ROW_MIN`) and two lines beside the tile: the name, and the
//! detail under it (the email, or where the open Profile syncs). The gear is
//! the same trailing adapter in the same lane.
//!
//! The foot is three sibling items in a row: the sync status (its glyph
//! with the attention dot, and its words), the linked devices (a glyph with
//! the count badge) and `+`. The dot and the badge are paint; what they mean
//! is in each item's name. The foot keeps today's keyboard model: one stop,
//! Enter presses `+`.
//!
//! **Values are today's, each through a token** (QUESTIONS E4): the name's
//! line box is `S4` and the detail's `S3_5`, `S0_5` apart, the pair centred
//! in the row, so each line sits where `draw_two_lines` set it; the foot's
//! status is `S2` from its buttons, and they are `S1` apart, as
//! `head_button_rects` packed them.

use koru_core::{ProfileId, ProfileSummary};
use koru_draw::el::{
    Action, Align, El, ElState, FrameCx, Key, Leaf, Look, Role, StateRecipe, col, leaf, row,
    scroll_y, text,
};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Ink, Line, Radius, Space, Surface, alias, metrics, roles};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::ImageBrush;

use super::{HEADING, MANAGE, PAGE, PRIMARY, SURFACE, TrailingAdapter};
use crate::panes::explore::{
    ExploreView, HeadButton, PopoverKind, PopoverPart, PopoverRow, SyncStatus, label_fit,
};

/// The developer names the host reads geometry by.
pub(crate) const ROW: &str = "Profile row";
pub(crate) const LIST: &str = "Profile list";
pub(crate) const LINES: &str = "Profile lines";
pub(crate) const FOOT: &str = "Profiles foot";
pub(crate) const NAME: &str = "Profile name";

/// The foot's items, keyed by the parts they have always published.
pub(crate) const STATUS: Key = Key::surface(SURFACE, "profile-status");
pub(crate) const DEVICES: Key = Key::surface(SURFACE, "profile-devices");
pub(crate) const NEW_PROFILE: Key = Key::surface(SURFACE, "new-profile");

pub(crate) fn primary(id: ProfileId) -> Key {
    Key::model(id.as_uuid().as_u128(), PRIMARY)
}

pub(crate) fn manage(id: ProfileId) -> Key {
    Key::model(id.as_uuid().as_u128(), MANAGE)
}

/// The foot item a key names.
pub(crate) fn button_of(key: Key) -> Option<HeadButton> {
    [
        (STATUS, HeadButton::Status),
        (DEVICES, HeadButton::Devices),
        (NEW_PROFILE, HeadButton::New),
    ]
    .into_iter()
    .find_map(|(k, button)| (k == key).then_some(button))
}

pub(crate) fn key_of(button: HeadButton) -> Key {
    match button {
        HeadButton::Status => STATUS,
        HeadButton::Devices => DEVICES,
        HeadButton::New => NEW_PROFILE,
    }
}

/// The Profile row's states: the lit plate, as the workspace row's.
pub(crate) static PROFILE_ROW_STATES: StateRecipe = StateRecipe {
    name: "profile row",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::NONE,
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
};

/// The status words: brighter while their item is lit.
static STATUS_WORDS: StateRecipe = StateRecipe {
    name: "foot status",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look {
        bg: None,
        ink: Some(Ink::Primary),
    },
    pressed: Look::NONE,
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
};

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
        let side = metrics::PROFILE_TILE.value;
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

/// The status's attention dot: what its colour says is in the item's name.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Dot {
    /// Up to date, or syncing: the accent.
    Accent,
    /// A cloud that cannot be reached: grey.
    Hint,
    /// Something to act on: red.
    Attention,
}

/// The cloud glyph at the status's leading end, with its dot.
pub(crate) struct StatusGlyphLeaf {
    /// The offer to subscribe, in the accent.
    pub offer: bool,
    pub dot: Option<Dot>,
}

impl Leaf for StatusGlyphLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        // The glyph's slot, as wide as the foot has always given it.
        (18.0, 0.0)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let p = cx.palette();
        let ink = if self.offer {
            p.accent
        } else if state.hovered {
            p.text
        } else {
            p.text_dim
        };
        koru_shell_chrome::tabs::draw_icon(scene, root, engine, icons::CLOUD, at, ink);
        let colour = match self.dot {
            None => return,
            Some(Dot::Accent) => p.accent,
            Some(Dot::Hint) => p.row_hint,
            Some(Dot::Attention) => theme::accent_tone("red").map_or(p.text, |tone| tone.swatch),
        };
        const DOT: f32 = 6.0;
        koru_draw::scene::fill(
            scene,
            root,
            colour,
            &koru_draw::scene::rrect(
                at.x + at.w - DOT + 1.0,
                at.y + (at.h - 18.0) * 0.5,
                DOT,
                DOT,
                DOT * 0.5,
            ),
        );
    }
}

/// One of the foot's square buttons, lit while it is; the devices button
/// carries its count in a badge over its corner.
pub(crate) struct FootButtonLeaf {
    pub glyph: char,
    pub badge: Option<String>,
}

impl Leaf for FootButtonLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        // A lane-wide square, so `+` lines up with every row's gear.
        let side = metrics::TRAILING_LANE_W.value;
        (side, side)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let p = cx.palette();
        koru_ui::icon_button::draw_with_in(
            scene,
            root,
            engine,
            at,
            self.glyph,
            koru_ui::icon_button::State {
                hovered: state.hovered,
                active: false,
                disabled: false,
            },
            koru_ui::icon_button::Metrics::COMPACT,
            p,
        );
        let Some(count) = self.badge.as_deref() else {
            return;
        };
        // A small pill over the button's corner.
        let role = theme::role::hint();
        let w = (engine.simple_layout(count, role).width() + 8.0).max(16.0);
        let pill = Rect::new(at.x + at.w - w + 4.0, at.y - 6.0, w, 16.0);
        let shape = koru_draw::scene::rrect(pill.x, pill.y, pill.w, pill.h, 8.0);
        koru_draw::scene::fill(scene, root, Surface::Raised.of(p), &shape);
        koru_draw::scene::stroke(scene, root, Line::Border.of(p), theme::BORDER_W, &shape);
        let label = engine.simple_layout_wrapped(count, role, (pill.w - 8.0).max(1.0));
        let line_h = label
            .lines()
            .next()
            .map_or(0.0, |line| line.metrics().line_height);
        koru_draw::scene::draw_layout_first_line(
            scene,
            root,
            &label,
            pill.x + 4.0,
            pill.y + (pill.h - line_h) * 0.5,
            Some(Ink::Primary.of(p)),
        );
    }
}

/// A hairline over the top edge of what follows it, taking no room: the
/// foot's rule, drawn over the foot as it always was rather than above it.
pub(crate) struct TopRuleLeaf;

impl Leaf for TopRuleLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, 0.0)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        koru_draw::scene::fill(
            scene,
            root,
            Line::Divider.of(cx.palette()),
            &koru_draw::scene::rect(at.x, at.y, at.w, theme::BORDER_W as f32),
        );
    }
}

impl ExploreView {
    /// The Profiles page as an element tree: the heading, the list that
    /// scrolls under it, and the foot pinned to the panel's floor.
    pub(crate) fn profiles_page(&self, scroll: f32, avatar: Option<&ImageBrush>) -> El {
        let lang = crate::format::lang();
        let rows = self.rows_for(PopoverKind::Profiles);
        let count = rows
            .iter()
            .filter(|row| matches!(row, PopoverRow::Profile(_)))
            .count();
        let heading = format!(
            "{} · {count}",
            koru_i18n::t(lang, koru_i18n::Key::WorkspacesProfiles).to_uppercase()
        );
        col("Profiles page")
            .id(PAGE)
            .semantics(Role::Menu)
            .name(koru_i18n::Key::TerminalSwitchProfile)
            .pad_y(alias::PANEL_PAD)
            .child(
                text(heading.clone())
                    .named("Profiles heading")
                    .id(HEADING)
                    .semantics(Role::Heading)
                    .name_text(heading)
                    .role(roles::POPOVER_HEADING)
                    .ink(Ink::Hint)
                    .pad_x(alias::NAV_PAD)
                    .min_h(metrics::ROW_MIN),
            )
            .child(
                scroll_y(LIST)
                    .fill()
                    .offset(scroll)
                    .children(rows.iter().filter_map(|row| match row {
                        PopoverRow::Profile(i) => self.profiles.get(*i).map(|profile| {
                            let current = self.current_profile == Some(profile.id);
                            self.profile_row(profile, current, avatar.filter(|_| current))
                        }),
                        _ => None,
                    })),
            )
            .child(self.profiles_foot())
    }

    /// One Profile row: the workspace row's shape, two lines beside the tile.
    pub(crate) fn profile_row(
        &self,
        profile: &ProfileSummary,
        current: bool,
        avatar: Option<&ImageBrush>,
    ) -> El {
        let primary = primary(profile.id);
        let gear = manage(profile.id);
        let detail = self.profile_detail(profile, crate::format::lang());
        let name = text(label_fit(&profile.name))
            .named(NAME)
            .role(roles::ROW)
            .ink(if current {
                Ink::Primary
            } else {
                Ink::Secondary
            })
            .emphasis(current)
            .h(Space::S4)
            // Elided like a workspace's name, so a cut name shows it was cut
            // (QUESTIONS E7, the user's answer, 2026-10-30).
            .truncate();
        let mut lines = col(LINES)
            .fill()
            .gap(Space::S0_5)
            .align_self(Align::Centre)
            .child(name);
        // A Profile with nothing to say under its name is one line, centred.
        if !detail.is_empty() {
            lines = lines.child(
                text(detail)
                    .named("Profile detail")
                    .role(roles::HINT)
                    .ink(Ink::Secondary)
                    .h(Space::S3_5)
                    .truncate(),
            );
        }
        row(ROW)
            .min_h(metrics::PROFILE_ROW_MIN)
            .pad_x(alias::ROW_PLATE_INSET)
            .hit_as(primary)
            .child(
                row("Profile plate")
                    .fill()
                    .pad_x(alias::ROW_PLATE_INSET)
                    .gap(Space::S1)
                    .radius(Radius::Metric(metrics::WORKSPACE_GEAR_RADIUS))
                    .states(&PROFILE_ROW_STATES)
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
                            .child(lines),
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
                            .name(koru_i18n::Key::ManageProfile),
                    ),
            )
    }

    /// The foot: a rule over it, then the sync status, the linked devices
    /// and `+`, each its own target and node.
    fn profiles_foot(&self) -> El {
        let lang = crate::format::lang();
        let state = self.sync_status();
        let offer = state == SyncStatus::NotSetUp;
        let words = if offer {
            koru_i18n::t(lang, koru_i18n::Key::CloudSubscribe)
        } else {
            koru_i18n::t(lang, state.key())
        };
        let dot = match state {
            SyncStatus::NotSetUp => None,
            SyncStatus::UpToDate | SyncStatus::Syncing => Some(Dot::Accent),
            SyncStatus::Offline => Some(Dot::Hint),
            SyncStatus::NeedsAttention => Some(Dot::Attention),
        };
        let n = self.linked_count;
        let devices = koru_i18n::fill(
            koru_i18n::t(
                lang,
                if n == 1 {
                    koru_i18n::Key::DevicesCountOne
                } else {
                    koru_i18n::Key::DevicesCountMany
                },
            ),
            &[&n.to_string()],
        );
        let mut status_words = text(words)
            .named("Sync status words")
            .role(roles::ROW)
            .fill()
            .clip()
            .activity([STATUS]);
        status_words = if offer {
            status_words.ink(Ink::Accent)
        } else {
            status_words.ink(Ink::Secondary).states(&STATUS_WORDS)
        };
        col(FOOT)
            .pad_x(alias::NAV_PAD)
            .child(leaf("Foot rule", TopRuleLeaf).decorative())
            .child(
                row("Foot items")
                    .min_h(metrics::PROFILE_FOOT_MIN)
                    .gap(Space::S2)
                    .child(
                        row("Sync status")
                            .fill()
                            .gap(Space::S2)
                            .id(STATUS)
                            .hit()
                            .action(Action::Click)
                            .action(Action::Focus)
                            .semantics(Role::MenuItem)
                            .name_text(words)
                            .child(
                                leaf("Sync glyph", StatusGlyphLeaf { offer, dot })
                                    .decorative()
                                    .activity([STATUS]),
                            )
                            .child(status_words),
                    )
                    .child(
                        row("Foot buttons")
                            .gap(Space::S1)
                            .align_self(Align::Centre)
                            .child(
                                leaf(
                                    "Linked devices",
                                    FootButtonLeaf {
                                        glyph: icons::DEVICES,
                                        badge: (n > 0).then(|| n.to_string()),
                                    },
                                )
                                .semantic()
                                .id(DEVICES)
                                .hit()
                                .action(Action::Click)
                                .action(Action::Focus)
                                .semantics(Role::MenuItem)
                                .name_text(devices),
                            )
                            .child(
                                leaf(
                                    "New profile",
                                    FootButtonLeaf {
                                        glyph: icons::PLUS,
                                        badge: None,
                                    },
                                )
                                .semantic()
                                .id(NEW_PROFILE)
                                .hit()
                                .action(Action::Click)
                                .action(Action::Focus)
                                .semantics(Role::MenuItem)
                                .name(koru_i18n::Key::NewProfile),
                            ),
                    ),
            )
    }

    /// The foot's lit item: the one the pointer is on, or `+` — what Enter
    /// presses — while the keyboard has the foot.
    pub(crate) fn lit_foot_key(&self) -> Option<Key> {
        match self.popover_part {
            Some(PopoverPart::Button(button)) => Some(key_of(button)),
            None => Some(NEW_PROFILE),
            Some(_) => None,
        }
    }
}
