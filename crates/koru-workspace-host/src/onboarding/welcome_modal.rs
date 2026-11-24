// SPDX-License-Identifier: MPL-2.0

//! **The unified first-run welcome modal** (`Screen::Entry` in
//! `workspace_modal` mode): one floating card over the live workspace shell
//! combining `You` (profile tile + name), `First workspace` (workspace tile +
//! name), and the `Koru Cloud` opt-in card (toggle, A/B copy, Monthly / Yearly
//! billing selector) — plus the compact **Welcome back** sign-in mode when the
//! person clicks *Already have an account? Sign in*.
//!
//! Geometry comes from one element tree laid out per frame; drawing,
//! hit-testing, and accessibility all read `frame.node(key).rect`.

use std::rc::Rc;

use super::setup::leaves::{ButtonLeaf, ButtonStyle, NameField, TileLeaf};
use super::setup::tokens::SETUP_FIELD_GAP;
use super::*;

pub(super) mod leaves;
pub(super) mod tokens;

use koru_draw::el::{
    Align, El, Frame, FrameCx, Interaction, Key as ElKey, Length, Look, Ring, Spacing, StateRecipe,
    col, leaf, row, spacer, text,
};
use koru_draw::theme::{Ink, Line, Radius, Space, Surface, roles};
use koru_i18n::{Key as T, fill, t};

use leaves::{
    BillingSegmentLeaf, CenteredTextLeaf, CloudToggleLeaf, DividerLeaf, LinkLeaf, LogoLeaf,
    ProviderButtonLeaf, ProviderKind, WelcomeProfileTileLeaf,
};
use tokens::*;

const SURFACE: &str = "welcome-modal";

pub(super) const KEY_CARD: ElKey = ElKey::surface(SURFACE, "card");
pub(super) const KEY_TITLE: ElKey = ElKey::surface(SURFACE, "title");
pub(super) const KEY_LEAD: ElKey = ElKey::surface(SURFACE, "lead");
pub(super) const KEY_AVATAR: ElKey = ElKey::surface(SURFACE, "avatar");
pub(super) const KEY_NAME: ElKey = ElKey::surface(SURFACE, "name");
pub(super) const KEY_TILE: ElKey = ElKey::surface(SURFACE, "workspace-tile");
pub(super) const KEY_WORKSPACE_NAME: ElKey = ElKey::surface(SURFACE, "workspace-name");
pub(super) const KEY_CLOUD_CARD: ElKey = ElKey::surface(SURFACE, "cloud-card");
pub(super) const KEY_CLOUD_TOGGLE: ElKey = ElKey::surface(SURFACE, "cloud-toggle");
pub(super) const KEY_BILLING_TRACK: ElKey = ElKey::surface(SURFACE, "billing-track");
pub(super) const KEY_BILLING_MONTHLY: ElKey = ElKey::surface(SURFACE, "billing-monthly");
pub(super) const KEY_BILLING_YEARLY: ElKey = ElKey::surface(SURFACE, "billing-yearly");
pub(super) const KEY_CONTINUE: ElKey = ElKey::surface(SURFACE, "continue");
pub(super) const KEY_GOOGLE: ElKey = ElKey::surface(SURFACE, "google");
pub(super) const KEY_APPLE: ElKey = ElKey::surface(SURFACE, "apple");
pub(super) const KEY_EMAIL_LINK: ElKey = ElKey::surface(SURFACE, "email-link");
pub(super) const KEY_START_WITHOUT_CLOUD: ElKey = ElKey::surface(SURFACE, "start-without-cloud");
pub(super) const KEY_SIGN_IN_LINK: ElKey = ElKey::surface(SURFACE, "sign-in-link");
pub(super) const KEY_SET_UP_SPACE: ElKey = ElKey::surface(SURFACE, "set-up-space");
pub(super) const KEY_STATUS: ElKey = ElKey::surface(SURFACE, "status");
pub(super) const KEY_RECOVERY: ElKey = ElKey::surface(SURFACE, "recovery");

const WINDOW: &str = "Welcome modal window";
const CARD: &str = "Welcome modal card";

static ROW_STATES: StateRecipe = StateRecipe {
    name: "welcome choice row",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::bg(Surface::Hover),
    selected_pressed: Look::bg(Surface::Hover),
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

fn hit_for_key(key: ElKey) -> Option<Hit> {
    Some(match key {
        k if k == KEY_AVATAR => Hit::ProfileAppearance,
        k if k == KEY_NAME => Hit::Name,
        k if k == KEY_TILE => Hit::WorkspaceAppearance,
        k if k == KEY_WORKSPACE_NAME => Hit::WorkspaceName,
        k if k == KEY_CLOUD_TOGGLE => Hit::CloudToggle,
        k if k == KEY_BILLING_MONTHLY => Hit::BillingMonthly,
        k if k == KEY_BILLING_YEARLY => Hit::BillingYearly,
        k if k == KEY_CONTINUE => Hit::Primary,
        k if k == KEY_GOOGLE => Hit::Secondary,
        k if k == KEY_APPLE => Hit::Apple,
        k if k == KEY_EMAIL_LINK => Hit::ContinueWithEmail,
        k if k == KEY_START_WITHOUT_CLOUD => Hit::StartWithoutCloud,
        k if k == KEY_SIGN_IN_LINK => Hit::WelcomeExisting,
        k if k == KEY_SET_UP_SPACE => Hit::Back,
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
        Hit::CloudToggle => KEY_CLOUD_TOGGLE,
        Hit::BillingMonthly => KEY_BILLING_MONTHLY,
        Hit::BillingYearly => KEY_BILLING_YEARLY,
        Hit::Primary => KEY_CONTINUE,
        Hit::Secondary => KEY_GOOGLE,
        Hit::Apple => KEY_APPLE,
        Hit::ContinueWithEmail => KEY_EMAIL_LINK,
        Hit::StartWithoutCloud => KEY_START_WITHOUT_CLOUD,
        Hit::WelcomeExisting => KEY_SIGN_IN_LINK,
        Hit::Back => KEY_SET_UP_SPACE,
        Hit::Recovery => KEY_RECOVERY,
        _ => return None,
    })
}

impl OnboardingView {
    /// Whether the primary submit button (*Continue to checkout* or *Start using
    /// Koru*) is shown on the main welcome view.
    pub(super) fn welcome_modal_shows_primary(&self) -> bool {
        !self.welcome_signin_mode && (self.welcome_signed_in || !self.welcome_cloud_enabled)
    }

    /// Whether the signed-in primary submits directly to checkout rather than
    /// opening Koru immediately.
    pub(super) fn welcome_modal_needs_checkout(&self) -> bool {
        self.welcome_signed_in && self.welcome_cloud_enabled && !self.welcome_cloud_access
    }

    pub(super) fn welcome_modal_primary_label(&self) -> &'static str {
        let lang = self.language;
        if self.setup_finishing {
            t(lang, T::SetupFinishing)
        } else if self.creation_conflict {
            t(lang, T::ProfileCreationManage)
        } else if self.welcome_modal_needs_checkout() {
            t(lang, T::WelcomeModalContinueToCheckout)
        } else {
            t(lang, T::SetupStart)
        }
    }

    pub(super) fn welcome_modal_signed_in_line(&self) -> String {
        let lang = self.language;
        let email = self
            .account_email
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty());
        match (self.browser_provider, email) {
            (BrowserProvider::Google, Some(email)) => {
                fill(t(lang, T::WelcomeCloudSignedInGoogle), &[email])
            }
            (BrowserProvider::Apple, Some(email)) => {
                fill(t(lang, T::WelcomeCloudSignedInApple), &[email])
            }
            (BrowserProvider::Email, Some(email)) => {
                fill(t(lang, T::WelcomeCloudSignedInEmail), &[email])
            }
            (BrowserProvider::Google, None) => t(lang, T::ProfileChooserEyebrowGoogle).to_owned(),
            (BrowserProvider::Apple, None) => t(lang, T::ProfileChooserEyebrowApple).to_owned(),
            (BrowserProvider::Email, None) => t(lang, T::ProfileChooserEyebrowEmail).to_owned(),
        }
    }

    /// Initial focus inside the unified welcome modal lands on `Your name` when
    /// open on the main view (or the first provider on *Welcome back*).
    pub(super) fn welcome_modal_initial_focus(&self) -> usize {
        if self.welcome_signin_mode {
            return 0;
        }
        self.welcome_modal_controls()
            .iter()
            .position(|hit| *hit == Hit::Name)
            .unwrap_or(0)
    }

    pub(super) fn welcome_modal_controls(&self) -> Vec<Hit> {
        if self.setup_finishing {
            return Vec::new();
        }
        if self.welcome_signin_mode {
            return vec![
                Hit::Secondary,
                Hit::Apple,
                Hit::ContinueWithEmail,
                Hit::Back,
            ];
        }
        let mut controls = vec![
            Hit::ProfileAppearance,
            Hit::Name,
            Hit::WorkspaceAppearance,
            Hit::WorkspaceName,
            Hit::CloudToggle,
        ];
        if self.welcome_cloud_enabled {
            controls.extend([Hit::BillingMonthly, Hit::BillingYearly]);
        }
        if self.welcome_signed_in {
            if self.setup_primary_enabled() {
                controls.push(Hit::Primary);
            }
        } else if self.welcome_cloud_enabled {
            controls.extend([
                Hit::Secondary,
                Hit::Apple,
                Hit::ContinueWithEmail,
                Hit::StartWithoutCloud,
                Hit::WelcomeExisting,
            ]);
        } else {
            if self.setup_primary_enabled() {
                controls.push(Hit::Primary);
            }
            controls.push(Hit::WelcomeExisting);
        }
        if self.recovery_offered() {
            controls.push(Hit::Recovery);
        }
        controls
    }

    fn focus_welcome_control(&mut self, hit: Hit) {
        let before = self.controls().get(self.focus).copied();
        if let Some(index) = self.controls().iter().position(|control| *control == hit) {
            self.focus = index;
        }
        self.setup_focus_entered(before);
    }

    pub(super) fn finish_welcome_profile_creation(&mut self, connect_account: bool) {
        if self.setup_finishing || !self.setup_complete() {
            return;
        }
        self.setup_finishing = true;
        self.connect_account = connect_account;
        self.pending = Some(Action::CreateLocalProfile {
            id: self.local_creation_id,
            owner_display_name: self.setup_owner_display_name(),
            name: self.owner_name.borrow().value(),
            workspace_name: self.workspace_name.borrow().value(),
            workspace_icon: self.workspace_icon.clone(),
            workspace_color: self.workspace_color.clone(),
            profile_icon: self.profile_icon.clone(),
            profile_color: self.profile_color.clone(),
            connect_account,
        });
    }

    pub(super) fn activate_welcome_modal(&mut self, hit: Hit) {
        if self.setup_finishing {
            return;
        }
        match hit {
            Hit::Name | Hit::WorkspaceName => self.focus_welcome_control(hit),
            Hit::ProfileAppearance => {
                self.focus_welcome_control(hit);
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
                self.focus_welcome_control(hit);
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
            Hit::CloudToggle => {
                self.welcome_cloud_enabled = !self.welcome_cloud_enabled;
                self.welcome_path = if self.welcome_cloud_enabled {
                    WelcomePath::CreateAccount
                } else {
                    WelcomePath::Offline
                };
                self.focus = self.focus.min(self.controls().len().saturating_sub(1));
                self.error = None;
            }
            Hit::BillingMonthly => {
                if self.welcome_cloud_enabled {
                    self.welcome_billing = WelcomeBillingInterval::Monthly;
                    self.welcome_path = WelcomePath::CreateAccount;
                    self.error = None;
                }
            }
            Hit::BillingYearly => {
                if self.welcome_cloud_enabled {
                    self.welcome_billing = WelcomeBillingInterval::Yearly;
                    self.welcome_path = WelcomePath::CreateAccount;
                    self.error = None;
                }
            }
            Hit::StartWithoutCloud => {
                self.welcome_cloud_enabled = false;
                self.welcome_path = WelcomePath::Offline;
                self.error = None;
                if self.setup_complete() {
                    self.telemetry.push(Telemetry::EntryOffline);
                    self.modal_account_flow = false;
                    self.finish_welcome_profile_creation(false);
                } else {
                    self.focus_welcome_control(Hit::Name);
                }
            }
            Hit::WelcomeExisting => {
                self.welcome_signin_mode = true;
                self.welcome_path = WelcomePath::ExistingAccount;
                self.keyboard_focus = false;
                self.focus = 0;
                self.error = None;
            }
            Hit::Back => {
                self.welcome_signin_mode = false;
                self.welcome_path = if self.welcome_cloud_enabled {
                    WelcomePath::CreateAccount
                } else {
                    WelcomePath::Offline
                };
                self.keyboard_focus = false;
                self.focus = self.welcome_modal_initial_focus();
                self.error = None;
            }
            Hit::Primary if self.creation_conflict => {
                self.setup_finishing = true;
                self.pending = Some(Action::ManageProfiles);
            }
            Hit::Primary if self.setup_complete() => {
                if self.welcome_modal_needs_checkout() {
                    self.pending = Some(Action::StartCheckout {
                        plan: self.welcome_billing.plan_code(),
                    });
                } else {
                    if !self.welcome_signed_in {
                        self.telemetry.push(Telemetry::EntryOffline);
                        self.modal_account_flow = false;
                    }
                    self.finish_welcome_profile_creation(self.welcome_signed_in);
                }
            }
            Hit::Secondary => {
                self.modal_account_flow = true;
                if self.reopen_account_creation() {
                    return;
                }
                self.telemetry.push(Telemetry::EntryGoogle);
                self.begin_browser_signin(BrowserProvider::Google);
            }
            Hit::Apple => {
                self.modal_account_flow = true;
                if self.reopen_account_creation() {
                    return;
                }
                self.telemetry.push(Telemetry::EntryApple);
                self.begin_browser_signin(BrowserProvider::Apple);
            }
            Hit::ContinueWithEmail => {
                self.modal_account_flow = true;
                if self.reopen_account_creation() {
                    return;
                }
                self.telemetry.push(Telemetry::EntryEmail);
                self.begin_browser_signin(BrowserProvider::Email);
            }
            Hit::Recovery if self.recovery_offered() => self.open_recovery(),
            _ => {}
        }
    }

    fn welcome_signin_tree(&mut self) -> El {
        let lang = self.language;
        let controls = self.controls();
        let focused = self
            .keyboard_focus
            .then(|| controls.get(self.focus).copied())
            .flatten();
        let hover = self.hover;
        let area = self.setup_area();
        let margin = Spacing::from(WELCOME_CARD_MARGIN).px(koru_draw::theme::Profile::Desktop);
        let width = Length::Runtime(
            WELCOME_CARD_MAX_W
                .value
                .min((area.w - margin * 2.0).max(280.0)),
            "welcome sign-in modal width",
        );

        let header = col("Welcome back header")
            .gap(WELCOME_BLOCK_GAP)
            .child(
                row("Welcome back brand row")
                    .items_center()
                    .gap(WELCOME_LOGO_GAP)
                    .child(leaf("Koru logo", LogoLeaf).decorative())
                    .child(
                        text(t(lang, T::ProfileChooserWelcomeBack))
                            .named("Welcome back title")
                            .role(roles::SUBTITLE)
                            .emphasis(true)
                            .id(KEY_TITLE),
                    ),
            )
            .child(
                text(t(lang, T::WelcomeSignInLead))
                    .named("Welcome back lead")
                    .role(roles::UI)
                    .ink(Ink::Secondary)
                    .wrap()
                    .id(KEY_LEAD),
            );

        let providers = row("Sign-in providers")
            .gap(WELCOME_PROVIDER_GAP)
            .child(
                leaf(
                    "Google",
                    ProviderButtonLeaf {
                        kind: ProviderKind::Google,
                        label: t(lang, T::WelcomeModalGoogle).to_owned(),
                        hovered: hover == Some(Hit::Secondary),
                        focused: focused == Some(Hit::Secondary),
                    },
                )
                .semantic()
                .fill()
                .id(KEY_GOOGLE)
                .hit(),
            )
            .child(
                leaf(
                    "Apple",
                    ProviderButtonLeaf {
                        kind: ProviderKind::Apple,
                        label: t(lang, T::WelcomeModalApple).to_owned(),
                        hovered: hover == Some(Hit::Apple),
                        focused: focused == Some(Hit::Apple),
                    },
                )
                .semantic()
                .fill()
                .id(KEY_APPLE)
                .hit(),
            );

        let email_link = LinkLeaf::new(
            &mut self.engine,
            t(lang, T::WelcomeModalEmailSignInLink),
            false,
            true,
            hover == Some(Hit::ContinueWithEmail),
            focused == Some(Hit::ContinueWithEmail),
        );
        let setup_link = LinkLeaf::new(
            &mut self.engine,
            t(lang, T::WelcomeSignInSetUpSpace),
            false,
            false,
            hover == Some(Hit::Back),
            focused == Some(Hit::Back),
        );

        let mut body = col("Welcome back actions")
            .gap(WELCOME_BLOCK_GAP)
            .child(
                text(t(lang, T::WelcomeModalSignInLeadExisting))
                    .role(roles::META)
                    .ink(Ink::Secondary),
            )
            .child(providers)
            .child(
                leaf("or sign in with email", email_link)
                    .semantic()
                    .align_self(Align::Centre)
                    .id(KEY_EMAIL_LINK)
                    .hit(),
            );

        if let Some(error) = self.error.clone() {
            body = body.child(
                text(error)
                    .named("Welcome status")
                    .role(roles::META)
                    .ink(Ink::Notice)
                    .wrap()
                    .id(KEY_STATUS),
            );
        }

        let footer_row = row("New to Koru row")
            .items_center()
            .align_self(Align::Centre)
            .gap(Space::S1)
            .child(
                text(t(lang, T::WelcomeSignInNewToKoru))
                    .role(roles::META)
                    .ink(Ink::Secondary),
            )
            .child(
                leaf("Set up your space", setup_link)
                    .semantic()
                    .id(KEY_SET_UP_SPACE)
                    .hit(),
            );

        let card = col(CARD)
            .id(KEY_CARD)
            .w(width)
            .max_w(WELCOME_CARD_MAX_W)
            .pad(WELCOME_CARD_PAD)
            .gap(WELCOME_SECTION_GAP)
            .child(header)
            .child(body)
            .child(
                col("Footer divider")
                    .pad_y(WELCOME_DIVIDER_MARGIN)
                    .child(leaf("Divider", DividerLeaf).decorative()),
            )
            .child(footer_row);

        col(WINDOW)
            .items_center()
            .pad_x(WELCOME_CARD_MARGIN)
            .child(spacer())
            .child(card)
            .child(spacer())
    }

    fn welcome_modal_tree(&mut self) -> El {
        if self.welcome_signin_mode {
            return self.welcome_signin_tree();
        }
        let lang = self.language;
        let finishing = self.setup_finishing;
        let controls = self.controls();
        let active_control = controls.get(self.focus).copied();
        let focused_ring = self.keyboard_focus.then_some(active_control).flatten();
        let hover = self.hover;
        let caret_on = self.caret_on();
        let area = self.setup_area();
        let margin = Spacing::from(WELCOME_CARD_MARGIN).px(koru_draw::theme::Profile::Desktop);
        let pad_px = Spacing::from(WELCOME_CARD_PAD).px(koru_draw::theme::Profile::Desktop);
        let card_w = WELCOME_CARD_MAX_W
            .value
            .min((area.w - margin * 2.0).max(280.0));
        let inner_w = (card_w - pad_px * 2.0).max(200.0);
        let width = Length::Runtime(card_w, "welcome modal width");

        let is_yearly = self.welcome_billing == WelcomeBillingInterval::Yearly;
        let cloud_on = self.welcome_cloud_enabled;
        let signed_in = self.welcome_signed_in;
        let (label_key, placeholder_key) = self.setup_owner_keys();

        let header = col("Welcome header")
            .gap(WELCOME_BLOCK_GAP)
            .child(
                row("Welcome brand row")
                    .items_center()
                    .gap(WELCOME_LOGO_GAP)
                    .child(leaf("Koru logo", LogoLeaf).decorative())
                    .child(
                        text(t(lang, self.welcome_header_variant.title_key()))
                            .named("Welcome title")
                            .role(roles::SUBTITLE)
                            .emphasis(true)
                            .wrap()
                            .id(KEY_TITLE),
                    ),
            )
            .child(
                text(t(lang, self.welcome_header_variant.lead_key()))
                    .named("Welcome lead")
                    .role(roles::UI)
                    .ink(Ink::Secondary)
                    .wrap()
                    .id(KEY_LEAD),
            );

        let owner_field = NameField {
            input: Rc::clone(&self.owner_name),
            placeholder: t(lang, placeholder_key).to_owned(),
            focused: active_control == Some(Hit::Name),
            caret_on,
            disabled: finishing,
        };
        let workspace_field = NameField {
            input: Rc::clone(&self.workspace_name),
            placeholder: t(lang, T::WorkspaceName).to_owned(),
            focused: active_control == Some(Hit::WorkspaceName),
            caret_on,
            disabled: finishing,
        };

        let fields = col("Welcome space fields")
            .gap(Space::S1_5)
            .child(
                text(t(lang, label_key))
                    .role(roles::META)
                    .ink(Ink::Secondary),
            )
            .child(
                row("You")
                    .gap(SETUP_FIELD_GAP)
                    .child(
                        leaf(
                            "Profile tile",
                            WelcomeProfileTileLeaf {
                                icon: self.profile_icon.clone(),
                                color: self.profile_color.clone(),
                                name: self.owner_name.borrow().value(),
                                circular_avatar: signed_in,
                                hovered: hover == Some(Hit::ProfileAppearance),
                                focused: focused_ring == Some(Hit::ProfileAppearance),
                                disabled: finishing,
                            },
                        )
                        .semantic()
                        .id(KEY_AVATAR)
                        .hit(),
                    )
                    .child(
                        leaf("NameField", owner_field)
                            .semantic()
                            .fill()
                            .id(KEY_NAME)
                            .hit(),
                    ),
            )
            .child(
                text(t(lang, T::SetupWorkspaceLabel))
                    .role(roles::META)
                    .ink(Ink::Secondary),
            )
            .child(
                row("First workspace")
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
                                focused: focused_ring == Some(Hit::WorkspaceAppearance),
                                disabled: finishing,
                            },
                        )
                        .semantic()
                        .id(KEY_TILE)
                        .hit(),
                    )
                    .child(
                        leaf("NameField", workspace_field)
                            .semantic()
                            .fill()
                            .id(KEY_WORKSPACE_NAME)
                            .hit(),
                    ),
            );

        let cloud_toggle = leaf(
            "Koru Cloud toggle",
            CloudToggleLeaf {
                on: cloud_on,
                rtl: lang.is_rtl(),
                hovered: hover == Some(Hit::CloudToggle),
                focused: focused_ring == Some(Hit::CloudToggle),
                disabled: finishing,
            },
        )
        .semantic()
        .id(KEY_CLOUD_TOGGLE)
        .hit();

        let mut cloud_card = col("Koru Cloud card")
            .id(KEY_CLOUD_CARD)
            .pad_x(WELCOME_OPTION_PAD_X)
            .pad_y(WELCOME_CLOUD_PAD_Y)
            .gap(Space::S2_5);

        let billing_disabled = !cloud_on || finishing;
        let monthly_segment = BillingSegmentLeaf::new(
            &mut self.engine,
            t(lang, T::WelcomeModalBillingMonthly),
            None,
            !is_yearly,
            hover == Some(Hit::BillingMonthly),
            focused_ring == Some(Hit::BillingMonthly),
            billing_disabled,
        );
        let yearly_segment = BillingSegmentLeaf::new(
            &mut self.engine,
            t(lang, T::WelcomeModalBillingYearly),
            Some(t(lang, T::WelcomeModalSave16)),
            is_yearly,
            hover == Some(Hit::BillingYearly),
            focused_ring == Some(Hit::BillingYearly),
            billing_disabled,
        );
        let mut monthly_leaf = leaf("Monthly billing", monthly_segment)
            .semantic()
            .id(KEY_BILLING_MONTHLY);
        let mut yearly_leaf = leaf("Yearly billing", yearly_segment)
            .semantic()
            .id(KEY_BILLING_YEARLY);
        if cloud_on {
            monthly_leaf = monthly_leaf.hit();
            yearly_leaf = yearly_leaf.hit();
        }
        let billing_track = row("Billing toggle track")
            .id(KEY_BILLING_TRACK)
            .items_center()
            .gap(WELCOME_SEGMENT_GAP)
            .child(monthly_leaf)
            .child(yearly_leaf);

        let price_key = if is_yearly {
            T::WelcomeModalPlusPriceYearly
        } else {
            T::WelcomeModalPlusPrice
        };
        let price_ink = if cloud_on { Ink::Primary } else { Ink::Hint };
        let secondary_ink = if cloud_on { Ink::Secondary } else { Ink::Hint };
        let price_col = col("Founding price")
            .items_end()
            .gap(Space::S0)
            .child(
                text(t(lang, price_key))
                    .role(roles::ROW)
                    .emphasis(true)
                    .ink(price_ink),
            )
            .child(
                text(t(lang, T::WelcomeCloudFoundingPriceLock))
                    .role(roles::META)
                    .ink(secondary_ink),
            );

        let cloud_top = row("Koru Cloud header")
            .gap(WELCOME_ROW_GAP)
            .child(
                col("Koru Cloud copy")
                    .fill()
                    .gap(WELCOME_OPTION_TEXT_GAP)
                    .child(
                        text(t(lang, T::WelcomeCloudTitle))
                            .role(roles::ROW)
                            .emphasis(true),
                    )
                    .child(
                        text(t(lang, self.welcome_cloud_variant.body_key()))
                            .role(roles::META)
                            .ink(secondary_ink)
                            .wrap(),
                    ),
            )
            .child(cloud_toggle.align_self(Align::Start));

        let billing_row = row("Billing interval row")
            .items_center()
            .gap(WELCOME_ROW_GAP)
            .child(billing_track)
            .child(spacer())
            .child(price_col);

        cloud_card = cloud_card.child(cloud_top).child(billing_row);

        if signed_in {
            cloud_card = cloud_card.child(
                text(self.welcome_modal_signed_in_line())
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .wrap(),
            );
        }

        let mut footer = col("Welcome footer").gap(WELCOME_BLOCK_GAP);
        if let Some(error) = self.error.clone() {
            footer = footer.child(
                text(error)
                    .named("Welcome status")
                    .role(roles::META)
                    .ink(Ink::Notice)
                    .wrap()
                    .id(KEY_STATUS),
            );
        }

        if signed_in {
            let primary_label = self.welcome_modal_primary_label().to_owned();
            footer = footer.child(
                leaf(
                    "Welcome primary",
                    ButtonLeaf {
                        label: primary_label,
                        width: 0.0,
                        style: ButtonStyle::Primary,
                        hovered: hover == Some(Hit::Primary),
                        focused: focused_ring == Some(Hit::Primary),
                        disabled: !self.setup_primary_enabled(),
                    },
                )
                .semantic()
                .id(KEY_CONTINUE)
                .hit(),
            );
            let subcopy_key = if self.welcome_modal_needs_checkout() {
                Some(if is_yearly {
                    T::WelcomeModalCheckoutFootYearly
                } else {
                    T::WelcomeModalCheckoutFootMonthly
                })
            } else if !cloud_on {
                Some(T::WelcomeModalOffFoot)
            } else {
                None
            };
            if let Some(key) = subcopy_key {
                let caption = CenteredTextLeaf::new(
                    &mut self.engine,
                    t(lang, key),
                    theme::role::meta(),
                    true,
                    inner_w,
                );
                footer = footer.child(leaf("Welcome footnote", caption).decorative());
            }
        } else if cloud_on {
            let providers = row("Sign-in providers")
                .gap(WELCOME_PROVIDER_GAP)
                .child(
                    leaf(
                        "Google",
                        ProviderButtonLeaf {
                            kind: ProviderKind::Google,
                            label: t(lang, T::WelcomeModalGoogle).to_owned(),
                            hovered: hover == Some(Hit::Secondary),
                            focused: focused_ring == Some(Hit::Secondary),
                        },
                    )
                    .semantic()
                    .fill()
                    .id(KEY_GOOGLE)
                    .hit(),
                )
                .child(
                    leaf(
                        "Apple",
                        ProviderButtonLeaf {
                            kind: ProviderKind::Apple,
                            label: t(lang, T::WelcomeModalApple).to_owned(),
                            hovered: hover == Some(Hit::Apple),
                            focused: focused_ring == Some(Hit::Apple),
                        },
                    )
                    .semantic()
                    .fill()
                    .id(KEY_APPLE)
                    .hit(),
                );
            let email_link = LinkLeaf::new(
                &mut self.engine,
                t(lang, T::WelcomeModalUseEmailInstead),
                false,
                true,
                hover == Some(Hit::ContinueWithEmail),
                focused_ring == Some(Hit::ContinueWithEmail),
            );
            let offline_link = LinkLeaf::new(
                &mut self.engine,
                t(lang, T::WelcomeModalStartWithoutCloud),
                false,
                true,
                hover == Some(Hit::StartWithoutCloud),
                focused_ring == Some(Hit::StartWithoutCloud),
            );
            let links_row = row("Secondary auth links")
                .items_center()
                .align_self(Align::Centre)
                .gap(Space::S1_5)
                .child(
                    leaf("Use email instead", email_link)
                        .semantic()
                        .id(KEY_EMAIL_LINK)
                        .hit(),
                )
                .child(text("·").role(roles::META).ink(Ink::Secondary))
                .child(
                    leaf("Start without Cloud", offline_link)
                        .semantic()
                        .id(KEY_START_WITHOUT_CLOUD)
                        .hit(),
                );
            footer = footer
                .child(
                    text(t(lang, T::WelcomeModalTrialLead))
                        .role(roles::META)
                        .ink(Ink::Secondary),
                )
                .child(providers)
                .child(links_row);
        } else {
            let primary_label = self.welcome_modal_primary_label().to_owned();
            let caption = CenteredTextLeaf::new(
                &mut self.engine,
                t(lang, T::WelcomeModalOffFoot),
                theme::role::meta(),
                true,
                inner_w,
            );
            footer = footer
                .child(
                    leaf(
                        "Start using Koru",
                        ButtonLeaf {
                            label: primary_label,
                            width: 0.0,
                            style: ButtonStyle::Primary,
                            hovered: hover == Some(Hit::Primary),
                            focused: focused_ring == Some(Hit::Primary),
                            disabled: !self.setup_primary_enabled(),
                        },
                    )
                    .semantic()
                    .id(KEY_CONTINUE)
                    .hit(),
                )
                .child(leaf("Welcome footnote", caption).decorative());
        }

        if !signed_in {
            let sign_in_link = LinkLeaf::new(
                &mut self.engine,
                t(lang, T::WelcomeModalSignInLink),
                false,
                true,
                hover == Some(Hit::WelcomeExisting),
                focused_ring == Some(Hit::WelcomeExisting),
            );
            let sign_in_row = row("Existing account row")
                .items_center()
                .align_self(Align::Centre)
                .gap(Space::S1)
                .child(
                    text(t(lang, T::WelcomeModalAlreadyHaveAccount))
                        .role(roles::META)
                        .ink(Ink::Secondary),
                )
                .child(
                    leaf("Sign in", sign_in_link)
                        .semantic()
                        .id(KEY_SIGN_IN_LINK)
                        .hit(),
                );
            footer = footer
                .child(
                    col("Footer divider")
                        .pad_y(WELCOME_DIVIDER_MARGIN)
                        .child(leaf("Divider", DividerLeaf).decorative()),
                )
                .child(sign_in_row);
        }

        if !finishing && self.recovery_offered() {
            footer = footer.child(
                text(t(lang, T::OnboardingRecoveryLink))
                    .named("Recovery link")
                    .role(roles::META)
                    .ink(Ink::Accent)
                    .wrap()
                    .align_self(Align::Centre)
                    .radius(Radius::M)
                    .states(&ROW_STATES)
                    .id(KEY_RECOVERY)
                    .hit()
                    .focusable(),
            );
        }

        let card = col(CARD)
            .id(KEY_CARD)
            .w(width)
            .max_w(WELCOME_CARD_MAX_W)
            .pad(WELCOME_CARD_PAD)
            .gap(WELCOME_SECTION_GAP)
            .child(header)
            .child(fields)
            .child(cloud_card)
            .child(footer);

        col(WINDOW)
            .items_center()
            .pad_x(WELCOME_CARD_MARGIN)
            .child(spacer())
            .child(card)
            .child(spacer())
    }

    pub(super) fn welcome_modal_frame(&mut self) -> (El, Frame) {
        let tree = self.welcome_modal_tree();
        let controls = self.controls();
        let interaction = Interaction {
            hot: self.hover.and_then(key_for_hit),
            pressed: None,
            focused: self
                .keyboard_focus
                .then(|| controls.get(self.focus).copied().and_then(key_for_hit))
                .flatten(),
        };
        let area = self.setup_area();
        let frame = tree.layout(
            &mut self.engine,
            &mut self.setup_cache,
            area,
            FrameCx::root().with_interaction(interaction),
        );
        (tree, frame)
    }

    pub(super) fn welcome_modal_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        if self.setup_finishing {
            return None;
        }
        let (_, frame) = self.welcome_modal_frame();
        let key = frame.hit_at(x, y)?;
        let hit = hit_for_key(key)?;
        if hit == Hit::Primary && !self.setup_primary_enabled() {
            return None;
        }
        Some(hit)
    }

    /// Pointer press on either name field inside the unified Welcome modal.
    pub(super) fn welcome_modal_field_press(&mut self, x: f32, y: f32) -> Option<usize> {
        if self.setup_finishing || self.welcome_signin_mode {
            return None;
        }
        let (_, frame) = self.welcome_modal_frame();
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

    pub(super) fn welcome_modal_drag(&mut self, x: f32) -> bool {
        if self.welcome_signin_mode {
            return false;
        }
        let (_, frame) = self.welcome_modal_frame();
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

    pub(super) fn draw_welcome_modal(&mut self, canvas: &mut Scene, root: Affine) {
        let (tree, frame) = self.welcome_modal_frame();
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
            &scene::Panel::popover(WELCOME_CARD_RADIUS.value),
        );
        let p = theme::p();
        frame.paint(&tree, canvas, root, &mut self.engine);
        if let Some(rect) = frame.node(KEY_CLOUD_CARD).map(|node| node.rect) {
            scene::stroke(
                canvas,
                root,
                p.panel_border,
                1.0,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, WELCOME_OPTION_RADIUS.value),
            );
        }
    }
}
