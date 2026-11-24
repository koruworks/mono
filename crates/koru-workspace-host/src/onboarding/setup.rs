// SPDX-License-Identifier: MPL-2.0

//! **The first-run setup card** (welcome step 1, TECHNICAL.final §5): one
//! card in place of the Profile and Workspace steps — a profile tile and
//! name, a first workspace tile and name, and one primary button. Both tiles
//! open the full icon-and-color modal when clicked and reveal a pencil
//! affordance on hover.
//!
//! ```text
//! col  Setup window          the area beside the welcome sidebar, below the caption
//!   spacer
//!   col  Setup card          at most 540, never wider than the area less 32
//!     text  title, lead      not when embedded setup/title, setup/lead
//!     text  "You" / "Profile"                  setup/you
//!     row   You              tile, name field  setup/avatar, setup/name
//!     text  "First workspace"
//!     row   First workspace  tile, name field  setup/workspace-tile, setup/workspace-name
//!     text  note, status     when there is one setup/note, setup/status
//!     leaf  primary                            setup/primary
//!     col   Other actions    Back, Resume, Recovery when offered
//!   spacer
//! ```
//!
//! **The frame is the one source of geometry** (D7): drawing, the pointer and
//! the accessibility projection all read `frame.node(key).rect`. Focus, typed
//! actions and semantics stay in [`OnboardingView`] and
//! [`super::accessibility`]; `el::Frame::semantics()` and `focus_order()` are
//! not used for this screen.
//!
//! **Finishing is a state of the card**, not a screen: while the primary's
//! creation is in flight the card offers no control, answers no press, draws
//! its fields and tiles inert and its primary reads *Finishing…*.
//!
//! Every size is a token from [`tokens`] or measured geometry; this file and
//! [`leaves`] are on the source scan's migrated list
//! (`koru_draw::theme::ui_scan`).

use super::welcome_modal::leaves::LinkLeaf;
use super::*;

pub(super) mod leaves;
pub(super) mod tokens;

use koru_draw::el::{
    Align, El, Frame, FrameCx, Interaction, Key as ElKey, Length, Look, Ring, Spacing, StateRecipe,
    col, leaf, row, spacer, text,
};
use koru_draw::theme::{Ink, Line, Radius, Space, Surface, roles};
use koru_i18n::{Key as T, t};
use std::rc::Rc;

use leaves::{ButtonLeaf, ButtonStyle, NameField, TileLeaf};
use tokens::*;

/// Which label the card's first field wears: on first run it names the
/// person, *You*; in Manage → New profile it names only the new Profile,
/// *Profile* (D8). The host says which; the first-run window never does.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum SetupOwnerLabel {
    #[default]
    You,
    Profile,
}

/// The emoji and color a first workspace starts with (D6).
pub(super) const SETUP_DEFAULT_EMOJI: &str = "🌱";
pub(super) const SETUP_DEFAULT_COLOR: &str = "green";

/// The object every key on the card belongs to.
const SURFACE: &str = "setup";

pub(super) const KEY_CARD: ElKey = ElKey::surface(SURFACE, "card");
pub(super) const KEY_TITLE: ElKey = ElKey::surface(SURFACE, "title");
pub(super) const KEY_LEAD: ElKey = ElKey::surface(SURFACE, "lead");
pub(super) const KEY_YOU_LABEL: ElKey = ElKey::surface(SURFACE, "you");
pub(super) const KEY_YOU_ROW: ElKey = ElKey::surface(SURFACE, "you-row");
pub(super) const KEY_AVATAR: ElKey = ElKey::surface(SURFACE, "avatar");
pub(super) const KEY_NAME: ElKey = ElKey::surface(SURFACE, "name");
pub(super) const KEY_WORKSPACE_ROW: ElKey = ElKey::surface(SURFACE, "workspace-row");
pub(super) const KEY_TILE: ElKey = ElKey::surface(SURFACE, "workspace-tile");
pub(super) const KEY_WORKSPACE_NAME: ElKey = ElKey::surface(SURFACE, "workspace-name");
pub(super) const KEY_NOTE: ElKey = ElKey::surface(SURFACE, "note");
pub(super) const KEY_STATUS: ElKey = ElKey::surface(SURFACE, "status");
pub(super) const KEY_PRIMARY: ElKey = ElKey::surface(SURFACE, "primary");
pub(super) const KEY_BACK: ElKey = ElKey::surface(SURFACE, "back");
pub(super) const KEY_RESUME: ElKey = ElKey::surface(SURFACE, "resume");
pub(super) const KEY_RECOVERY: ElKey = ElKey::surface(SURFACE, "recovery");

/// The developer names the frame's nodes carry.
pub(super) const WINDOW: &str = "Setup window";
pub(super) const CARD: &str = "Setup card";

/// **The key-to-control table**, both ways.
fn hit_for_key(key: ElKey) -> Option<Hit> {
    Some(match key {
        k if k == KEY_AVATAR => Hit::ProfileAppearance,
        k if k == KEY_NAME => Hit::Name,
        k if k == KEY_TILE => Hit::WorkspaceAppearance,
        k if k == KEY_WORKSPACE_NAME => Hit::WorkspaceName,
        k if k == KEY_PRIMARY => Hit::Primary,
        k if k == KEY_BACK => Hit::Back,
        k if k == KEY_RESUME => Hit::ResumeAccount,
        k if k == KEY_RECOVERY => Hit::Recovery,
        _ => return None,
    })
}

pub(super) fn key_for_hit(hit: Hit) -> Option<ElKey> {
    Some(match hit {
        Hit::ProfileAppearance => KEY_AVATAR,
        Hit::Name => KEY_NAME,
        Hit::WorkspaceAppearance => KEY_TILE,
        Hit::WorkspaceName => KEY_WORKSPACE_NAME,
        Hit::Primary => KEY_PRIMARY,
        Hit::Back => KEY_BACK,
        Hit::ResumeAccount => KEY_RESUME,
        Hit::Recovery => KEY_RECOVERY,
        _ => return None,
    })
}

/// **The recovery link's states**: a wrapping line of words that is a
/// target and a focus stop, lit under the pointer, ringed under the keyboard.
static LINK_STATES: StateRecipe = StateRecipe {
    name: "setup recovery link",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

impl OnboardingView {
    /// Show the card, as *Use Koru without an account* does: the account-free
    /// route, the keyboard on the name field.
    pub fn show_setup(&mut self) {
        self.connect_account = false;
        self.modal_account_flow = false;
        self.setup_finishing = false;
        self.screen = Screen::Setup;
        self.focus = self.setup_initial_focus();
        self.error = None;
    }

    /// The host's word on the first label (D8): Manage → New profile says
    /// [`SetupOwnerLabel::Profile`], under which the field starts empty, is
    /// not filled from the account, and is sent with no owner name (Q-A,
    /// Q-G).
    pub fn set_owner_label(&mut self, label: SetupOwnerLabel) {
        self.owner_label = label;
    }

    /// Whether the workspace-modal setup card shows *Change* in its eyebrow
    /// to return to Screen 1.
    pub(super) fn setup_has_change(&self) -> bool {
        self.workspace_modal
            && !self.embedded
            && koru_features::cloud_sync()
            && self.accounts_offered()
    }

    /// Whether the card has a *Back* / *Change*: only when there is an entry
    /// choice to go back to, and never when embedded in Settings → New profile.
    pub(super) fn setup_has_back(&self) -> bool {
        !self.embedded
            && (self.setup_has_change()
                || (!self.workspace_modal && Screen::first() != Screen::Setup))
    }

    /// Which subtitle the setup card displays for the active onboarding mode.
    pub(super) fn setup_lead_key(&self) -> T {
        if self.workspace_modal && self.connect_account {
            T::SetupLeadPlus
        } else {
            T::SetupLead
        }
    }

    /// The eyebrow shown above the title in workspace-modal onboarding.
    pub(super) fn setup_eyebrow_text(&self) -> String {
        let lang = self.language;
        if !self.connect_account {
            return t(lang, T::SetupEyebrowOffline).to_owned();
        }
        let plan = match self.welcome_path {
            WelcomePath::CreateAccount => t(lang, T::SetupEyebrowPlus),
            WelcomePath::ExistingAccount | WelcomePath::Offline => t(lang, T::SetupEyebrowAccount),
        };
        match self
            .account_email
            .as_deref()
            .map(str::trim)
            .filter(|email| !email.is_empty())
        {
            Some(email) => format!("{plan} · {email}"),
            None => plan.to_owned(),
        }
    }

    /// Both names hold a non-space character. `TextInput::value` trims and
    /// `is_empty` does not, so a name of spaces leaves the primary disabled.
    pub(super) fn setup_complete(&self) -> bool {
        !self.owner_name.borrow().value().is_empty()
            && !self.workspace_name.borrow().value().is_empty()
    }

    /// Whether the primary can be pressed: complete, or offering Manage in
    /// the conflict state — and never while the card is finishing.
    pub(super) fn setup_primary_enabled(&self) -> bool {
        !self.setup_finishing && (self.creation_conflict || self.setup_complete())
    }

    pub(super) fn setup_primary_label(&self) -> &'static str {
        let key = if self.setup_finishing {
            T::SetupFinishing
        } else if self.creation_conflict {
            T::ProfileCreationManage
        } else if self.owner_label == SetupOwnerLabel::Profile {
            T::SetupCreateProfile
        } else {
            T::SetupStart
        };
        t(self.language, key)
    }

    /// What the action sends as the owner's name: the typed name on first
    /// run, where it names the person and the Profile at once (D3); nothing
    /// under *Profile*, signed in or out, so Manage never renames the account
    /// (Q-G).
    pub(super) fn setup_owner_display_name(&self) -> Option<String> {
        match self.owner_label {
            SetupOwnerLabel::You => Some(self.owner_name.borrow().value()),
            SetupOwnerLabel::Profile => None,
        }
    }

    /// The first label, and the field's placeholder and accessible name.
    pub(super) fn setup_owner_keys(&self) -> (T, T) {
        match self.owner_label {
            SetupOwnerLabel::You => (T::SetupYouLabel, T::SetupNamePlaceholder),
            SetupOwnerLabel::Profile => (T::SetupProfileLabel, T::A11yFirstRunProfileName),
        }
    }

    /// Entering the card lands the caret directly in the name field so typing
    /// begins immediately, while Shift+Tab reaches the profile tile before it.
    pub(super) fn setup_initial_focus(&self) -> usize {
        self.setup_controls()
            .iter()
            .position(|control| *control == Hit::Name)
            .unwrap_or(0)
    }

    /// The card's stops, in reading order: the profile tile, the name field,
    /// the workspace tile, the workspace name, the primary when it can be
    /// pressed, then *Back*, *Resume* and recovery when offered. Nothing while
    /// finishing.
    pub(super) fn setup_controls(&self) -> Vec<Hit> {
        if self.setup_finishing {
            return Vec::new();
        }
        let mut controls = vec![
            Hit::ProfileAppearance,
            Hit::Name,
            Hit::WorkspaceAppearance,
            Hit::WorkspaceName,
        ];
        if self.setup_primary_enabled() {
            controls.push(Hit::Primary);
        }
        if self.setup_has_back() {
            controls.push(Hit::Back);
        }
        if !self.workspace_modal && self.can_reopen_account_creation() {
            controls.push(Hit::ResumeAccount);
        }
        if self.recovery_offered() {
            controls.push(Hit::Recovery);
        }
        controls
    }

    /// Put the keyboard on `hit`, when the card offers it.
    fn focus_setup_control(&mut self, hit: Hit) {
        let before = self.controls().get(self.focus).copied();
        if let Some(index) = self.controls().iter().position(|control| *control == hit) {
            self.focus = index;
        }
        self.setup_focus_entered(before);
    }

    /// **What entering a stop does.** Leaving a name field blurs it so its
    /// selection does not linger; entering the workspace name while it is
    /// still the default selects all of it, so typing replaces it.
    pub(super) fn setup_focus_entered(&mut self, before: Option<Hit>) {
        if self.screen != Screen::Setup && !(self.screen == Screen::Entry && self.workspace_modal) {
            return;
        }
        let now = self.controls().get(self.focus).copied();
        if now == before {
            return;
        }
        self.blur_text_field(before);
        if now == Some(Hit::WorkspaceName) && !self.workspace_name_edited {
            self.workspace_name
                .borrow_mut()
                .select_all(&mut self.engine);
        }
    }

    /// What a press, Enter or Space on a card control does.
    pub(super) fn activate_setup(&mut self, hit: Hit) {
        if self.setup_finishing {
            return;
        }
        match hit {
            Hit::Name | Hit::WorkspaceName => self.focus_setup_control(hit),
            Hit::ProfileAppearance => {
                self.focus_setup_control(hit);
                let name = self.owner_name.borrow().value();
                let title = if name.is_empty() {
                    t(self.language, self.setup_owner_keys().0)
                } else {
                    &name
                };
                self.workspace_appearance = Some(EmojiColorPanel::for_profile_draft(
                    &mut self.engine,
                    title,
                    self.profile_icon.clone(),
                    self.profile_color.clone(),
                ));
            }
            Hit::WorkspaceAppearance => {
                self.focus_setup_control(hit);
                let name = self.workspace_name.borrow().value();
                let title = if name.is_empty() {
                    t(self.language, T::SetupWorkspaceLabel)
                } else {
                    &name
                };
                self.workspace_appearance = Some(EmojiColorPanel::for_draft(
                    &mut self.engine,
                    title,
                    self.workspace_icon.clone(),
                    self.workspace_color.clone(),
                ));
            }
            Hit::Primary if self.creation_conflict => {
                self.setup_finishing = true;
                self.pending = Some(Action::ManageProfiles);
            }
            Hit::Primary if self.setup_complete() => {
                self.setup_finishing = true;
                self.pending = Some(Action::CreateLocalProfile {
                    id: self.local_creation_id,
                    owner_display_name: self.setup_owner_display_name(),
                    name: self.owner_name.borrow().value(),
                    workspace_name: self.workspace_name.borrow().value(),
                    workspace_icon: self.workspace_icon.clone(),
                    workspace_color: self.workspace_color.clone(),
                    profile_icon: self.profile_icon.clone(),
                    profile_color: self.profile_color.clone(),
                    connect_account: self.connect_account,
                });
            }
            Hit::Back if self.setup_has_back() => {
                self.modal_account_flow = false;
                self.show_entry();
            }
            Hit::Recovery if self.recovery_offered() => self.open_recovery(),
            _ => {}
        }
    }

    /// The area the card is laid out in: beside the welcome sidebar and below
    /// the caption in the first-run window; the host's column when embedded.
    pub(super) fn setup_area(&self) -> Rect {
        if self.embedded {
            return Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1);
        }
        let right = if self.workspace_modal {
            Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1)
        } else {
            welcome::split(self.viewport.0, self.viewport.1).1
        };
        let top = if self.custom_caption {
            theme::CAPTION_H
        } else {
            0.0
        };
        Rect::new(right.x, right.y + top, right.w, (right.h - top).max(0.0))
    }

    /// **The card as one tree**, built per frame from the view's state.
    fn setup_tree(&mut self) -> El {
        let lang = self.language;
        let finishing = self.setup_finishing;
        let controls = self.controls();
        let focused = controls.get(self.focus).copied();
        let hover = self.hover;
        let caret_on = self.caret_on();
        let (label_key, placeholder_key) = self.setup_owner_keys();
        let area = self.setup_area();

        let width = if self.embedded {
            Length::Runtime(area.w, "Manage column width")
        } else {
            let margin = Spacing::from(SETUP_CARD_MARGIN).px(koru_draw::theme::Profile::Desktop);
            Length::Runtime(
                SETUP_CARD_MAX_W.value.min(area.w - margin * 2.0),
                "setup host width",
            )
        };
        // Embedded, the page frames the card: no panel, so no inset, and the
        // fields line up with the page's head.
        let pad: Spacing = if self.embedded {
            Space::S0.into()
        } else {
            SETUP_CARD_PAD.into()
        };

        let label = |key: T| {
            text(t(lang, key))
                .named("Setup label")
                .role(roles::META)
                .ink(Ink::Secondary)
        };
        let owner = NameField {
            input: Rc::clone(&self.owner_name),
            placeholder: t(lang, placeholder_key).to_owned(),
            focused: focused == Some(Hit::Name),
            caret_on,
            disabled: finishing,
        };
        let workspace = NameField {
            input: Rc::clone(&self.workspace_name),
            placeholder: t(lang, T::WorkspaceName).to_owned(),
            focused: focused == Some(Hit::WorkspaceName),
            caret_on,
            disabled: finishing,
        };

        let mut card = col(CARD)
            .id(KEY_CARD)
            .w(width)
            .max_w(SETUP_CARD_MAX_W)
            .pad(pad)
            .gap(SETUP_ROW_GAP);
        if self.workspace_modal && !self.embedded {
            card = card.min_h(super::welcome_modal::tokens::WELCOME_MODAL_MIN_H);
        }
        // Embedded, the host page's head already says *New profile* with its
        // own lead, so the card has neither (Q-C): its first line is the
        // *Profile* label, directly under the page head.
        if !self.embedded {
            if self.workspace_modal {
                let mut eyebrow = row("Setup eyebrow")
                    .items_center()
                    .gap(SETUP_FIELD_GAP)
                    .child(
                        text(self.setup_eyebrow_text())
                            .named("Setup eyebrow text")
                            .role(roles::META)
                            .ink(Ink::Secondary)
                            .fill(),
                    );
                if !finishing && self.setup_has_change() {
                    let change = LinkLeaf::new(
                        &mut self.engine,
                        t(lang, T::SetupChange),
                        false,
                        false,
                        hover == Some(Hit::Back),
                        focused == Some(Hit::Back),
                    );
                    eyebrow = eyebrow.child(leaf("Change", change).semantic().id(KEY_BACK).hit());
                }
                card = card.child(eyebrow);
            }
            card = card
                .child(
                    text(t(lang, T::SetupTitle))
                        .named("Setup title")
                        .role(roles::SUBTITLE)
                        .wrap()
                        .id(KEY_TITLE),
                )
                .child(
                    text(t(lang, self.setup_lead_key()))
                        .named("Setup lead")
                        .role(roles::UI)
                        .ink(Ink::Secondary)
                        .wrap()
                        .id(KEY_LEAD),
                );
        }
        card = card
            .child(label(label_key).id(KEY_YOU_LABEL))
            .child(
                row("You")
                    .id(KEY_YOU_ROW)
                    .gap(SETUP_FIELD_GAP)
                    .child(
                        leaf(
                            "Profile tile",
                            TileLeaf {
                                component: "ProfileIcon",
                                icon: self.profile_icon.clone(),
                                color: self.profile_color.clone(),
                                name: self.owner_name.borrow().value(),
                                fallback_initial: false,
                                hovered: hover == Some(Hit::ProfileAppearance),
                                focused: focused == Some(Hit::ProfileAppearance),
                                disabled: finishing,
                            },
                        )
                        .semantic()
                        .id(KEY_AVATAR)
                        .hit(),
                    )
                    .child(
                        leaf("NameField", owner)
                            .semantic()
                            .fill()
                            .id(KEY_NAME)
                            .hit(),
                    ),
            )
            .child(label(T::SetupWorkspaceLabel))
            .child(
                row("First workspace")
                    .id(KEY_WORKSPACE_ROW)
                    .gap(SETUP_FIELD_GAP)
                    .child(
                        leaf(
                            "Workspace tile",
                            TileLeaf {
                                component: "WorkspaceIcon",
                                icon: self.workspace_icon.clone(),
                                color: self.workspace_color.clone(),
                                name: self.workspace_name.borrow().value(),
                                fallback_initial: true,
                                hovered: hover == Some(Hit::WorkspaceAppearance),
                                focused: focused == Some(Hit::WorkspaceAppearance),
                                disabled: finishing,
                            },
                        )
                        .semantic()
                        .id(KEY_TILE)
                        .hit(),
                    )
                    .child(
                        leaf("NameField", workspace)
                            .semantic()
                            .fill()
                            .id(KEY_WORKSPACE_NAME)
                            .hit(),
                    ),
            );
        // Only the legacy connected first run says where things live: the
        // two-screen modal explains the selected plan in its eyebrow and footer.
        if self.connect_account && !self.embedded && !self.workspace_modal {
            card = card.child(
                text(t(lang, T::SetupConnectedNote))
                    .named("Setup note")
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .wrap()
                    .id(KEY_NOTE),
            );
        }
        if let Some(error) = self.error.clone() {
            card = card.child(
                text(error)
                    .named("Setup status")
                    .role(roles::META)
                    .ink(Ink::Notice)
                    .wrap()
                    .id(KEY_STATUS),
            );
        }
        if self.workspace_modal && !self.embedded {
            card = card.child(spacer());
        }
        let primary_label = self.setup_primary_label().to_owned();
        card = card.child(
            leaf(
                "Setup primary",
                ButtonLeaf {
                    label: primary_label,
                    width: 0.0,
                    style: ButtonStyle::Primary,
                    hovered: hover == Some(Hit::Primary),
                    focused: focused == Some(Hit::Primary),
                    disabled: !self.setup_primary_enabled(),
                },
            )
            .semantic()
            .id(KEY_PRIMARY)
            .hit(),
        );

        // The quieter ways on, under the primary — none while finishing, as
        // *Back* was not drawn on the finishing screen this replaces.
        let mut others = Vec::new();
        if !finishing && !self.workspace_modal && self.setup_has_back() {
            let back = t(lang, T::Back);
            others.push(self.setup_button(
                "Back",
                KEY_BACK,
                back,
                ButtonStyle::Ghost,
                Hit::Back,
                focused,
            ));
        }
        if !finishing && !self.workspace_modal && self.can_reopen_account_creation() {
            let resume = t(lang, T::AccountCreationResume);
            others.push(self.setup_button(
                "Resume account creation",
                KEY_RESUME,
                resume,
                ButtonStyle::Secondary,
                Hit::ResumeAccount,
                focused,
            ));
        }
        if !finishing && self.recovery_offered() {
            others.push(
                text(t(lang, T::OnboardingRecoveryLink))
                    .named("Recovery link")
                    .role(roles::ROW)
                    .ink(Ink::Accent)
                    .wrap()
                    .align_self(Align::Stretch)
                    .radius(Radius::M)
                    .states(&LINK_STATES)
                    .id(KEY_RECOVERY)
                    .hit()
                    .focusable(),
            );
        }
        if !others.is_empty() {
            card = card.child(
                col("Other actions")
                    .items_start()
                    .gap(SETUP_ACTION_GAP)
                    .children(others),
            );
        }

        if self.embedded {
            col(WINDOW).items_start().child(card)
        } else {
            col(WINDOW)
                .items_center()
                .pad_x(SETUP_CARD_MARGIN)
                .child(spacer())
                .child(card)
                .child(spacer())
        }
    }

    fn setup_button(
        &mut self,
        name: &'static str,
        key: ElKey,
        label: &str,
        style: ButtonStyle,
        hit: Hit,
        focused: Option<Hit>,
    ) -> El {
        let width = style.width(&mut self.engine, label);
        leaf(
            name,
            ButtonLeaf {
                label: label.to_owned(),
                width,
                style,
                hovered: self.hover == Some(hit),
                focused: focused == Some(hit),
                disabled: false,
            },
        )
        .semantic()
        .id(key)
        .hit()
    }

    /// **The card laid out**: the tree and the frame everything reads. The
    /// host's pointer and keyboard state go in as the frame's interaction, so
    /// the recovery link's ring and hover are the frame's.
    pub(super) fn setup_frame(&mut self) -> (El, Frame) {
        let tree = self.setup_tree();
        let controls = self.controls();
        let interaction = Interaction {
            hot: self.hover.and_then(key_for_hit),
            pressed: None,
            focused: controls.get(self.focus).copied().and_then(key_for_hit),
        };
        let area = self.setup_area();
        let frame = tree.layout(
            &mut self.engine,
            &mut self.setup_cache,
            area,
            FrameCx::root().with_interaction(interaction),
        );
        if let Some(card) = frame.node(KEY_CARD) {
            self.setup_height.set(Some(
                card.rect.y + card.rect.h + SETUP_EMBEDDED_BOTTOM.value,
            ));
        }
        (tree, frame)
    }

    /// Where `key` is on the card this frame, when the card shows it.
    pub(super) fn setup_node_rect(&mut self, key: ElKey) -> Option<Rect> {
        let (_, frame) = self.setup_frame();
        frame.node(key).map(|node| node.rect)
    }

    /// The control under a point: the frame names the key. `None` while
    /// finishing, and for the primary while it cannot be pressed.
    pub(super) fn setup_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        if self.setup_finishing {
            return None;
        }
        let (_, frame) = self.setup_frame();
        let key = frame.hit_at(x, y)?;
        let hit = hit_for_key(key)?;
        if hit == Hit::Primary && !self.setup_primary_enabled() {
            return None;
        }
        Some(hit)
    }

    /// A press on one of the two name fields: the caret goes where it landed,
    /// and an unedited workspace name is selected whole. Answers the stop the
    /// keyboard moves to.
    pub(super) fn setup_field_press(&mut self, x: f32, y: f32) -> Option<usize> {
        if self.setup_finishing {
            return None;
        }
        let (_, frame) = self.setup_frame();
        let name = frame.node(KEY_NAME).map(|node| node.rect);
        let workspace = frame.node(KEY_WORKSPACE_NAME).map(|node| node.rect);
        let before = self.controls().get(self.focus).copied();
        let hit = if name.is_some_and(|rect| {
            self.owner_name
                .borrow_mut()
                .click(&mut self.engine, rect, x, y)
        }) {
            Hit::Name
        } else if workspace.is_some_and(|rect| {
            self.workspace_name
                .borrow_mut()
                .click(&mut self.engine, rect, x, y)
        }) {
            if !self.workspace_name_edited && before != Some(Hit::WorkspaceName) {
                let mut input = self.workspace_name.borrow_mut();
                input.select_all(&mut self.engine);
                // A press that selected everything is not the start of a
                // drag that would narrow it again.
                input.release();
            }
            Hit::WorkspaceName
        } else {
            return None;
        };
        let index = self.controls().iter().position(|control| *control == hit)?;
        self.focus = index;
        if hit != Hit::WorkspaceName {
            self.setup_focus_entered(before);
        } else if before != Some(Hit::WorkspaceName) {
            self.blur_text_field(before);
        }
        Some(index)
    }

    /// A drag carried on from a press in either name field.
    pub(super) fn setup_drag(&mut self, x: f32) -> bool {
        let (_, frame) = self.setup_frame();
        let name = frame.node(KEY_NAME).map(|node| node.rect);
        let workspace = frame.node(KEY_WORKSPACE_NAME).map(|node| node.rect);
        let mut dragged = false;
        if let Some(rect) = name {
            dragged |= self.owner_name.borrow_mut().drag(&mut self.engine, rect, x);
        }
        if let Some(rect) = workspace {
            dragged |= self
                .workspace_name
                .borrow_mut()
                .drag(&mut self.engine, rect, x);
        }
        dragged
    }

    /// The focused stop's rectangle, for a host that scrolls the card.
    pub(super) fn setup_focused_rect(&mut self) -> Option<Rect> {
        let hit = *self.controls().get(self.focus)?;
        self.setup_node_rect(key_for_hit(hit)?)
    }

    /// The height a host page gives the embedded card: the card's bottom and
    /// the room under it, from the last frame laid out.
    pub(super) fn setup_content_height(&self) -> Option<f32> {
        self.setup_height.get()
    }

    /// Draw the card: in the first-run window inside one floating panel, so
    /// its records nest under one `FloatingPanel` in the inspector;
    /// embedded, on the host's page with no panel.
    pub(super) fn draw_setup(&mut self, canvas: &mut Scene, root: Affine) {
        let (tree, frame) = self.setup_frame();
        if self.embedded {
            frame.paint(&tree, canvas, root, &mut self.engine);
            return;
        }
        let Some(card) = frame.node(KEY_CARD).map(|node| node.rect) else {
            return;
        };
        let _panel = scene::floating_panel_scope(
            canvas,
            root,
            card.x,
            card.y,
            card.w,
            card.h,
            &scene::Panel::popover(SETUP_CARD_RADIUS.value),
        );
        frame.paint(&tree, canvas, root, &mut self.engine);
    }
}

#[cfg(test)]
mod tests;
