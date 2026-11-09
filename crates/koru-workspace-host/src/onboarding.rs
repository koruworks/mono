// SPDX-License-Identifier: MPL-2.0

//! Identity-free first-run form state and drawing.
//!
//! Store, browser, and network work leave as typed actions. This controller
//! owns no Profile or Workspace value and performs no I/O.
//!
//! **What it offers a screen reader is [`accessibility`]**, a child module
//! rather than a section of this file: §7.10 of the accessibility roadmap is
//! explicit that a projection does not grow inline inside this one. What stays
//! here is what was already here — the screens, their geometry, their
//! validation and the typed actions they raise — plus the two fields the
//! projection needs and could not keep for itself (`screen_shown`,
//! `screen_instance`) and one grid helper three call sites had a copy of.

pub mod accessibility;
mod browser_signin;
#[cfg(test)]
mod creation_tests;
mod email;
mod profile_selection;
mod recovery;
use profile_selection::SelectionDraft;
pub use recovery::{RecoverableProfile, UnassignedWorkspaceRow};

use koru_agent_wire::{AccountProfileChoice, AccountProfileRow, AccountState};
use koru_appearance_ui::{EmojiColorPanel, Outcome as AppearanceOutcome};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{inspect, scene, theme, tile::Tile};
use koru_ui::Typed;
use koru_ui::button::{
    ActionButton, ActionStyle, ButtonRole, GAP as BUTTON_GAP, button_width, draw_button,
    draw_button_enabled,
};
use koru_ui::scrollbar::{self, ScrollbarGeom};
use koru_ui::text_input::{InputStyle, TextInput};
use koru_ui::welcome::{self, Layout as WelcomeLayout};
use vello::{
    Scene,
    kurbo::Affine,
    peniko::{Color, Fill},
};
use winit::{
    keyboard::{Key, ModifiersState, NamedKey},
    window::CursorIcon,
};

const PAD: f32 = 24.0;
const MAX_W: f32 = 600.0;
const CARD_GAP: f32 = 12.0;
const EXISTING_ROW_PITCH: f32 = 58.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Work,
    School,
    Personal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Telemetry {
    EntryOffline,
    EntryGoogle,
    EntryApple,
    ProfilePurpose(Purpose),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Account(koru_agent_wire::AccountCommand),
    ManageProfiles,
    OpenCreatedProfile(koru_core::ProfileId),
    ConfirmAccountCreation,
    CancelAccountCreation,
    StartGoogle,
    StartApple,
    /// The welcome screen's one submit. Which journey it turns out to be is
    /// the worker's to answer, and the stage that comes back says which
    /// screen follows.
    ContinueWithEmail {
        address: String,
        password: String,
    },
    SubmitEmailCode {
        code: String,
    },
    SubmitEmailPassword {
        password: String,
    },
    ResendEmailCode,
    OpenUri(&'static str),
    RetryAccount,
    RetryEnrollment,
    CancelGoogle,
    CancelEnrollment,
    SelectAccountProfiles {
        profile_ids: Vec<String>,
    },
    RestoreLocalProfile {
        profile_id: String,
    },
    /// `NeedsProfile` recovery (root-catalog §6.3): restore a shared Profile
    /// deletion by its batch. The host performs it off this thread and opens
    /// the Profile that comes back.
    RestoreDeletedProfile {
        profile: koru_core::ProfileId,
        deletion: koru_store_core::ids::DeletionId,
    },
    CreateLocalProfile {
        id: koru_core::ProfileId,
        owner_display_name: String,
        name: String,
        workspace_name: String,
        template: String,
        workspace_icon: Option<String>,
        workspace_color: Option<String>,
        profile_icon: Option<String>,
        profile_color: Option<String>,
        connect_account: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Entry,
    Browser,
    /// The address and the password are with the worker. Which of the two
    /// journeys this is, is not known yet — that is the whole point of the
    /// one screen — so the copy here says neither.
    EmailWorking,
    EmailCode,
    EmailPassword,
    AccountStatus,
    ExistingProfiles,
    RestoringProfile,
    /// The restorable Trash and the unassigned workspaces, from the Profile
    /// form (root-catalog §6.3). See `recovery`.
    Recovery,
    Profile,
    Workspace,
    Finishing,
}

impl Screen {
    /// Where the first run starts. **With Koru Cloud shown** that is the
    /// entry choice — an account (email, Google, Apple) or *Use Koru without an
    /// account*. **With it hidden** (koru-features) there is nothing to
    /// choose: the first run starts where *Use Koru without an account* leads,
    /// the local Profile form, and every later step is unchanged.
    fn first() -> Screen {
        if koru_features::cloud_sync() {
            Screen::Entry
        } else {
            Screen::Profile
        }
    }

    /// The inspector's name for the screen — what the F12 tree shows as the
    /// node every part of the form nests under.
    fn inspection_name(self) -> &'static str {
        match self {
            Screen::Entry => "Entry",
            Screen::Browser => "Browser sign-in",
            Screen::EmailWorking => "Email sign-in",
            Screen::EmailCode => "Email code",
            Screen::EmailPassword => "Email password",
            Screen::AccountStatus => "Account status",
            Screen::ExistingProfiles => "Profile chooser",
            Screen::RestoringProfile => "Restoring Profile",
            Screen::Recovery => "Recovery",
            Screen::Profile => "Profile form",
            Screen::Workspace | Screen::Finishing => "Workspace form",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Primary,
    Secondary,
    /// The welcome screen's Sign in with Apple, beside Google's `Secondary`.
    Apple,
    /// The welcome screen's email form: two fields and the button under them.
    EmailAddress,
    EmailPassword,
    /// The eye inside the password field: show or hide the password.
    RevealPassword,
    ContinueWithEmail,
    /// The code screen: the six boxes, and the two quiet ways out of them.
    EmailCode,
    ResendCode,
    AnotherAddress,
    /// The password screen's two boxes.
    NewPassword,
    ConfirmPassword,
    /// *Try again* in the welcome screen's failure banner.
    Retry,
    Terms,
    Privacy,
    AccountAction(usize),
    Existing(usize),
    OpenExisting(usize),
    RetryExisting(usize),
    RefreshProfiles,
    CancelSelection,
    /// The Profile form's way to the recovery screen.
    Recovery,
    /// A recovery row's *Restore*, by its position in the deleted list.
    RestoreDeleted(usize),
    CreateNew,
    ContinueOffline,
    ResumeAccount,
    Name,
    ProfileName,
    Purpose(Purpose),
    ProfileAppearance,
    Template(usize),
    WorkspaceAppearance,
    Back,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProfileSource {
    Local,
    TrustedDevice,
    Unavailable,
}

/// [`welcome::Failure`], holding its message.
struct EntryFailure {
    at: welcome::Action,
    message: String,
    retry: bool,
}

impl EntryFailure {
    fn borrow(&self) -> welcome::Failure<'_> {
        welcome::Failure {
            at: self.at,
            message: &self.message,
            retry: self.retry,
        }
    }
}

fn welcome_action(hit: Hit) -> Option<welcome::Action> {
    match hit {
        Hit::EmailAddress => Some(welcome::Action::Address),
        Hit::EmailPassword => Some(welcome::Action::Password),
        Hit::ContinueWithEmail => Some(welcome::Action::ContinueWithEmail),
        Hit::Secondary => Some(welcome::Action::Google),
        Hit::Apple => Some(welcome::Action::Apple),
        Hit::Primary => Some(welcome::Action::ContinueOffline),
        Hit::Retry => Some(welcome::Action::Retry),
        Hit::Terms => Some(welcome::Action::Terms),
        Hit::Privacy => Some(welcome::Action::Privacy),
        _ => None,
    }
}

pub struct OnboardingView {
    created_profile_retry: Option<(koru_core::ProfileId, koru_agent_wire::AccountCommand)>,
    account_state: AccountState,
    browser_signin: Option<browser_signin::Submission>,
    /// Which provider the browser wait is for, so its copy names the right one.
    browser_apple: bool,
    account_forms: std::collections::BTreeMap<u64, Box<OnboardingView>>,
    language: koru_i18n::Lang,
    viewport: (f32, f32),
    engine: TextEngine,
    screen: Screen,
    account_profiles: Vec<AccountProfileRow>,
    account_profile_sources: Vec<ProfileSource>,
    account_surface: Option<crate::account_ui::Surface>,
    selected_existing: Option<usize>,
    /// What `NeedsProfile` can recover, pushed in by the host (`recovery`).
    recovery_deleted: Vec<RecoverableProfile>,
    recovery_unassigned: Vec<UnassignedWorkspaceRow>,
    selection: SelectionDraft,
    existing_profile_scroll: f32,
    existing_profile_scroll_drag: Option<f32>,
    pointer: (f32, f32),
    enrollment_cancelling: bool,
    /// The email journey's five boxes. They live here rather than in a
    /// journey struct because every other field on this form does, and the
    /// key router reaches them all the same way.
    email_address: TextInput,
    email_password: TextInput,
    email_code: TextInput,
    new_password: TextInput,
    confirm_password: TextInput,
    owner_name: TextInput,
    profile_name: TextInput,
    profile_name_edited: bool,
    purpose: Option<Purpose>,
    profile_icon: Option<String>,
    profile_color: Option<String>,
    profile_appearance_customized: bool,
    profile_appearance: Option<EmojiColorPanel>,
    connect_account: bool,
    workspace_name: TextInput,
    template: Option<usize>,
    workspace_name_edited: bool,
    workspace_icon: Option<String>,
    workspace_color: Option<String>,
    workspace_appearance_customized: bool,
    workspace_appearance: Option<EmojiColorPanel>,
    custom_caption: bool,
    /// The form is embedded in a host page that frames it (the Profile
    /// manager's *New profile*): no presentation pane, and the column starts
    /// at the left edge and the top, under the page's own head.
    embedded: bool,
    /// Visible host area in form coordinates; overlays must not use the
    /// scrollable form's virtual height as their window.
    embedded_visible_area: Option<Rect>,
    caption_hover: Option<koru_ui::caption::Action>,
    maximized: bool,
    /// **The screen the accessibility tree was last built for, and the number
    /// that screen's controls are named by.** Both belong to
    /// [`accessibility`], which is the only thing that reads or writes them;
    /// they are fields here because a projection built from `&self` state
    /// cannot keep state of its own. See that module for why a transition
    /// mints a new number rather than reusing one.
    screen_shown: Option<Screen>,
    screen_instance: u64,
    focus: usize,
    hover: Option<Hit>,
    pending: Option<Action>,
    /// Kept through uncertain Store responses; a new form mints a new identity.
    local_creation_id: koru_core::ProfileId,
    creation_conflict: bool,
    creation_submission: Option<crate::account_ui::CommandSubmissionId>,
    creation_waiting: bool,
    telemetry: Vec<Telemetry>,
    error: Option<String>,
    /// What the welcome screen's last sign-in attempt was, so a failure that
    /// comes back to it is drawn under that action and *Try again* repeats it.
    last_attempt: Option<welcome::Action>,
    /// The welcome field `error` is about, when it is about a field rather
    /// than the attempt. Cleared with every new attempt.
    error_field: Option<welcome::Action>,
    /// The focused field's caret blinks from here; every key and press
    /// restarts it so the caret is solid while somebody is typing.
    blink_origin: std::time::Instant,
    /// The phase last drawn, so the loop asks for a frame only when it flips.
    caret_drawn: Option<bool>,
    /// A caret blinks only in the window that has the keyboard.
    window_focused: bool,
}

impl Default for OnboardingView {
    fn default() -> Self {
        let mut engine = TextEngine::new();
        let name = TextInput::new(&mut engine, theme::role::row());
        let profile_name = TextInput::new(&mut engine, theme::role::row());
        let workspace_name = TextInput::new(&mut engine, theme::role::row());
        let email_address = TextInput::new(&mut engine, theme::role::row());
        // Secret, with the shared eye that shows what was typed.
        let email_password = TextInput::new(&mut engine, theme::role::row())
            .secret()
            .with_end_icon(koru_ui::EndIcon::Reveal);
        let email_code = TextInput::new(&mut engine, theme::role::row());
        let new_password = TextInput::new(&mut engine, theme::role::row());
        let confirm_password = TextInput::new(&mut engine, theme::role::row());
        Self {
            created_profile_retry: None,
            account_state: AccountState::default(),
            browser_signin: None,
            browser_apple: false,
            account_forms: Default::default(),
            language: koru_i18n::Lang::for_system(
                koru_window::system_locale::system_locale().as_deref(),
            ),
            viewport: (1000.0, 680.0),
            engine,
            screen: Screen::first(),
            account_profiles: Vec::new(),
            account_profile_sources: Vec::new(),
            account_surface: None,
            selected_existing: None,
            recovery_deleted: Vec::new(),
            recovery_unassigned: Vec::new(),
            selection: SelectionDraft::default(),
            existing_profile_scroll: 0.0,
            existing_profile_scroll_drag: None,
            pointer: (-1.0, -1.0),
            enrollment_cancelling: false,
            email_address,
            email_password,
            email_code,
            new_password,
            confirm_password,
            owner_name: name,
            profile_name,
            profile_name_edited: false,
            purpose: None,
            profile_icon: None,
            profile_color: None,
            profile_appearance_customized: false,
            profile_appearance: None,
            connect_account: false,
            workspace_name,
            template: None,
            workspace_name_edited: false,
            workspace_icon: None,
            workspace_color: None,
            workspace_appearance_customized: false,
            workspace_appearance: None,
            custom_caption: false,
            embedded: false,
            embedded_visible_area: None,
            caption_hover: None,
            maximized: false,
            screen_shown: None,
            screen_instance: 0,
            focus: 0,
            hover: None,
            pending: None,
            local_creation_id: koru_core::ProfileId::new(),
            creation_conflict: false,
            creation_submission: None,
            creation_waiting: false,
            telemetry: Vec::new(),
            error: None,
            last_attempt: None,
            error_field: None,
            blink_origin: std::time::Instant::now(),
            caret_drawn: None,
            window_focused: true,
        }
    }
}

impl OnboardingView {
    pub fn accept_parked_account_result(
        &mut self,
        owner: u64,
        result: &koru_agent_wire::AccountOperationResult,
    ) {
        let Some(view) = self.account_forms.get_mut(&owner) else {
            return;
        };
        use koru_agent_wire::AccountOperationResult as Result;
        match result {
            Result::AccountCreationRequired => {
                let state = view.account_state.clone();
                view.account_creation_required(&state);
            }
            Result::SignedIn | Result::SignedOut => view.account_creation_finished(),
            Result::Cancelled => view.account_cancelled(),
            Result::Profiles { profiles } => view.show_account_profiles(profiles.clone()),
            Result::ProfileChoices {
                display_name,
                choices,
            } => view.show_account_profile_choices(display_name, choices.clone()),
            Result::ProfilesSelected { profiles } => view.profiles_selected(profiles.clone()),
            Result::Failed { code } => {
                let mut state = view.account_state.clone();
                state.stage = koru_agent_wire::AccountStage::Failed;
                state.failure_code = Some(code.clone());
                view.show_account_state(&state);
            }
            _ => {}
        }
    }
    pub fn connection_paused_scoped(
        &mut self,
        owner: u64,
        profile: koru_core::ProfileId,
        retry: koru_agent_wire::AccountCommand,
    ) {
        if self.account_scope() == owner {
            self.connection_paused(profile, retry);
        } else if let Some(view) = self.account_forms.get_mut(&owner) {
            view.connection_paused(profile, retry);
        }
    }
    pub fn connection_paused(
        &mut self,
        profile: koru_core::ProfileId,
        retry: koru_agent_wire::AccountCommand,
    ) {
        self.created_profile_retry = Some((profile, retry));
        self.account_surface = Some(crate::account_ui::profile_connection_pending(self.language));
        self.screen = Screen::AccountStatus;
        self.focus = 0;
    }
    pub fn account_scope(&self) -> u64 {
        self.account_state.active_scope
    }
    pub fn account_generation(&self) -> u64 {
        self.browser_generation()
            .unwrap_or(self.account_state.generation)
    }
    /// Retain the actual unfinished form, including its local text inputs, when
    /// the person selects a different held account. This is presentation state.
    pub fn select_account_context(&mut self, state: &AccountState) -> bool {
        if self.account_state.active_scope == state.active_scope {
            self.observe_account_projection(state);
            return false;
        }
        let old_scope = self.account_state.active_scope;
        let mut forms = std::mem::take(&mut self.account_forms);
        let mut next = forms
            .remove(&state.active_scope)
            .unwrap_or_else(|| Box::new(Self::default()));
        next.viewport = self.viewport;
        next.custom_caption = self.custom_caption;
        next.embedded = self.embedded;
        next.embedded_visible_area = self.embedded_visible_area;
        next.language = self.language;
        std::mem::swap(self, &mut next);
        forms.insert(old_scope, next);
        self.account_forms = forms;
        self.observe_account_projection(state);
        true
    }
    pub fn set_viewport(&mut self, width: f32, height: f32) {
        self.viewport = (width.max(1.0), height.max(1.0));
        self.clamp_existing_profile_scroll();
    }

    /// Embed the form in a host page that already frames it (the Profile
    /// manager's *New profile*): the welcome's presentation pane is left out,
    /// and the column lines up with the page's head instead of centring.
    pub fn set_embedded(&mut self, embedded: bool) {
        self.embedded = embedded;
    }

    pub fn set_embedded_visible_area(&mut self, area: Rect) {
        self.embedded_visible_area = Some(area);
    }

    fn appearance_area(&self) -> Rect {
        self.embedded_visible_area
            .filter(|_| self.embedded)
            .unwrap_or(Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1))
    }

    pub fn set_custom_caption(&mut self, enabled: bool) {
        self.custom_caption = enabled;
        if !enabled {
            self.caption_hover = None;
        }
    }

    pub fn set_maximized(&mut self, maximized: bool) -> bool {
        let changed = self.maximized != maximized;
        self.maximized = maximized;
        changed
    }

    pub fn caption_action_at(&self, x: f32, y: f32) -> Option<koru_ui::caption::Action> {
        self.custom_caption
            .then(|| koru_ui::caption::direct_hit(self.caption_rect(), x, y))?
    }

    pub fn is_caption_drag_area(&self, x: f32, y: f32) -> bool {
        self.custom_caption
            && self.caption_rect().contains(x, y)
            && self.caption_action_at(x, y).is_none()
    }

    pub fn take_action(&mut self) -> Option<Action> {
        self.pending.take()
    }

    pub fn account_submission(&self) -> Option<crate::account_ui::CommandSubmissionId> {
        self.creation_submission
    }

    pub fn account_creation_finished(&mut self) {
        self.browser_signin = None;
        self.creation_submission = None;
        self.creation_waiting = false;
        // Profile discovery follows the authoritative sign-in result. Keep a
        // truthful progress surface during that separate asynchronous request.
        self.account_surface = Some(crate::account_ui::project(
            &AccountState {
                stage: koru_agent_wire::AccountStage::Exchanging,
                ..Default::default()
            },
            None,
        ));
    }

    fn can_reopen_account_creation(&self) -> bool {
        self.account_surface
            .as_ref()
            .is_some_and(|surface| surface.kind == crate::account_ui::SurfaceKind::AccountCreation)
            || self.creation_submission.is_some()
            || !self.account_profiles.is_empty()
    }

    fn reopen_account_creation(&mut self) -> bool {
        if !self.can_reopen_account_creation() {
            return false;
        }
        self.screen = if self.account_profiles.is_empty() {
            Screen::AccountStatus
        } else {
            Screen::ExistingProfiles
        };
        self.focus = 0;
        true
    }

    fn resume_account_rect(&mut self) -> Rect {
        let c = self.content();
        let label = koru_i18n::t(self.language, koru_i18n::Key::AccountCreationResume);
        Rect::new(c.x, c.y, button_width(&mut self.engine, label), 36.0)
    }

    pub fn account_command_failed(&mut self, failure: crate::account_ui::CommandFailure) -> bool {
        let mut parked_changed = false;
        for view in self.account_forms.values_mut() {
            parked_changed |= view.account_command_failed(failure);
        }
        if self.browser_command_failed(failure) {
            return true;
        }
        if self.selection.failed(failure) {
            self.selection.notice = Some(if failure.uncertain {
                koru_i18n::Key::ProfileChooserUncertain
            } else {
                koru_i18n::Key::ProfileChooserRefused
            });
            if !self.keep_local_selection_form() {
                self.screen = Screen::ExistingProfiles;
                self.focus = 0;
            }
            return true;
        }
        if self.creation_submission != Some(failure.submission) || !self.creation_waiting {
            return parked_changed;
        }
        self.creation_waiting = false;
        self.account_surface = Some(crate::account_ui::command_failure_surface());
        self.screen = Screen::AccountStatus;
        self.focus = 0;
        true
    }

    pub fn account_creation_required(&mut self, state: &AccountState) {
        self.browser_signin = None;
        self.creation_submission = None;
        self.creation_waiting = false;
        let mut state = state.clone();
        state.stage = koru_agent_wire::AccountStage::AwaitingAccountCreation;
        state.failure_code = None;
        self.show_account_state(&state);
    }

    pub fn take_telemetry(&mut self) -> Vec<Telemetry> {
        std::mem::take(&mut self.telemetry)
    }

    pub fn account_cancelled(&mut self) {
        if self.selection.submitted.is_some() || self.enrollment_cancelling {
            self.selection.settled();
            self.selection.submitted = None;
            self.enrollment_cancelling = false;
            if !self.keep_local_selection_form() {
                self.error = None;
            }
            return;
        }
        self.account_surface = None;
        self.browser_signin = None;
        self.creation_submission = None;
        self.creation_waiting = false;
        self.enrollment_cancelling = false;
        self.screen = Screen::first();
        self.focus = 0;
        self.error = None;
    }

    pub fn show_account_profiles(&mut self, profiles: Vec<AccountProfileRow>) {
        let choices = profiles
            .into_iter()
            .map(|profile| AccountProfileChoice {
                profile,
                available_on_this_device: false,
                available_on_another_device: true,
            })
            .collect();
        self.show_account_profile_choices("", choices);
    }

    pub fn show_account_profile_choices(
        &mut self,
        owner_display_name: &str,
        choices: Vec<AccountProfileChoice>,
    ) {
        let keep_local_form = !self.account_profiles.is_empty()
            && matches!(self.screen, Screen::Profile | Screen::Workspace);
        self.account_creation_finished();
        if !keep_local_form && !owner_display_name.trim().is_empty() {
            self.owner_name
                .set_text(&mut self.engine, owner_display_name.trim());
            self.update_profile_name_suggestion();
        }
        if choices.is_empty() {
            self.account_profiles.clear();
            self.account_profile_sources.clear();
            self.selection.reconcile(&Default::default());
            if keep_local_form {
                return;
            }
            if self.selection_has_unfinished() {
                self.screen = Screen::ExistingProfiles;
            } else {
                self.show_connected_profile();
            }
            return;
        }
        let (profiles, sources): (Vec<_>, Vec<_>) = choices
            .into_iter()
            .filter(|choice| self.choice_authorized(&choice.profile))
            .map(|choice| {
                let source = if choice.available_on_this_device {
                    ProfileSource::Local
                } else if choice.available_on_another_device {
                    ProfileSource::TrustedDevice
                } else {
                    ProfileSource::Unavailable
                };
                (choice.profile, source)
            })
            .unzip();
        if profiles.is_empty() {
            self.account_profiles.clear();
            self.account_profile_sources.clear();
            self.selection.reconcile(&Default::default());
            if keep_local_form {
                return;
            }
            if self.selection_has_unfinished() {
                self.screen = Screen::ExistingProfiles;
            } else {
                self.show_connected_profile();
            }
            return;
        }
        // Even one row is a choice. Showing and selecting it explicitly keeps
        // the person aware of which Profile another device will send.
        let selected = self.selected_account_profile_id().map(str::to_owned);
        self.existing_profile_scroll = 0.0;
        self.existing_profile_scroll_drag = None;
        self.account_profiles = profiles;
        self.account_profile_sources = sources;
        self.selected_existing = selected.and_then(|id| {
            self.account_profiles
                .iter()
                .position(|row| row.profile_id == id)
        });
        self.prune_choice_grants();
        if !keep_local_form {
            self.screen = Screen::ExistingProfiles;
            self.focus = 0;
        }
        if !self.selection.locked() {
            self.enrollment_cancelling = false;
            self.selection.notice = None;
            if !keep_local_form {
                self.error = None;
            }
        }
    }

    pub fn restore_failed(&mut self, message: impl Into<String>) {
        self.screen = Screen::ExistingProfiles;
        self.focus = 0;
        self.error = Some(message.into());
    }

    pub fn profile_selected(&mut self) {
        self.enrollment_cancelling = false;
        self.selection.settled();
        if !self.keep_local_selection_form() {
            self.focus = 0;
            self.error = None;
        }
    }

    pub fn profiles_selected(&mut self, profiles: Vec<koru_agent_wire::AccountProfileEnrollment>) {
        if self.enrollment_cancelling {
            self.account_state.enrollments = profiles;
            return;
        }
        // This targeted acknowledgement means the requested Profile manifests
        // are durably local. Peer enrollment and document arrival continue in
        // the background; unsolicited progress is not this acknowledgement.
        for (row, source) in self
            .account_profiles
            .iter()
            .zip(&mut self.account_profile_sources)
        {
            if self
                .selection
                .submitted
                .as_ref()
                .is_some_and(|ids| ids.contains(&row.profile_id))
                && profiles
                    .iter()
                    .any(|profile| profile.profile_id == row.profile_id)
            {
                *source = ProfileSource::Local;
            }
        }
        self.account_state.enrollments = profiles;
        self.profile_selected();
        self.open_first_connected_selection();
    }

    fn open_first_connected_selection(&mut self) {
        if self.screen != Screen::ExistingProfiles
            || self.pending.is_some()
            || self.enrollment_cancelling
        {
            return;
        }
        let Some(selected) = self.selection.submitted.as_ref() else {
            return;
        };
        let first = self
            .account_profiles
            .iter()
            .enumerate()
            .find(|(_, row)| selected.contains(&row.profile_id));
        let Some((index, first)) = first else {
            return;
        };
        if self.account_profile_sources.get(index) != Some(&ProfileSource::Local) {
            return;
        }
        self.pending = Some(Action::RestoreLocalProfile {
            profile_id: first.profile_id.clone(),
        });
        self.selected_existing = Some(index);
        self.screen = Screen::RestoringProfile;
    }

    pub fn selected_account_profile_id(&self) -> Option<&str> {
        self.selected_existing
            .and_then(|index| self.account_profiles.get(index))
            .map(|profile| profile.profile_id.as_str())
    }

    /// Keep stable Agent state as an inline native page. Backend prose never
    /// enters the view; the projection owns all person-facing copy and actions.
    pub fn show_account_state(&mut self, state: &AccountState) {
        self.observe_account_projection(state);
        // **The email journey answers its own failures where it can**, and
        // owns the two stages that are its own screens. Both are checked
        // before the general projection, which would otherwise replace a
        // mistyped digit with an error page.
        if self.settle_email_failure(state) {
            return;
        }
        match state.stage {
            koru_agent_wire::AccountStage::AwaitingEmailCode => {
                if self.screen != Screen::EmailCode {
                    self.show_email_code();
                }
                self.error = None;
                return;
            }
            koru_agent_wire::AccountStage::AwaitingEmailPassword => {
                if self.screen != Screen::EmailPassword {
                    self.show_email_password();
                }
                return;
            }
            _ => {}
        }
        self.open_first_connected_selection();
        if self.screen == Screen::RestoringProfile {
            return;
        }
        if (self.selection.submitted.is_some() || !self.account_profiles.is_empty())
            && matches!(self.screen, Screen::Profile | Screen::Workspace)
            && matches!(
                state.stage,
                koru_agent_wire::AccountStage::SignedIn
                    | koru_agent_wire::AccountStage::Refreshing
                    | koru_agent_wire::AccountStage::Unavailable
                    | koru_agent_wire::AccountStage::Failed
            )
        {
            return;
        }
        if !self.account_profiles.is_empty()
            && self.screen == Screen::ExistingProfiles
            && matches!(
                state.stage,
                koru_agent_wire::AccountStage::SignedIn
                    | koru_agent_wire::AccountStage::Refreshing
                    | koru_agent_wire::AccountStage::Failed
                    | koru_agent_wire::AccountStage::Unavailable
            )
        {
            self.screen = Screen::ExistingProfiles;
            return;
        }
        if matches!(
            state.stage,
            koru_agent_wire::AccountStage::SignedIn
                | koru_agent_wire::AccountStage::SignedOut
                | koru_agent_wire::AccountStage::Cancelled
        ) {
            self.creation_submission = None;
            self.creation_waiting = false;
        }
        if matches!(
            state.stage,
            koru_agent_wire::AccountStage::Failed | koru_agent_wire::AccountStage::Unavailable
        ) {
            self.creation_waiting = false;
        }
        let profile = self.selected_profile_name().to_owned();
        let projected =
            crate::account_ui::state_for_profile(state, self.selected_account_profile_id());
        self.account_surface = Some(crate::account_ui::project_for_lang(
            &projected,
            Some(&profile),
            self.language,
        ));
        if self.creation_submission.is_some()
            && state.stage == koru_agent_wire::AccountStage::AwaitingAccountCreation
        {
            if self.creation_waiting {
                self.account_surface
                    .as_mut()
                    .expect("projected")
                    .actions
                    .clear();
            } else {
                self.account_surface = Some(crate::account_ui::command_failure_surface());
            }
        }
        self.screen = Screen::AccountStatus;
        self.focus = 0;
        self.error = None;
    }
    pub fn show_profile(&mut self) {
        self.connect_account = false;
        self.screen = Screen::Profile;
        self.focus = 0;
        self.error = None;
    }

    fn show_entry(&mut self) {
        self.profile_appearance = None;
        self.screen = Screen::first();
        self.focus = 0;
        self.error = None;
    }

    /// Enter the same Google-connected Profile setup from an already-open
    /// application. Persistence still leaves through the typed action seam.
    pub fn begin_connected_profile_creation(&mut self) {
        self.connect_account = true;
        self.screen = Screen::Profile;
        self.focus = 0;
        self.error = None;
    }

    fn show_connected_profile(&mut self) {
        self.begin_connected_profile_creation();
    }

    /// A Store refusal before Profile acknowledgement is safe to retry from
    /// the Workspace step. The form values remain intact.
    pub fn creation_failed(&mut self, message: impl Into<String>) {
        self.screen = Screen::Workspace;
        self.focus = 0;
        self.error = Some(message.into());
    }

    pub fn creation_conflict(&mut self) {
        self.creation_conflict = true;
        self.creation_failed(koru_i18n::t(
            self.language,
            koru_i18n::Key::ProfileCreationExists,
        ));
    }

    fn workspace_action_label(&self) -> &'static str {
        if self.screen == Screen::Finishing {
            "Finishing..."
        } else if self.creation_conflict {
            koru_i18n::t(self.language, koru_i18n::Key::ProfileCreationManage)
        } else {
            "Create workspace"
        }
    }

    /// Account errors return to the entry choice; they are unrelated to local
    /// Profile creation and must not skip the offline path.
    pub fn account_failed(&mut self, message: impl Into<String>) {
        self.screen = Screen::first();
        self.focus = 0;
        self.error = Some(message.into());
        self.error_field = None;
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        let area = self.appearance_area();
        if let Some(appearance) = self.profile_appearance.as_mut() {
            return appearance.mouse_move(
                &mut self.engine,
                (area.w, area.h),
                x - area.x,
                y - area.y,
            );
        }
        if let Some(appearance) = self.workspace_appearance.as_mut() {
            return appearance.mouse_move(
                &mut self.engine,
                (area.w, area.h),
                x - area.x,
                y - area.y,
            );
        }
        let caption_hover = self.caption_action_at(x, y);
        let caption_changed = caption_hover != self.caption_hover;
        self.caption_hover = caption_hover;
        if let Some(grab) = self.existing_profile_scroll_drag {
            if let Some(bar) = self.existing_profile_scrollbar() {
                self.existing_profile_scroll = bar.scroll_for_pointer(y, grab);
            }
            return true;
        }
        let dragged = match self.screen {
            Screen::Profile => {
                let owner = self.owner_name_rect();
                let profile = self.profile_name_rect();
                self.owner_name.drag(&mut self.engine, owner, x)
                    | self.profile_name.drag(&mut self.engine, profile, x)
            }
            Screen::Workspace => {
                let field = self.workspace_name_rect();
                self.workspace_name.drag(&mut self.engine, field, x)
            }
            _ => false,
        };
        let next = self.hit(x, y);
        let changed = next != self.hover;
        self.hover = next;
        caption_changed || dragged || changed
    }

    pub fn set_window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
        self.restart_caret_blink();
    }

    fn restart_caret_blink(&mut self) {
        self.blink_origin = std::time::Instant::now();
    }

    /// Whether the focused control is a field with a caret in it.
    fn caret_field_focused(&self) -> bool {
        matches!(
            self.controls().get(self.focus),
            Some(
                Hit::EmailAddress
                    | Hit::EmailPassword
                    | Hit::NewPassword
                    | Hit::ConfirmPassword
                    | Hit::Name
                    | Hit::ProfileName
            )
        )
    }

    fn caret_blinks(&self) -> bool {
        self.window_focused && self.caret_field_focused()
    }

    /// The caret's phase this frame: solid whenever it is not blinking.
    fn caret_on(&self) -> bool {
        !self.caret_blinks() || crate::app::caret_phase_on(self.blink_origin)
    }

    /// When the caret next changes phase, while one is blinking.
    pub fn caret_blink_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.caret_blinks()
            .then(|| crate::app::next_caret_flip(self.blink_origin, now))
    }

    /// The phase on screen is not the phase it should be.
    pub fn caret_needs_frame(&self) -> bool {
        self.caret_drawn
            .is_some_and(|drawn| drawn != self.caret_on())
    }

    pub fn mouse_down(&mut self, x: f32, y: f32) -> bool {
        self.restart_caret_blink();
        self.pointer = (x, y);
        let area = self.appearance_area();
        if self.profile_appearance.is_some() {
            let outcome = self.profile_appearance.as_mut().map(|appearance| {
                appearance.mouse_down(&mut self.engine, (area.w, area.h), x - area.x, y - area.y)
            });
            return outcome.is_some_and(|outcome| self.settle_profile_appearance(outcome));
        }
        if self.workspace_appearance.is_some() {
            let outcome = self.workspace_appearance.as_mut().map(|appearance| {
                appearance.mouse_down(&mut self.engine, (area.w, area.h), x - area.x, y - area.y)
            });
            return outcome.is_some_and(|outcome| self.settle_workspace_appearance(outcome));
        }
        if let Some(bar) = self.existing_profile_scrollbar()
            && bar.in_gutter(x, y)
        {
            let grab = if bar.on_thumb(x, y) {
                y - bar.thumb_top
            } else {
                bar.thumb_h * 0.5
            };
            self.existing_profile_scroll = bar.scroll_for_pointer(y, grab);
            self.existing_profile_scroll_drag = Some(grab);
            return true;
        }
        let field_clicked = match self.screen {
            Screen::Entry if !self.accounts_offered() => None,
            Screen::Entry => {
                let layout = self.welcome_layout();
                if self
                    .email_address
                    .click(&mut self.engine, layout.address, x, y)
                {
                    self.controls()
                        .iter()
                        .position(|hit| *hit == Hit::EmailAddress)
                } else if self
                    .email_password
                    .click(&mut self.engine, layout.password, x, y)
                {
                    self.controls()
                        .iter()
                        .position(|hit| *hit == Hit::EmailPassword)
                } else {
                    None
                }
            }
            Screen::EmailCode => {
                let row = self.email_code_row();
                // One field wearing six boxes: a press anywhere on the row
                // puts the caret at the end, which is where the next digit
                // goes.
                row.contains(x, y).then(|| {
                    self.email_code.move_to_end(&mut self.engine);
                    0
                })
            }
            Screen::EmailPassword => {
                let new = self.new_password_rect();
                let confirm = self.confirm_password_rect();
                if self.new_password.click(&mut self.engine, new, x, y) {
                    self.controls()
                        .iter()
                        .position(|hit| *hit == Hit::NewPassword)
                } else if self.confirm_password.click(&mut self.engine, confirm, x, y) {
                    self.controls()
                        .iter()
                        .position(|hit| *hit == Hit::ConfirmPassword)
                } else {
                    None
                }
            }
            Screen::Profile => {
                let owner = self.owner_name_rect();
                let profile = self.profile_name_rect();
                if self.owner_name.click(&mut self.engine, owner, x, y) {
                    self.controls().iter().position(|hit| *hit == Hit::Name)
                } else if self.profile_name.click(&mut self.engine, profile, x, y) {
                    self.controls()
                        .iter()
                        .position(|hit| *hit == Hit::ProfileName)
                } else {
                    None
                }
            }
            Screen::Workspace => {
                let field = self.workspace_name_rect();
                self.workspace_name
                    .click(&mut self.engine, field, x, y)
                    .then_some(0)
            }
            _ => None,
        };
        if let Some(focus) = field_clicked {
            self.focus = focus;
            return true;
        }
        let Some(hit) = self.hit(x, y) else {
            return false;
        };
        self.activate(hit);
        true
    }

    pub fn mouse_up(&mut self) {
        if let Some(appearance) = self.profile_appearance.as_mut() {
            let outcome = appearance.mouse_up();
            self.settle_profile_appearance(outcome);
            return;
        }
        if let Some(appearance) = self.workspace_appearance.as_mut() {
            let outcome = appearance.mouse_up();
            self.settle_workspace_appearance(outcome);
            return;
        }
        self.owner_name.release();
        self.profile_name.release();
        self.workspace_name.release();
        self.email_address.release();
        self.email_password.release();
        self.email_code.release();
        self.new_password.release();
        self.confirm_password.release();
        self.existing_profile_scroll_drag = None;
    }

    pub fn scroll_by(&mut self, delta: winit::event::MouseScrollDelta) -> bool {
        if let Some(appearance) = self.profile_appearance.as_mut() {
            appearance.scroll_by(delta);
            return true;
        }
        if let Some(appearance) = self.workspace_appearance.as_mut() {
            appearance.scroll_by(delta);
            return true;
        }
        if self.screen != Screen::ExistingProfiles
            || !self
                .existing_profiles_viewport()
                .contains(self.pointer.0, self.pointer.1)
        {
            return false;
        }
        let amount = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => -y * EXISTING_ROW_PITCH * 2.0,
            winit::event::MouseScrollDelta::PixelDelta(point) => -point.y as f32,
        };
        let before = self.existing_profile_scroll;
        self.existing_profile_scroll += amount;
        self.clamp_existing_profile_scroll();
        (before - self.existing_profile_scroll).abs() > f32::EPSILON
    }

    pub fn cursor_icon(&self) -> CursorIcon {
        if let Some(appearance) = &self.profile_appearance {
            appearance.cursor()
        } else if let Some(appearance) = &self.workspace_appearance {
            appearance.cursor()
        } else if matches!(
            self.hover,
            Some(
                Hit::Name
                    | Hit::ProfileName
                    | Hit::EmailAddress
                    | Hit::EmailPassword
                    | Hit::EmailCode
                    | Hit::NewPassword
                    | Hit::ConfirmPassword
            )
        ) {
            CursorIcon::Text
        } else if matches!(
            self.hover,
            Some(Hit::ProfileAppearance | Hit::WorkspaceAppearance | Hit::Template(_))
        ) {
            koru_ui::cursor::PRESS
        } else {
            CursorIcon::Default
        }
    }

    pub fn key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        self.restart_caret_blink();
        if let Some(appearance) = self.profile_appearance.as_mut() {
            let outcome = appearance.key(&mut self.engine, key, text, mods);
            return self.settle_profile_appearance(outcome);
        }
        if let Some(appearance) = self.workspace_appearance.as_mut() {
            let outcome = appearance.key(&mut self.engine, key, text, mods);
            return self.settle_workspace_appearance(outcome);
        }
        let focused = self.controls().get(self.focus).copied();
        if let Some(focused) = focused {
            let typed = match (self.screen, focused) {
                (Screen::Profile, Hit::Name) => {
                    let before = self.owner_name.value();
                    let typed = self.owner_name.key(&mut self.engine, key, text, mods);
                    if typed == Typed::Handled && self.owner_name.value() != before {
                        self.update_profile_name_suggestion();
                    }
                    typed
                }
                (Screen::Profile, Hit::ProfileName) => {
                    let before = self.profile_name.value();
                    let typed = self.profile_name.key(&mut self.engine, key, text, mods);
                    if typed == Typed::Handled && self.profile_name.value() != before {
                        self.profile_name_edited = true;
                    }
                    typed
                }
                (Screen::Entry, Hit::EmailAddress) => {
                    self.email_address.key(&mut self.engine, key, text, mods)
                }
                (Screen::Entry, Hit::EmailPassword) => {
                    self.email_password.key(&mut self.engine, key, text, mods)
                }
                (Screen::EmailCode, Hit::EmailCode) => self.email_code_typed(key, text, mods),
                (Screen::EmailPassword, Hit::NewPassword) => {
                    self.new_password.key(&mut self.engine, key, text, mods)
                }
                (Screen::EmailPassword, Hit::ConfirmPassword) => {
                    self.confirm_password.key(&mut self.engine, key, text, mods)
                }
                (Screen::Workspace, Hit::Name) => {
                    let before = self.workspace_name.value();
                    let typed = self.workspace_name.key(&mut self.engine, key, text, mods);
                    if typed == Typed::Handled && self.workspace_name.value() != before {
                        // Typed text outranks the card suggestions; an empty
                        // field holds no text, so deleting it all hands the
                        // authority back to the cards — they are pickable again.
                        self.workspace_name_edited = !self.workspace_name.is_empty();
                    }
                    typed
                }
                _ => Typed::Passed,
            };
            if typed == Typed::Handled {
                return true;
            }
        }
        let controls = self.controls();
        match key {
            Key::Named(NamedKey::Tab) if mods.is_empty() => {
                self.focus = (self.focus + 1) % controls.len().max(1);
                self.keep_focused_existing_profile_visible(&controls);
                true
            }
            Key::Named(NamedKey::Tab) if mods == ModifiersState::SHIFT => {
                self.focus = self
                    .focus
                    .checked_sub(1)
                    .unwrap_or(controls.len().saturating_sub(1));
                self.keep_focused_existing_profile_visible(&controls);
                true
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) if mods.is_empty() => {
                self.focus = (self.focus + 1) % controls.len().max(1);
                self.keep_focused_existing_profile_visible(&controls);
                true
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) if mods.is_empty() => {
                self.focus = self
                    .focus
                    .checked_sub(1)
                    .unwrap_or(controls.len().saturating_sub(1));
                self.keep_focused_existing_profile_visible(&controls);
                true
            }
            Key::Named(NamedKey::Enter) if mods.is_empty() => {
                // Enter belongs to the form, not the field — the single-line
                // input passes it through for exactly this. On the name field
                // that completes a form, Enter is the primary action (its
                // completeness guard lives in `activate`); activating the
                // field hit would only refocus it and read as "Enter does
                // nothing" once everything is filled.
                match (self.screen, focused) {
                    (Screen::Profile, Some(Hit::ProfileName)) => self.activate(Hit::Primary),
                    (Screen::Workspace, Some(Hit::Name)) => self.activate(Hit::Primary),
                    // Enter anywhere in the email form submits it, which is
                    // what a browser does and what a password manager's fill
                    // ends with.
                    (Screen::Entry, Some(Hit::EmailAddress | Hit::EmailPassword)) => {
                        self.activate(Hit::ContinueWithEmail)
                    }
                    (Screen::EmailCode, Some(Hit::EmailCode))
                    | (Screen::EmailPassword, Some(Hit::NewPassword | Hit::ConfirmPassword)) => {
                        self.activate(Hit::Primary)
                    }
                    _ => {
                        if let Some(hit) = controls.get(self.focus).copied() {
                            self.activate(hit);
                        }
                    }
                }
                true
            }
            Key::Named(NamedKey::Escape)
                if matches!(self.screen, Screen::Browser | Screen::EmailWorking) =>
            {
                self.cancel_browser_signin();
                true
            }
            // Not a cancellation: nothing is in flight on these two, and the
            // way back is the welcome screen with the journey dropped.
            Key::Named(NamedKey::Escape)
                if matches!(self.screen, Screen::EmailCode | Screen::EmailPassword) =>
            {
                self.abandon_email_journey(None);
                true
            }
            key if (matches!(key, Key::Named(NamedKey::Space))
                || matches!(key, Key::Character(value) if value.as_str() == " "))
                && self.screen == Screen::ExistingProfiles
                && mods.is_empty() =>
            {
                if let Some(hit) = controls.get(self.focus).copied() {
                    self.activate(hit);
                }
                true
            }
            Key::Named(NamedKey::Escape) if self.screen == Screen::Recovery => {
                self.show_profile();
                true
            }
            Key::Named(NamedKey::Escape) if self.screen == Screen::ExistingProfiles => {
                if self.selection_has_unfinished() {
                    self.activate_selection(Hit::CancelSelection);
                } else {
                    self.show_profile();
                }
                true
            }
            Key::Named(NamedKey::Escape) if self.screen == Screen::AccountStatus => {
                if let Some(index) = self.account_surface.as_ref().and_then(|surface| {
                    surface.actions.iter().position(|action| {
                        matches!(
                            action,
                            crate::account_ui::Action::Cancel
                                | crate::account_ui::Action::CancelAccountCreation
                        )
                    })
                }) {
                    self.activate_account_action(index);
                }
                true
            }
            _ => false,
        }
    }

    pub fn draw(&mut self, canvas: &mut Scene, root: Affine) {
        self.caret_drawn = Some(self.caret_on());
        // Entry's `welcome::draw` paints the backdrop itself; painting it here
        // as well drew the welcome split twice — the inspector showed two
        // WelcomeSidebar containers, and the sidebar paid double overdraw.
        if self.screen != Screen::Entry {
            welcome::draw_backdrop(
                canvas,
                root,
                &mut self.engine,
                self.viewport.0,
                self.viewport.1,
                !self.embedded,
            );
        }
        // One inspector node per screen, so a capture reads as the form the
        // person is looking at; the screens' own parts nest underneath it.
        // The node bounds the screen's content column, not the viewport: the
        // pick takes the *last* record containing the point, and a
        // viewport-wide node — drawn after the backdrop — would shadow the
        // sidebar's parts on every screen but Entry. Entry is the welcome
        // surface itself, so its parts draw inside the node either way.
        let screen_bounds = if self.screen == Screen::Entry {
            scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1)
        } else {
            let c = self.content();
            scene::rect(c.x, c.y, c.w, c.h)
        };
        {
            let _screen = inspect::scope(
                self.screen.inspection_name(),
                "screen",
                root,
                screen_bounds,
                |_| {},
            );
            match self.screen {
                Screen::Entry => self.draw_entry(canvas, root),
                Screen::Browser => self.draw_waiting(
                    canvas,
                    root,
                    "Finish signing in in your browser",
                    if self.browser_apple {
                        "Waiting for Apple..."
                    } else {
                        "Waiting for Google..."
                    },
                ),
                Screen::EmailWorking => {
                    self.draw_waiting(canvas, root, "One moment", "Checking that address...")
                }
                Screen::EmailCode => self.draw_email_code(canvas, root),
                Screen::EmailPassword => self.draw_email_password(canvas, root),
                Screen::AccountStatus => self.draw_account_status(canvas, root),
                Screen::ExistingProfiles => self.draw_existing_profiles(canvas, root),
                Screen::RestoringProfile => self.draw_restoring_profile(canvas, root),
                Screen::Recovery => self.draw_recovery(canvas, root),
                Screen::Profile => self.draw_profile(canvas, root),
                Screen::Workspace | Screen::Finishing => self.draw_workspace(canvas, root),
            }
            let area = self.appearance_area();
            let overlay = root * Affine::translate((f64::from(area.x), f64::from(area.y)));
            if let Some(mut appearance) = self.workspace_appearance.take() {
                appearance.draw(canvas, overlay, &mut self.engine, (area.w, area.h));
                self.workspace_appearance = Some(appearance);
            }
            if let Some(mut appearance) = self.profile_appearance.take() {
                appearance.draw(canvas, overlay, &mut self.engine, (area.w, area.h));
                self.profile_appearance = Some(appearance);
            }
        }
        // Window furniture, not screen content: the caption hangs directly
        // under the window root, a sibling of the screen — and, painted last,
        // stays on top of anything a form pops over it.
        self.draw_caption(canvas, root);
    }

    fn caption_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.viewport.0, theme::CAPTION_H)
    }
    fn draw_caption(&mut self, canvas: &mut Scene, root: Affine) {
        if self.custom_caption {
            let caption = self.caption_rect();
            let _caption = inspect::scope(
                "Caption",
                "container",
                root,
                scene::rect(caption.x, caption.y, caption.w, caption.h),
                |_| {},
            );
            koru_ui::caption::draw_direct_controls(
                canvas,
                root,
                &mut self.engine,
                caption,
                koru_ui::caption::State {
                    hovered: self.caption_hover,
                    maximized: self.maximized,
                    ..Default::default()
                },
            );
        }
    }

    /// Whether this build has a Koru Cloud to sign in to. The Agent's state
    /// says so — `AccountStage::NotConfigured`, from the one
    /// `koru_env::cloud_configured` fact — and the shell seeds it before the
    /// first frame. Without one the welcome offers no sign-in at all.
    fn accounts_offered(&self) -> bool {
        self.account_state.stage != koru_agent_wire::AccountStage::NotConfigured
    }

    /// What the welcome says in place of the sign-in choices when there is no
    /// Koru Cloud, in the reader's language.
    fn cloud_note(&self) -> welcome::CloudNote<'static> {
        welcome::CloudNote {
            title: koru_i18n::t(self.language, koru_i18n::Key::CloudNotConfigured),
            body: koru_i18n::t(self.language, koru_i18n::Key::CloudNotConfiguredBody),
        }
    }

    fn welcome_layout(&mut self) -> WelcomeLayout {
        let top = if self.custom_caption {
            theme::CAPTION_H
        } else {
            0.0
        };
        if !self.accounts_offered() {
            let note = self.cloud_note();
            return WelcomeLayout::without_accounts_in(
                &mut self.engine,
                self.viewport.0,
                self.viewport.1,
                top,
                note,
                !self.embedded,
            );
        }
        // The error travels with the geometry: the welcome screen draws it and
        // publishes it at the rectangle measured here, so both read one rect.
        let failure = self.welcome_failure();
        WelcomeLayout::new_in(
            &mut self.engine,
            self.viewport.0,
            self.viewport.1,
            top,
            failure.as_ref().map(EntryFailure::borrow),
            !self.embedded,
        )
    }

    /// The welcome screen's failure, placed: under the field it is about, or
    /// under the attempt that produced it.
    ///
    /// Owned, because the welcome surface is measured and drawn with this
    /// view's text engine borrowed mutably beside it.
    fn welcome_failure(&self) -> Option<EntryFailure> {
        let message = self.error.clone()?;
        let at = self.failure_at();
        Some(EntryFailure {
            at,
            message,
            retry: self.retry_target_for(at).is_some(),
        })
    }

    fn failure_at(&self) -> welcome::Action {
        self.error_field
            .or(self.last_attempt)
            .unwrap_or(welcome::Action::ContinueWithEmail)
    }

    /// What *Try again* presses. Only an attempt can be repeated, and the
    /// email form only while it still holds both halves: a journey that ended
    /// clears the password, and a link that does nothing is not offered.
    fn retry_target_for(&self, at: welcome::Action) -> Option<Hit> {
        match at {
            welcome::Action::Google => Some(Hit::Secondary),
            welcome::Action::Apple => Some(Hit::Apple),
            welcome::Action::ContinueWithEmail if self.email_form_ready() => {
                Some(Hit::ContinueWithEmail)
            }
            _ => None,
        }
    }

    fn retry_target(&self) -> Option<Hit> {
        self.error.as_ref()?;
        self.retry_target_for(self.failure_at())
    }

    fn content(&self) -> Rect {
        if self.embedded {
            // The host's head is already above; the column starts where it does.
            let width = self.viewport.0.min(MAX_W).max(240.0);
            return Rect::new(0.0, 0.0, width, self.viewport.1 - PAD);
        }
        let (_, right) = welcome::split(self.viewport.0, self.viewport.1);
        let width = (right.w - PAD * 2.0).min(MAX_W).max(240.0);
        Rect::new(
            right.x + (right.w - width) * 0.5,
            44.0,
            width,
            self.viewport.1 - 68.0,
        )
    }

    /// The host scrolls an embedded form instead of shrinking its fields into
    /// the action row. This uses the same field geometry as paint and input.
    pub fn embedded_content_height(&self) -> f32 {
        let field = match self.screen {
            Screen::Profile => self.profile_name_rect(),
            Screen::Workspace | Screen::Finishing => self.workspace_name_rect(),
            _ => return 600.0,
        };
        (field.y + field.h + 16.0 + profile_note_h(self.connect_account) + 24.0 + 64.0 + PAD)
            .max(600.0)
    }

    /// The currently focused local-creation control, for the host's existing
    /// scroll container to reveal after keyboard or accessibility navigation.
    pub fn focused_form_rect(&mut self) -> Option<Rect> {
        if self.profile_appearance.is_some() || self.workspace_appearance.is_some() {
            return None;
        }
        if !matches!(self.screen, Screen::Profile | Screen::Workspace) {
            return None;
        }
        let hit = *self.controls().get(self.focus)?;
        Some(match hit {
            Hit::Name if self.screen == Screen::Workspace => self.workspace_name_rect(),
            Hit::Name => self.owner_name_rect(),
            Hit::ProfileName => self.profile_name_rect(),
            Hit::ProfileAppearance => self.profile_icon_rect(),
            Hit::WorkspaceAppearance => self.workspace_icon_rect(),
            Hit::Purpose(purpose) => self.card_rect(
                PURPOSES
                    .iter()
                    .position(|candidate| *candidate == purpose)?,
                self.profile_card_metrics(),
            ),
            Hit::Template(index) => self.card_rect(index, self.workspace_card_metrics()),
            Hit::Primary | Hit::Back => {
                let label = if self.screen == Screen::Profile {
                    "Continue"
                } else {
                    self.workspace_action_label()
                };
                let (primary, back) = self.form_action_buttons(label);
                if hit == Hit::Primary { primary } else { back }
            }
            _ => return None,
        })
    }

    fn existing_profiles_viewport(&mut self) -> Rect {
        self.selection_layout().viewport
    }

    fn existing_profiles_content_height(&mut self) -> f32 {
        self.selection_layout().content_height
    }

    fn existing_profile_scrollbar(&mut self) -> Option<ScrollbarGeom> {
        if self.screen != Screen::ExistingProfiles {
            return None;
        }
        let layout = self.selection_layout();
        ScrollbarGeom::for_content(
            layout.viewport,
            layout.content_height,
            self.existing_profile_scroll,
        )
    }

    fn clamp_existing_profile_scroll(&mut self) {
        let area = self.existing_profiles_viewport();
        let max_scroll = (self.existing_profiles_content_height() - area.h).max(0.0);
        self.existing_profile_scroll = self.existing_profile_scroll.clamp(0.0, max_scroll);
    }

    fn keep_focused_existing_profile_visible(&mut self, controls: &[Hit]) {
        if self.screen != Screen::ExistingProfiles {
            return;
        }
        let Some(Hit::Existing(index) | Hit::OpenExisting(index) | Hit::RetryExisting(index)) =
            controls.get(self.focus).copied()
        else {
            return;
        };
        let layout = self.selection_layout();
        let row = layout.rows[index].rect;
        if row.y < layout.viewport.y {
            self.existing_profile_scroll -= layout.viewport.y - row.y;
        } else if row.y + row.h > layout.viewport.y + layout.viewport.h {
            self.existing_profile_scroll += row.y + row.h - layout.viewport.y - layout.viewport.h;
        }
        self.clamp_existing_profile_scroll();
    }

    fn controls(&self) -> Vec<Hit> {
        self.form_controls()
    }
    fn form_controls(&self) -> Vec<Hit> {
        match self.screen {
            // No Koru Cloud: nothing to sign in to, so the one way in and the
            // furniture.
            Screen::Entry if !self.accounts_offered() => {
                vec![Hit::Primary, Hit::Terms, Hit::Privacy]
            }
            // Reading order: the form first, then the other ways in, then the
            // furniture.
            Screen::Entry => {
                let mut controls = vec![
                    Hit::EmailAddress,
                    Hit::EmailPassword,
                    Hit::RevealPassword,
                    Hit::ContinueWithEmail,
                    Hit::Secondary,
                    Hit::Apple,
                ];
                // In the order it is drawn: the banner sits under the row it
                // is about and above the no-account choice.
                if self.retry_target().is_some() {
                    controls.push(Hit::Retry);
                }
                controls.extend([Hit::Primary, Hit::Terms, Hit::Privacy]);
                controls
            }
            Screen::Browser | Screen::EmailWorking => vec![Hit::Secondary],
            Screen::EmailCode => {
                let mut controls = vec![Hit::EmailCode];
                if self.email_code_complete() {
                    controls.push(Hit::Primary);
                }
                controls.extend([Hit::ResendCode, Hit::AnotherAddress]);
                controls
            }
            Screen::EmailPassword => {
                let mut controls = vec![Hit::NewPassword, Hit::ConfirmPassword];
                if self.new_password_ready() {
                    controls.push(Hit::Primary);
                }
                controls
            }
            Screen::AccountStatus => self
                .account_surface
                .as_ref()
                .map(|surface| (0..surface.actions.len()).map(Hit::AccountAction).collect())
                .unwrap_or_default(),
            Screen::ExistingProfiles => self.selection_controls(),
            Screen::RestoringProfile => Vec::new(),
            Screen::Recovery => self.recovery_controls(),
            Screen::Profile => {
                let mut controls = vec![Hit::Name];
                if self.can_reopen_account_creation() {
                    controls.insert(0, Hit::ResumeAccount);
                }
                controls.extend([
                    Hit::Purpose(Purpose::Work),
                    Hit::Purpose(Purpose::School),
                    Hit::Purpose(Purpose::Personal),
                ]);
                controls.extend([Hit::ProfileAppearance, Hit::ProfileName]);
                if self.profile_form_complete() {
                    controls.push(Hit::Primary);
                }
                if Self::profile_has_back() {
                    controls.push(Hit::Back);
                }
                if self.recovery_offered() {
                    controls.push(Hit::Recovery);
                }
                controls
            }
            Screen::Workspace => {
                let mut controls = vec![Hit::Name, Hit::WorkspaceAppearance];
                if !self.workspace_name_edited {
                    controls.extend((0..TEMPLATES.len()).map(Hit::Template));
                }
                if self.workspace_form_complete() {
                    controls.push(Hit::Primary);
                }
                controls.push(Hit::Back);
                controls
            }
            Screen::Finishing => Vec::new(),
        }
    }

    fn activate(&mut self, hit: Hit) {
        // A build with no Koru Cloud never offers these, and nothing reaches
        // them by another route either: no sign-in starts, so nothing is sent.
        if self.screen == Screen::Entry
            && !self.accounts_offered()
            && !matches!(hit, Hit::Primary | Hit::Terms | Hit::Privacy)
        {
            return;
        }
        if hit == Hit::ResumeAccount {
            self.reopen_account_creation();
            return;
        }
        match (self.screen, hit) {
            (Screen::Entry, Hit::Primary) => {
                self.telemetry.push(Telemetry::EntryOffline);
                self.show_profile();
            }
            (Screen::Entry, Hit::Secondary) => {
                if self.reopen_account_creation() {
                    return;
                }
                self.telemetry.push(Telemetry::EntryGoogle);
                self.begin_browser_signin(false);
            }
            (Screen::Entry, Hit::Apple) => {
                if self.reopen_account_creation() {
                    return;
                }
                self.telemetry.push(Telemetry::EntryApple);
                self.begin_browser_signin(true);
            }
            (Screen::Entry, Hit::RevealPassword) => {
                self.email_password.press_end_icon();
            }
            (Screen::Entry, Hit::Retry) => {
                if let Some(hit) = self.retry_target() {
                    self.activate(hit);
                }
            }
            (Screen::Entry, Hit::Terms) => {
                self.pending = Some(Action::OpenUri(welcome::TERMS_URL));
            }
            (Screen::Entry, Hit::Privacy) => {
                self.pending = Some(Action::OpenUri(welcome::PRIVACY_URL));
            }
            (Screen::Browser, Hit::Secondary) => self.cancel_browser_signin(),
            (Screen::AccountStatus, Hit::AccountAction(index)) => {
                self.activate_account_action(index);
            }
            (
                Screen::ExistingProfiles,
                hit @ (Hit::Existing(_)
                | Hit::OpenExisting(_)
                | Hit::RetryExisting(_)
                | Hit::RefreshProfiles
                | Hit::CancelSelection
                | Hit::Primary),
            ) => self.activate_selection(hit),
            (Screen::ExistingProfiles, Hit::CreateNew) => self.show_connected_profile(),
            (Screen::ExistingProfiles, Hit::ContinueOffline) => self.show_profile(),
            (Screen::Profile, Hit::Recovery) if self.recovery_offered() => self.open_recovery(),
            (Screen::Recovery, hit @ (Hit::Back | Hit::RestoreDeleted(_))) => {
                self.activate_recovery(hit)
            }
            (Screen::Profile, Hit::Name) => self.focus = 0,
            (Screen::Profile, Hit::ProfileName) => {
                if let Some(index) = self
                    .controls()
                    .iter()
                    .position(|hit| *hit == Hit::ProfileName)
                {
                    self.focus = index;
                }
            }
            (Screen::Profile, Hit::ProfileAppearance) => {
                let profile_name = self.profile_name();
                self.profile_appearance = Some(EmojiColorPanel::for_profile_draft(
                    &mut self.engine,
                    &profile_name,
                    self.profile_icon.clone(),
                    self.profile_color.clone(),
                ));
            }
            (Screen::Profile, Hit::Purpose(purpose)) => {
                self.telemetry.push(Telemetry::ProfilePurpose(purpose));
                self.purpose = Some(purpose);
                self.update_profile_name_suggestion();
                if !self.profile_appearance_customized {
                    self.profile_icon = Some(purpose_emoji(purpose).to_owned());
                    self.profile_color = None;
                }
            }
            (Screen::Profile, Hit::Back) => self.show_entry(),
            (Screen::Profile, Hit::Primary) if self.profile_form_complete() => {
                self.screen = Screen::Workspace;
                self.focus = 0;
            }
            (Screen::Workspace, Hit::Name) => self.focus = 0,
            (Screen::Workspace, Hit::WorkspaceAppearance) => {
                self.workspace_appearance = Some(EmojiColorPanel::for_draft(
                    &mut self.engine,
                    &self.workspace_name.value(),
                    self.workspace_icon.clone(),
                    self.workspace_color.clone(),
                ));
            }
            (Screen::Workspace, Hit::Template(index)) if !self.workspace_name_edited => {
                self.template = Some(index);
                // A template is a complete starting suggestion, including its
                // plain-language name. Keep following card choices until the
                // person types; from that moment their text is authoritative.
                self.workspace_name
                    .set_text(&mut self.engine, TEMPLATES[index].label);
                if !self.workspace_appearance_customized {
                    self.workspace_icon = Some(TEMPLATES[index].emoji.to_owned());
                    self.workspace_color = None;
                }
            }
            (Screen::Entry, Hit::ContinueWithEmail) if self.email_form_ready() => {
                let address = self.email_address.value();
                let password = self.email_password.value();
                self.begin_email_journey(address, password);
            }
            (Screen::EmailCode, Hit::Primary) if self.email_code_complete() => {
                self.error = None;
                self.pending = Some(Action::SubmitEmailCode {
                    code: self.email_code.value(),
                });
            }
            (Screen::EmailCode, Hit::ResendCode) => {
                self.error = None;
                self.pending = Some(Action::ResendEmailCode);
            }
            (Screen::EmailCode, Hit::AnotherAddress) => self.abandon_email_journey(None),
            (Screen::EmailPassword, Hit::Primary) if self.new_password_ready() => {
                self.error = None;
                self.pending = Some(Action::SubmitEmailPassword {
                    password: self.new_password.value(),
                });
            }
            (Screen::Workspace, Hit::Back) => {
                self.screen = Screen::Profile;
                self.focus = 0;
            }
            (Screen::Workspace, Hit::Primary) if self.creation_conflict => {
                self.screen = Screen::Finishing;
                self.pending = Some(Action::ManageProfiles);
            }
            (Screen::Workspace, Hit::Primary) if self.workspace_form_complete() => {
                let index = self.template.unwrap_or(EMPTY_TEMPLATE);
                self.screen = Screen::Finishing;
                self.pending = Some(Action::CreateLocalProfile {
                    id: self.local_creation_id,
                    owner_display_name: self.owner_name.value(),
                    name: self.profile_name(),
                    workspace_name: self.workspace_name.value(),
                    template: TEMPLATES[index].key.to_owned(),
                    workspace_icon: self.workspace_icon.clone(),
                    workspace_color: self.workspace_color.clone(),
                    profile_icon: self.profile_icon.clone(),
                    profile_color: self.profile_color.clone(),
                    connect_account: self.connect_account,
                });
            }
            _ => {}
        }
    }

    fn hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        match self.screen {
            Screen::Entry => {
                let layout = self.welcome_layout();
                match layout.hit(x, y) {
                    Some(welcome::Action::Address) => Some(Hit::EmailAddress),
                    Some(welcome::Action::Password)
                        if self
                            .email_password
                            .end_icon_rect(layout.password)
                            .is_some_and(|eye| eye.contains(x, y)) =>
                    {
                        Some(Hit::RevealPassword)
                    }
                    Some(welcome::Action::Password) => Some(Hit::EmailPassword),
                    Some(welcome::Action::ContinueWithEmail) => Some(Hit::ContinueWithEmail),
                    Some(welcome::Action::Google) => Some(Hit::Secondary),
                    Some(welcome::Action::Apple) => Some(Hit::Apple),
                    Some(welcome::Action::ContinueOffline) => Some(Hit::Primary),
                    Some(welcome::Action::Retry) => Some(Hit::Retry),
                    Some(welcome::Action::Terms) => Some(Hit::Terms),
                    Some(welcome::Action::Privacy) => Some(Hit::Privacy),
                    None => None,
                }
            }
            Screen::Browser | Screen::EmailWorking => self
                .button("Cancel", 64.0)
                .contains(x, y)
                .then_some(Hit::Secondary),
            Screen::EmailCode => {
                if self.email_code_row().contains(x, y) {
                    return Some(Hit::EmailCode);
                }
                let (resend, another) = self.email_code_actions();
                if resend.contains(x, y) {
                    return Some(Hit::ResendCode);
                }
                if another.contains(x, y) {
                    return Some(Hit::AnotherAddress);
                }
                (self.email_code_complete() && self.button("Continue", 64.0).contains(x, y))
                    .then_some(Hit::Primary)
            }
            Screen::EmailPassword => {
                if self.new_password_rect().contains(x, y) {
                    return Some(Hit::NewPassword);
                }
                if self.confirm_password_rect().contains(x, y) {
                    return Some(Hit::ConfirmPassword);
                }
                (self.new_password_ready()
                    && self
                        .button(email::CREATE_ACCOUNT_LABEL, 64.0)
                        .contains(x, y))
                .then_some(Hit::Primary)
            }
            Screen::AccountStatus => self.account_action_hit(x, y),
            Screen::ExistingProfiles => self.existing_profiles_hit(x, y),
            Screen::RestoringProfile => None,
            Screen::Recovery => self.recovery_hit(x, y),
            Screen::Profile => {
                if self.can_reopen_account_creation() && self.resume_account_rect().contains(x, y) {
                    return Some(Hit::ResumeAccount);
                }
                if self.recovery_offered() && self.recovery_link_rect().contains(x, y) {
                    return Some(Hit::Recovery);
                }
                if self.owner_name_rect().contains(x, y) {
                    return Some(Hit::Name);
                }
                if self.profile_name_rect().contains(x, y) {
                    return Some(Hit::ProfileName);
                }
                if self.profile_icon_rect().contains(x, y) {
                    return Some(Hit::ProfileAppearance);
                }
                let metrics = self.profile_card_metrics();
                for (index, purpose) in PURPOSES.into_iter().enumerate() {
                    if self.card_rect(index, metrics).contains(x, y) {
                        return Some(Hit::Purpose(purpose));
                    }
                }
                if self.profile_form_complete()
                    && self.form_action_buttons("Continue").0.contains(x, y)
                {
                    Some(Hit::Primary)
                } else if Self::profile_has_back()
                    && self.form_action_buttons("Continue").1.contains(x, y)
                {
                    Some(Hit::Back)
                } else {
                    None
                }
            }
            Screen::Workspace => self.workspace_hit(x, y),
            Screen::Finishing => None,
        }
    }

    fn existing_profiles_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        self.selection_hit(x, y)
    }
    fn workspace_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        if self.workspace_icon_rect().contains(x, y) {
            return Some(Hit::WorkspaceAppearance);
        }
        if self.workspace_name_rect().contains(x, y) {
            return Some(Hit::Name);
        }
        let metrics = self.workspace_card_metrics();
        if !self.workspace_name_edited {
            for index in 0..TEMPLATES.len() {
                if self.card_rect(index, metrics).contains(x, y) {
                    return Some(Hit::Template(index));
                }
            }
        }
        if self.workspace_form_complete()
            && self
                .form_action_buttons(self.workspace_action_label())
                .0
                .contains(x, y)
        {
            Some(Hit::Primary)
        } else if self
            .form_action_buttons(self.workspace_action_label())
            .1
            .contains(x, y)
        {
            Some(Hit::Back)
        } else {
            None
        }
    }

    fn settle_workspace_appearance(&mut self, outcome: AppearanceOutcome) -> bool {
        match outcome {
            AppearanceOutcome::Keep => true,
            AppearanceOutcome::Cancel => {
                self.workspace_appearance = None;
                true
            }
            AppearanceOutcome::Save { icon, color } => {
                self.workspace_icon = icon;
                self.workspace_color = color;
                self.workspace_appearance_customized = true;
                self.workspace_appearance = None;
                true
            }
        }
    }

    fn settle_profile_appearance(&mut self, outcome: AppearanceOutcome) -> bool {
        match outcome {
            AppearanceOutcome::Keep => true,
            AppearanceOutcome::Cancel => {
                self.profile_appearance = None;
                true
            }
            AppearanceOutcome::Save { icon, color } => {
                self.profile_icon = icon;
                self.profile_color = color;
                self.profile_appearance_customized = true;
                self.profile_appearance = None;
                true
            }
        }
    }

    fn button(&mut self, label: &str, bottom: f32) -> Rect {
        let c = self.content();
        Rect::new(
            c.x,
            c.y + c.h - bottom,
            button_width(&mut self.engine, label).max(128.0),
            42.0,
        )
    }

    /// Whether the Profile form has a *Back*: only when there is an entry
    /// choice to go back to ([`Screen::first`]).
    fn profile_has_back() -> bool {
        Screen::first() != Screen::Profile
    }

    fn form_action_buttons(&mut self, primary_label: &str) -> (Rect, Rect) {
        let primary = self.button(primary_label, 64.0);
        let back_w = ActionButton::new("Back", ActionStyle::Ghost, None).width(&mut self.engine);
        let back = Rect::new(
            primary.x + primary.w + BUTTON_GAP,
            primary.y,
            back_w,
            primary.h,
        );
        (primary, back)
    }

    fn owner_name_rect(&self) -> Rect {
        let c = self.content();
        Rect::new(c.x, c.y + 194.0, c.w, 44.0)
    }

    fn profile_icon_rect(&self) -> Rect {
        let c = self.content();
        let (columns, card_h, pitch, top) = self.profile_card_metrics();
        let rows = 3_usize.div_ceil(columns);
        let last_card_bottom = top + (rows - 1) as f32 * pitch + card_h;
        let gap = if columns == 3 { 40.0 } else { 20.0 };
        Rect::new(c.x, last_card_bottom + gap, 44.0, 44.0)
    }

    fn profile_name_rect(&self) -> Rect {
        let c = self.content();
        let icon = self.profile_icon_rect();
        Rect::new(icon.x + icon.w + 10.0, icon.y, c.w - icon.w - 10.0, icon.h)
    }

    fn workspace_name_rect(&self) -> Rect {
        let c = self.content();
        let icon = self.workspace_icon_rect();
        Rect::new(icon.x + icon.w + 10.0, icon.y, c.w - icon.w - 10.0, icon.h)
    }

    fn workspace_icon_rect(&self) -> Rect {
        let c = self.content();
        let (columns, _, pitch, top) = self.workspace_card_metrics();
        let rows = TEMPLATES.len().div_ceil(columns);
        Rect::new(c.x, top + rows as f32 * pitch + 10.0, 44.0, 44.0)
    }

    /// **Where card `index` sits** in a row-major grid, from the metrics one of
    /// the two `*_card_metrics` answers with.
    ///
    /// One copy: drawing, hit-testing and the accessibility projection each
    /// had — or would have had — the same four lines, and a card a screen
    /// reader can find somewhere other than where it answers a press is
    /// exactly the defect a second copy produces.
    fn card_rect(&self, index: usize, metrics: (usize, f32, f32, f32)) -> Rect {
        let (columns, card_h, pitch, top) = metrics;
        let c = self.content();
        let card_w = (c.w - CARD_GAP * (columns - 1) as f32) / columns as f32;
        Rect::new(
            c.x + (index % columns) as f32 * (card_w + CARD_GAP),
            top + (index / columns) as f32 * pitch,
            card_w,
            card_h,
        )
    }

    fn profile_card_metrics(&self) -> (usize, f32, f32, f32) {
        let c = self.content();
        if c.w >= 528.0 {
            (3, 76.0, 88.0, c.y + 278.0)
        } else {
            // The documented minimum window has two columns. Compact cards
            // leave the derived Profile name and action row reachable below
            // them without changing their row-major order.
            (card_columns(c.w), 62.0, 70.0, c.y + 278.0)
        }
    }

    fn workspace_card_metrics(&self) -> (usize, f32, f32, f32) {
        let c = self.content();
        if c.w >= 528.0 && self.viewport.1 >= 640.0 {
            (3, 122.0, 134.0, c.y + 126.0)
        } else {
            // Two compact columns keep all six choices, the name field and the
            // action row reachable at the window's documented minimum size.
            (2, 62.0, 70.0, c.y + 106.0)
        }
    }

    /// A purpose card is a shortcut to a suggested Profile name, not a
    /// requirement: a person who types the name themselves can go on.
    fn profile_form_complete(&self) -> bool {
        !self.owner_name.is_empty() && !self.profile_name.is_empty()
    }

    /// Like the Profile step, no card is chosen for the person: a name is
    /// enough, and a workspace named without a card starts
    /// [empty](EMPTY_TEMPLATE).
    fn workspace_form_complete(&self) -> bool {
        self.creation_conflict || !self.workspace_name.is_empty()
    }

    fn profile_name(&self) -> String {
        self.profile_name.value()
    }

    fn update_profile_name_suggestion(&mut self) {
        if self.profile_name_edited {
            return;
        }
        let owner = self.owner_name.value();
        let suggestion = match self.purpose {
            Some(purpose) if !owner.is_empty() => {
                format!("{owner} ({})", purpose_label(purpose))
            }
            _ => String::new(),
        };
        self.profile_name.set_text(&mut self.engine, &suggestion);
        if !self.profile_appearance_customized {
            self.profile_icon = self
                .purpose
                .map(|purpose| purpose_emoji(purpose).to_owned());
            self.profile_color = None;
        }
    }

    fn draw_entry(&mut self, canvas: &mut Scene, root: Affine) {
        let layout = self.welcome_layout();
        if !layout.accounts {
            let focused = self
                .controls()
                .get(self.focus)
                .copied()
                .and_then(welcome_action);
            let note = self.cloud_note();
            welcome::draw_without_accounts(
                canvas,
                root,
                &mut self.engine,
                layout,
                note,
                self.hover.and_then(welcome_action),
                focused,
            );
            return;
        }
        let focus = self.controls().get(self.focus).copied();
        let focused = focus.and_then(welcome_action);
        let ready = self.email_form_ready();
        let failure = self.welcome_failure();
        let invalid = failure.as_ref().map(|f| f.at);
        let caret_on = self.caret_on();
        let hover = self.hover;
        // **The two fields are this form's, not the welcome surface's.**
        // `koru-ui` holds no account state, so it measured the rectangles and
        // drew the labels; the editors, their text and their focus rings are
        // drawn here — handed to it so they land inside its content scope.
        let address = &mut self.email_address;
        let password = &mut self.email_password;
        let fields = |canvas: &mut Scene, engine: &mut TextEngine| {
            for (hit, rect) in [
                (Hit::EmailAddress, layout.address),
                (Hit::EmailPassword, layout.password),
            ] {
                let lit = focus == Some(hit);
                let failed = invalid.is_some() && invalid == welcome_action(hit);
                let style = if failed {
                    InputStyle::form(lit).invalid()
                } else {
                    InputStyle::form(lit)
                }
                .caret_shown(caret_on);
                let _scope = inspect::scope(
                    if hit == Hit::EmailAddress {
                        "EmailAddressField"
                    } else {
                        "EmailPasswordField"
                    },
                    "component",
                    root,
                    scene::rect(rect.x, rect.y, rect.w, rect.h),
                    |record| {
                        record.property("control", "TextInput", "koru_ui::text_input");
                        record.property("focused", lit, "view state");
                        record.property("invalid", failed, "view state");
                    },
                );
                if hit == Hit::EmailAddress {
                    address.draw(
                        canvas,
                        root,
                        engine,
                        rect,
                        style,
                        Some("you@example.com"),
                        lit,
                    );
                } else {
                    // Masked until its eye is pressed: the password is in the
                    // editor for selection, paste and undo, and in the scene
                    // only while the person has asked to see it.
                    password.set_end_icon_state(
                        hover == Some(Hit::RevealPassword),
                        focus == Some(Hit::RevealPassword),
                    );
                    password.draw(canvas, root, engine, rect, style, None, lit);
                }
            }
        };
        welcome::draw(
            canvas,
            root,
            &mut self.engine,
            layout,
            failure.as_ref().map(EntryFailure::borrow),
            hover.and_then(welcome_action),
            focused,
            ready,
            fields,
        );
    }

    /// Both halves of the welcome form typed. The button is drawn disabled
    /// until then, because "Continue" with no address is a button
    /// that does nothing when pressed.
    fn email_form_ready(&self) -> bool {
        !self.email_address.value().trim().is_empty() && !self.email_password.is_empty()
    }

    /// The shape both waits share: what is happening, what is being waited
    /// on, and the one way out.
    fn draw_waiting(&mut self, canvas: &mut Scene, root: Affine, title: &str, detail: &str) {
        let p = theme::p();
        let c = self.content();
        draw_text(
            &mut self.engine,
            canvas,
            root,
            title,
            c.x,
            c.y + 120.0,
            theme::role::subtitle(),
            "theme::role::subtitle()",
            p.text,
        );
        draw_text(
            &mut self.engine,
            canvas,
            root,
            detail,
            c.x,
            c.y + 184.0,
            theme::role::subtitle(),
            "theme::role::subtitle()",
            p.text_dim,
        );
        let cancel = self.button("Cancel", 64.0);
        draw_button(
            canvas,
            root,
            &mut self.engine,
            cancel,
            "Cancel",
            ButtonRole::Secondary,
            self.hover == Some(Hit::Secondary),
            true,
        );
    }

    fn account_action_label(&self, action: crate::account_ui::Action) -> &'static str {
        use crate::account_ui::Action;
        match action {
            Action::ConfirmAccountCreation => {
                koru_i18n::t(self.language, koru_i18n::Key::AccountCreationConfirm)
            }
            Action::CancelAccountCreation => {
                koru_i18n::t(self.language, koru_i18n::Key::AccountCreationCancel)
            }
            Action::SignInGoogle
                if self.account_surface.as_ref().is_some_and(|surface| {
                    surface.kind == crate::account_ui::SurfaceKind::Collision
                }) =>
            {
                "Sign in with the connected Google Account"
            }
            Action::SignInGoogle
                if self.account_surface.as_ref().is_some_and(|surface| {
                    surface.kind == crate::account_ui::SurfaceKind::ErasedAccount
                }) =>
            {
                "Reconnect with Google"
            }
            Action::SignInGoogle => "Sign in with Google",
            Action::SignInAnotherGoogle => "Sign in with another Google Account",
            Action::ChooseProfile => "Choose another Profile",
            Action::Retry => koru_i18n::t(self.language, koru_i18n::Key::ReminderRetry),
            Action::RetryEnrollment => "Try again",
            Action::ContactSupport => "Contact support",
            Action::ContinueOffline => {
                koru_i18n::t(self.language, koru_i18n::Key::AccountContinueOffline)
            }
            Action::Cancel => "Cancel connection",
            Action::CancelEnrollment => "Cancel connection",
        }
    }

    fn activate_account_action(&mut self, index: usize) {
        use crate::account_ui::Action as AccountAction;
        let Some(action) = self
            .account_surface
            .as_ref()
            .and_then(|surface| surface.actions.get(index))
            .copied()
        else {
            return;
        };
        match action {
            AccountAction::ConfirmAccountCreation | AccountAction::CancelAccountCreation => {
                self.creation_submission = Some(crate::account_ui::CommandSubmissionId::next());
                self.creation_waiting = true;
                self.pending = Some(if action == AccountAction::ConfirmAccountCreation {
                    Action::ConfirmAccountCreation
                } else {
                    Action::CancelAccountCreation
                });
                if let Some(surface) = &mut self.account_surface {
                    surface.actions.clear();
                }
            }
            AccountAction::Retry => {
                if let Some((_, retry)) = &self.created_profile_retry {
                    self.pending = Some(Action::Account(retry.clone()));
                    if let Some(surface) = &mut self.account_surface {
                        surface.actions.clear();
                    }
                    return;
                }
                if self.creation_submission.is_some() {
                    self.creation_submission = Some(crate::account_ui::CommandSubmissionId::next());
                    self.creation_waiting = true;
                    if let Some(surface) = &mut self.account_surface {
                        surface.actions.clear();
                    }
                }
                self.pending = Some(Action::RetryAccount);
            }
            AccountAction::RetryEnrollment => self.pending = Some(Action::RetryEnrollment),
            AccountAction::ContactSupport => {
                self.pending = Some(Action::OpenUri(welcome::CHAT_URL));
            }
            AccountAction::SignInGoogle
                if self.account_surface.as_ref().is_some_and(|surface| {
                    surface.kind == crate::account_ui::SurfaceKind::Collision
                }) =>
            {
                self.pending = Some(Action::RetryAccount);
            }
            AccountAction::SignInGoogle | AccountAction::SignInAnotherGoogle => {
                self.begin_browser_signin(false);
            }
            AccountAction::ChooseProfile if !self.account_profiles.is_empty() => {
                self.screen = Screen::ExistingProfiles;
                self.focus = 0;
            }
            AccountAction::ChooseProfile => self.account_cancelled(),
            AccountAction::ContinueOffline => {
                if let Some((profile, _)) = &self.created_profile_retry {
                    self.pending = Some(Action::OpenCreatedProfile(*profile));
                } else {
                    self.show_profile();
                }
            }
            AccountAction::Cancel => self.cancel_browser_signin(),
            AccountAction::CancelEnrollment => self.pending = Some(Action::CancelEnrollment),
        }
    }

    fn account_action_rect(&mut self, index: usize, label: &str) -> Rect {
        let c = self.content();
        let (_, _, bottom) = self.account_copy_geometry();
        Rect::new(
            c.x,
            bottom + 24.0 + index as f32 * 50.0,
            button_width(&mut self.engine, label).max(128.0),
            42.0,
        )
    }

    fn account_copy_geometry(&mut self) -> (f32, f32, f32) {
        let c = self.content();
        let Some(surface) = &self.account_surface else {
            return (c.y, c.y, c.y);
        };
        let title_y = c.y + 40.0;
        let title = self
            .engine
            .simple_layout_wrapped(surface.title, theme::role::subtitle(), c.w);
        let body_y = title_y + title.height() + 16.0;
        let body = self
            .engine
            .simple_layout_wrapped(&surface.body, theme::role::row(), c.w);
        let bottom = body_y + body.height() + if surface.status.is_some() { 32.0 } else { 0.0 };
        (title_y, body_y, bottom)
    }

    fn account_action_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        let actions = self
            .account_surface
            .as_ref()
            .map(|surface| surface.actions.clone())
            .unwrap_or_default();
        actions.into_iter().enumerate().find_map(|(index, action)| {
            let label = self.account_action_label(action);
            self.account_action_rect(index, label)
                .contains(x, y)
                .then_some(Hit::AccountAction(index))
        })
    }

    fn draw_account_status(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(surface) = self.account_surface.clone() else {
            return;
        };
        let p = theme::p();
        let c = self.content();
        let (title_y, body_y, bottom) = self.account_copy_geometry();
        draw_wrapped_text(
            &mut self.engine,
            canvas,
            root,
            surface.title,
            c.x,
            title_y,
            c.w,
            theme::role::subtitle(),
            "theme::role::subtitle()",
            p.text,
        );
        draw_wrapped_text(
            &mut self.engine,
            canvas,
            root,
            &surface.body,
            c.x,
            body_y,
            c.w,
            theme::role::row(),
            "theme::role::row()",
            p.text_dim,
        );
        if let Some(status) = surface.status {
            draw_text(
                &mut self.engine,
                canvas,
                root,
                status,
                c.x,
                bottom - 24.0,
                theme::role::meta(),
                "theme::role::meta()",
                p.text_dim,
            );
        }
        let controls = self.controls();
        for (index, action) in surface.actions.into_iter().enumerate() {
            let label = self.account_action_label(action);
            let rect = self.account_action_rect(index, label);
            let hit = Hit::AccountAction(index);
            draw_button(
                canvas,
                root,
                &mut self.engine,
                rect,
                label,
                if index == 0 {
                    ButtonRole::Primary
                } else {
                    ButtonRole::Secondary
                },
                self.hover == Some(hit),
                controls.get(self.focus) == Some(&hit),
            );
        }
    }
    fn draw_existing_profiles(&mut self, canvas: &mut Scene, root: Affine) {
        self.draw_selection(canvas, root);
    }

    fn draw_restoring_profile(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let c = self.content();
        draw_text(
            &mut self.engine,
            canvas,
            root,
            "Restoring your Profile",
            c.x,
            c.y + 120.0,
            theme::role::subtitle(),
            "theme::role::subtitle()",
            p.text,
        );
        draw_wrapped_text(
            &mut self.engine,
            canvas,
            root,
            "Koru is validating the retained local content before making this Profile visible again.",
            c.x,
            c.y + 176.0,
            c.w,
            theme::role::row(),
            "theme::role::row()",
            p.text_dim,
        );
    }

    fn selected_profile_name(&self) -> &str {
        self.selected_existing
            .and_then(|index| self.account_profiles.get(index))
            .and_then(|profile| profile.name.as_deref())
            .unwrap_or("this Profile")
    }
    fn draw_profile(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let c = self.content();
        if self.can_reopen_account_creation() {
            let rect = self.resume_account_rect();
            let focused = self.controls().get(self.focus) == Some(&Hit::ResumeAccount);
            draw_button(
                canvas,
                root,
                &mut self.engine,
                rect,
                koru_i18n::t(self.language, koru_i18n::Key::AccountCreationResume),
                ButtonRole::Secondary,
                self.hover == Some(Hit::ResumeAccount),
                focused,
            );
        } else {
            draw_text(
                &mut self.engine,
                canvas,
                root,
                "Step 1 of 2",
                c.x,
                c.y + 24.0,
                theme::role::meta(),
                "theme::role::meta()",
                p.text_dim,
            );
        }
        draw_text(
            &mut self.engine,
            canvas,
            root,
            "First, who's working here?",
            c.x,
            c.y + 62.0,
            theme::role::subtitle(),
            "theme::role::subtitle()",
            p.text,
        );
        draw_text(
            &mut self.engine,
            canvas,
            root,
            "This is the name of the person who owns this Koru Identity.",
            c.x,
            c.y + 108.0,
            theme::role::row(),
            "theme::role::row()",
            p.text_dim,
        );
        draw_text(
            &mut self.engine,
            canvas,
            root,
            "Display name",
            c.x,
            c.y + 174.0,
            theme::role::meta(),
            "theme::role::meta()",
            p.text_dim,
        );
        let focused_control = self.controls().get(self.focus).copied();
        let owner_focused = focused_control == Some(Hit::Name);
        let profile_name_focused = focused_control == Some(Hit::ProfileName);
        let field = self.owner_name_rect();
        {
            let _name_field = inspect::scope(
                "NameField",
                "component",
                root,
                scene::rect(field.x, field.y, field.w, field.h),
                |record| {
                    record.property("control", "TextInput", "koru_ui::text_input");
                    record.property("focused", owner_focused, "view state");
                },
            );
            let caret_on = self.caret_on();
            self.owner_name.draw(
                canvas,
                root,
                &mut self.engine,
                field,
                InputStyle::form(owner_focused).caret_shown(caret_on),
                Some("Your name"),
                owner_focused,
            );
        }
        draw_text(
            &mut self.engine,
            canvas,
            root,
            "What is this Profile for?",
            c.x,
            c.y + 254.0,
            theme::role::meta(),
            "theme::role::meta()",
            p.text_dim,
        );
        let metrics = self.profile_card_metrics();
        let focused_control = self.controls().get(self.focus).copied();
        for (index, purpose) in PURPOSES.into_iter().enumerate() {
            let rect = self.card_rect(index, metrics);
            let _card = inspect::scope(
                "PurposeCard",
                "container",
                root,
                scene::rect(rect.x, rect.y, rect.w, rect.h),
                |record| {
                    record.property("purpose", purpose_label(purpose), "PURPOSES");
                    record.property("selected", self.purpose == Some(purpose), "view state");
                },
            );
            paint_choice_card(
                canvas,
                root,
                rect,
                self.purpose == Some(purpose),
                focused_control == Some(Hit::Purpose(purpose)),
            );
            // The label sits on the card's floor and the emoji takes the room
            // above it: twice the row size where the card has it, and only as
            // much as fits in the compact cards, so the two never touch.
            let label_style = theme::role::section_heading();
            let label_y = rect.y + rect.h - PURPOSE_CARD_PAD - label_style.metrics.line_height;
            let emoji_size = (label_y - PURPOSE_EMOJI_GAP - (rect.y + PURPOSE_CARD_PAD))
                .min(theme::role::row().metrics.size * 2.0);
            let emoji = self.engine.simple_layout(
                purpose_emoji(purpose),
                theme::TextStyle::new(
                    theme::TextMetrics::new(emoji_size, emoji_size),
                    theme::FontWeight::Medium,
                ),
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &emoji,
                rect.x + 12.0,
                rect.y + PURPOSE_CARD_PAD,
                None,
            );
            draw_text(
                &mut self.engine,
                canvas,
                root,
                purpose_label(purpose),
                rect.x + 12.0,
                label_y,
                label_style,
                "theme::role::section_heading()",
                p.text,
            );
        }
        let icon = self.profile_icon_rect();
        draw_text(
            &mut self.engine,
            canvas,
            root,
            "Profile name",
            c.x,
            icon.y - 20.0,
            theme::role::meta(),
            "theme::role::meta()",
            p.text_dim,
        );
        let profile_name = self.profile_name();
        {
            let _icon = inspect::scope(
                "ProfileIcon",
                "component",
                root,
                scene::rect(icon.x, icon.y, icon.w, icon.h),
                |record| {
                    record.property("control", "Tile", "koru_draw::tile");
                },
            );
            Tile {
                icon: self.profile_icon.as_deref(),
                color: self.profile_color.as_deref(),
                name: &profile_name,
            }
            .draw(canvas, root, &mut self.engine, icon);
            if self.controls().get(self.focus) == Some(&Hit::ProfileAppearance) {
                scene::stroke(
                    canvas,
                    root,
                    p.focus_ring,
                    2.0,
                    &scene::rrect(icon.x - 2.0, icon.y - 2.0, icon.w + 4.0, icon.h + 4.0, 12.0),
                );
            }
        }
        let profile_field = self.profile_name_rect();
        {
            let _profile_field = inspect::scope(
                "ProfileNameField",
                "component",
                root,
                scene::rect(
                    profile_field.x,
                    profile_field.y,
                    profile_field.w,
                    profile_field.h,
                ),
                |record| {
                    record.property("control", "TextInput", "koru_ui::text_input");
                    record.property("focused", profile_name_focused, "view state");
                },
            );
            let caret_on = self.caret_on();
            self.profile_name.draw(
                canvas,
                root,
                &mut self.engine,
                profile_field,
                InputStyle::form(profile_name_focused).caret_shown(caret_on),
                Some("Profile name"),
                profile_name_focused,
            );
        }
        let footer_y = icon.y + icon.h + 16.0;
        // Only the connected route says where things live: it is the one
        // that has an account to explain.
        if self.connect_account {
            draw_text(
                &mut self.engine,
                canvas,
                root,
                "Your Profile and content stay on your devices. Your Koru account only helps connect them.",
                c.x,
                footer_y,
                theme::role::meta(),
                "theme::role::meta()",
                p.text_dim,
            );
        }
        if let Some(error) = self.error.as_deref() {
            draw_text(
                &mut self.engine,
                canvas,
                root,
                error,
                c.x,
                footer_y + profile_note_h(self.connect_account),
                theme::role::meta(),
                "theme::role::meta()",
                p.notice_fg,
            );
        }
        let label = if self.screen == Screen::Finishing {
            "Finishing..."
        } else {
            "Continue"
        };
        let (button, back) = self.form_action_buttons(label);
        let complete = self.profile_form_complete();
        let controls = self.controls();
        let primary_focused = controls.get(self.focus) == Some(&Hit::Primary);
        let back_focused = controls.get(self.focus) == Some(&Hit::Back);
        draw_button_enabled(
            canvas,
            root,
            &mut self.engine,
            button,
            label,
            ButtonRole::Primary,
            self.hover == Some(Hit::Primary),
            primary_focused,
            complete,
        );
        if Self::profile_has_back() {
            ActionButton::new("Back", ActionStyle::Ghost, None).draw_focused(
                canvas,
                root,
                &mut self.engine,
                back,
                self.hover == Some(Hit::Back),
                back_focused,
            );
        }
        // `NeedsProfile` recovery (root-catalog §6.3): only while the host
        // found a restorable Profile or an unassigned workspace.
        if self.recovery_offered() {
            let rect = self.recovery_link_rect();
            let focused = controls.get(self.focus) == Some(&Hit::Recovery);
            ActionButton::new(
                koru_i18n::t(self.language, koru_i18n::Key::OnboardingRecoveryLink),
                ActionStyle::Ghost,
                None,
            )
            .draw_focused(
                canvas,
                root,
                &mut self.engine,
                rect,
                self.hover == Some(Hit::Recovery),
                focused,
            );
        }
    }
    fn draw_workspace(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let c = self.content();
        draw_text(
            &mut self.engine,
            canvas,
            root,
            "Step 2 of 2",
            c.x,
            c.y + 18.0,
            theme::role::meta(),
            "theme::role::meta()",
            p.text_dim,
        );
        draw_text(
            &mut self.engine,
            canvas,
            root,
            "Now a workspace to put things in",
            c.x,
            c.y + 54.0,
            theme::role::subtitle(),
            "theme::role::subtitle()",
            p.text,
        );
        draw_text(
            &mut self.engine,
            canvas,
            root,
            "Pick a template to start from. You can rename or empty it later.",
            c.x,
            c.y + 86.0,
            theme::role::row(),
            "theme::role::row()",
            p.text_dim,
        );
        let metrics = self.workspace_card_metrics();
        let detailed = metrics.1 >= 100.0;
        for (index, template) in TEMPLATES.iter().enumerate() {
            let rect = self.card_rect(index, metrics);
            let selected = self.template == Some(index);
            let enabled = !self.workspace_name_edited;
            let _card = inspect::scope(
                "TemplateCard",
                "container",
                root,
                scene::rect(rect.x, rect.y, rect.w, rect.h),
                |record| {
                    record.property("template", template.label, "TEMPLATES");
                    record.property("selected", selected, "view state");
                    record.property("enabled", enabled, "view state");
                    record.property(
                        "suggested",
                        template_is_suggested(self.purpose, index),
                        "template_is_suggested",
                    );
                },
            );
            paint_choice_card(
                canvas,
                root,
                rect,
                selected,
                self.controls().get(self.focus) == Some(&Hit::Template(index)),
            );
            let emoji = self
                .engine
                .simple_layout(template.emoji, theme::role::row());
            scene::draw_layout_first_line(
                canvas,
                root,
                &emoji,
                rect.x + 12.0,
                rect.y + if detailed { 12.0 } else { 18.0 },
                None,
            );
            draw_text(
                &mut self.engine,
                canvas,
                root,
                template.label,
                rect.x + if detailed { 12.0 } else { 42.0 },
                rect.y + if detailed { 43.0 } else { 18.0 },
                theme::role::section_heading(),
                "theme::role::section_heading()",
                if enabled || selected {
                    p.text
                } else {
                    p.text_dim
                },
            );
            if detailed {
                let description = self.engine.simple_layout_wrapped(
                    template.description,
                    theme::role::meta(),
                    rect.w - 24.0,
                );
                scene::draw_layout(
                    canvas,
                    root,
                    &description,
                    rect.x + 12.0,
                    rect.y + 69.0,
                    Some(if enabled || selected {
                        p.text_dim
                    } else {
                        p.row_hint
                    }),
                );
            }
            if detailed && template_is_suggested(self.purpose, index) {
                let suggested = self.engine.simple_layout("Suggested", theme::role::meta());
                let badge = Rect::new(
                    rect.x + rect.w - suggested.width() - 18.0,
                    rect.y + 9.0,
                    suggested.width() + 12.0,
                    20.0,
                );
                scene::fill(
                    canvas,
                    root,
                    p.row_selected,
                    &scene::rrect(badge.x, badge.y, badge.w, badge.h, 10.0),
                );
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &suggested,
                    badge.x + 6.0,
                    badge.y + 2.0,
                    Some(p.accent),
                );
            }
        }
        let icon = self.workspace_icon_rect();
        {
            let _icon = inspect::scope(
                "WorkspaceIcon",
                "component",
                root,
                scene::rect(icon.x, icon.y, icon.w, icon.h),
                |record| {
                    record.property("control", "Tile", "koru_draw::tile");
                },
            );
            Tile {
                icon: self.workspace_icon.as_deref(),
                color: self.workspace_color.as_deref(),
                name: &self.workspace_name.value(),
            }
            .draw(canvas, root, &mut self.engine, icon);
            if self.controls().get(self.focus) == Some(&Hit::WorkspaceAppearance) {
                scene::stroke(
                    canvas,
                    root,
                    p.focus_ring,
                    2.0,
                    &scene::rrect(icon.x - 2.0, icon.y - 2.0, icon.w + 4.0, icon.h + 4.0, 12.0),
                );
            }
        }
        let field = self.workspace_name_rect();
        {
            let _name_field = inspect::scope(
                "WorkspaceNameField",
                "component",
                root,
                scene::rect(field.x, field.y, field.w, field.h),
                |record| {
                    record.property("control", "TextInput", "koru_ui::text_input");
                    record.property("focused", self.focus == 0, "view state");
                },
            );
            let caret_on = self.caret_on();
            self.workspace_name.draw(
                canvas,
                root,
                &mut self.engine,
                field,
                InputStyle::form(self.focus == 0).caret_shown(caret_on),
                Some("Workspace name"),
                self.focus == 0,
            );
        }
        if let Some(error) = self.error.as_deref() {
            draw_text(
                &mut self.engine,
                canvas,
                root,
                error,
                c.x,
                c.y + c.h - 150.0,
                theme::role::meta(),
                "theme::role::meta()",
                p.notice_fg,
            );
        }
        let label = self.workspace_action_label();
        let (primary, back) = self.form_action_buttons(label);
        let complete = self.workspace_form_complete();
        let controls = self.controls();
        draw_button_enabled(
            canvas,
            root,
            &mut self.engine,
            primary,
            label,
            ButtonRole::Primary,
            self.hover == Some(Hit::Primary),
            controls.get(self.focus) == Some(&Hit::Primary),
            complete,
        );
        if self.screen != Screen::Finishing {
            ActionButton::new("Back", ActionStyle::Ghost, None).draw_focused(
                canvas,
                root,
                &mut self.engine,
                back,
                self.hover == Some(Hit::Back),
                controls.get(self.focus) == Some(&Hit::Back),
            );
        }
    }
}

fn paint_choice_card(canvas: &mut Scene, root: Affine, rect: Rect, selected: bool, focused: bool) {
    let p = theme::p();
    scene::fill(
        canvas,
        root,
        if selected { p.row_selected } else { p.panel_bg },
        &scene::rrect(rect.x, rect.y, rect.w, rect.h, 8.0),
    );
    scene::stroke(
        canvas,
        root,
        if selected { p.accent } else { p.panel_border },
        theme::BORDER_W,
        &scene::rrect(rect.x, rect.y, rect.w, rect.h, 8.0),
    );
    if focused {
        scene::stroke(
            canvas,
            root,
            p.focus_ring,
            2.0,
            &scene::rrect(rect.x - 2.0, rect.y - 2.0, rect.w + 4.0, rect.h + 4.0, 10.0),
        );
    }
}

/// The three purpose cards, in the one order drawing, hit-testing and the
/// accessibility projection all read them in.
const PURPOSES: [Purpose; 3] = [Purpose::Work, Purpose::School, Purpose::Personal];

fn purpose_label(purpose: Purpose) -> &'static str {
    match purpose {
        Purpose::Work => "Work",
        Purpose::School => "School",
        Purpose::Personal => "Personal",
    }
}

/// The line the connected route's note takes under the Profile form, which
/// a failure is written below. The account-free route has no note.
fn profile_note_h(connect_account: bool) -> f32 {
    if connect_account { 24.0 } else { 0.0 }
}

/// A purpose card's top and bottom air, and the least gap between its emoji
/// and its label.
const PURPOSE_CARD_PAD: f32 = 10.0;
const PURPOSE_EMOJI_GAP: f32 = 4.0;

fn purpose_emoji(purpose: Purpose) -> &'static str {
    match purpose {
        Purpose::Work => "💼",
        Purpose::School => "🎓",
        Purpose::Personal => "🏡",
    }
}

struct Template {
    key: &'static str,
    label: &'static str,
    emoji: &'static str,
    description: &'static str,
}

/// What a workspace named without choosing a card starts from: the one
/// template that adds nothing the person did not ask for.
const EMPTY_TEMPLATE: usize = 5;

const TEMPLATES: [Template; 6] = [
    Template {
        key: "project",
        label: "Project",
        emoji: "🚀",
        description: "Roadmap, meeting notes, decisions",
    },
    Template {
        key: "research",
        label: "Research",
        emoji: "🔎",
        description: "Sources, reading log, findings",
    },
    Template {
        key: "course",
        label: "Course",
        emoji: "📚",
        description: "Lectures, assignments, study plan",
    },
    Template {
        key: "recipes",
        label: "Recipes",
        emoji: "🍜",
        description: "To try, weeknight, groceries",
    },
    Template {
        key: "home",
        label: "Home",
        emoji: "🏠",
        description: "Improvements, quotes, warranties",
    },
    Template {
        key: "empty",
        label: "Empty",
        emoji: "📄",
        description: "One untitled document, nothing else",
    },
];

fn template_is_suggested(purpose: Option<Purpose>, index: usize) -> bool {
    matches!(
        (purpose, index),
        (Some(Purpose::Work), 0 | 1)
            | (Some(Purpose::School), 1 | 2)
            | (Some(Purpose::Personal), 3 | 4)
    )
}

fn card_columns(width: f32) -> usize {
    if width >= 528.0 {
        3
    } else if width >= 344.0 {
        2
    } else {
        1
    }
}

fn draw_text(
    engine: &mut TextEngine,
    canvas: &mut Scene,
    root: Affine,
    value: &str,
    x: f32,
    y: f32,
    style: theme::TextStyle,
    role: &'static str,
    color: Color,
) {
    koru_ui::text_paragraph::draw_line(canvas, root, engine, value, x, y, style, role, color);
}

#[allow(clippy::too_many_arguments)]
fn draw_wrapped_text(
    engine: &mut TextEngine,
    canvas: &mut Scene,
    root: Affine,
    value: &str,
    x: f32,
    y: f32,
    width: f32,
    style: theme::TextStyle,
    role: &'static str,
    color: Color,
) {
    koru_ui::text_paragraph::KoruTextParagraph::new(value, style, role, color).draw(
        canvas,
        root,
        engine,
        Rect::new(x, y, width, 0.0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidebar_count(view: &mut OnboardingView) -> usize {
        let mut canvas = Scene::new();
        let root = Affine::IDENTITY;
        let capture = koru_draw::inspect::Frame::begin(
            1,
            root,
            koru_draw::scene::rect(0.0, 0.0, 1000.0, 680.0),
        );
        view.draw(&mut canvas, root);
        capture
            .finish()
            .records
            .iter()
            .filter(|r| r.name == "WelcomeSidebar")
            .count()
    }

    // ----------------------------------------------- the email journey

    fn a_welcome_screen() -> OnboardingView {
        let mut view = OnboardingView::default();
        view.set_viewport(1000.0, 680.0);
        view
    }

    fn type_into(view: &mut OnboardingView, hit: Hit, text: &str) {
        let index = view
            .controls()
            .iter()
            .position(|control| *control == hit)
            .expect("the control is reachable");
        view.focus = index;
        for character in text.chars() {
            let value = character.to_string();
            view.key(
                &Key::Character(value.clone().into()),
                Some(&value),
                ModifiersState::empty(),
            );
        }
    }

    fn at_stage(view: &mut OnboardingView, stage: koru_agent_wire::AccountStage) {
        let mut state = AccountState::default();
        state.stage = stage;
        view.show_account_state(&state);
    }

    fn failed_with(view: &mut OnboardingView, code: &str) {
        let mut state = AccountState::default();
        state.stage = koru_agent_wire::AccountStage::Failed;
        state.failure_code = Some(code.to_owned());
        view.show_account_state(&state);
    }

    /// **With Koru Cloud hidden the first run has no account choice**
    /// (koru-features): it opens on the local Profile form — where *Use Koru
    /// without an account* leads — with no *Back* to an entry screen, and an
    /// account failure (which cannot be raised from here) still lands there.
    #[test]
    fn without_cloud_the_first_run_starts_on_the_local_profile_form() {
        koru_features::testing::with(koru_features::Features::ALL_OFF, || {
            let mut view = a_welcome_screen();
            assert_eq!(view.screen, Screen::Profile);
            assert!(!view.connect_account);
            assert!(!view.controls().contains(&Hit::Back));
            assert!(!view.controls().contains(&Hit::Secondary));
            view.account_failed("x");
            assert_eq!(view.screen, Screen::Profile);
        });
        assert_eq!(a_welcome_screen().screen, Screen::Entry);
    }

    /// **A build with no Koru Cloud says so, and offers nothing to sign in
    /// to.** The welcome keeps its one way in; every press where the email
    /// form or a provider would be does nothing, so no sign-in is ever asked
    /// of the Agent — and there is no *Try again*, because nothing failed.
    #[test]
    fn without_a_cloud_origin_the_welcome_says_so_and_starts_no_sign_in() {
        let mut view = a_welcome_screen();
        // What the shell seeds before the first frame, from the Agent link.
        view.observe_account_projection(&AccountState::not_configured());
        assert_eq!(view.screen, Screen::Entry);
        assert_eq!(
            view.controls(),
            vec![Hit::Primary, Hit::Terms, Hit::Privacy]
        );
        let layout = view.welcome_layout();
        assert!(!layout.accounts);
        assert!(layout.cloud_note.is_some());

        // Where the ordinary welcome has its form and its providers.
        let ordinary =
            WelcomeLayout::new_with_top(&mut TextEngine::new(), 1000.0, 680.0, 0.0, None);
        for rect in [
            ordinary.address,
            ordinary.password,
            ordinary.email_submit,
            ordinary.google,
            ordinary.apple,
        ] {
            view.mouse_down(rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
            view.mouse_up();
            assert_eq!(view.take_action(), None, "{rect:?} started something");
            assert_eq!(view.screen, Screen::Entry);
        }
        for hit in [
            Hit::Secondary,
            Hit::Apple,
            Hit::ContinueWithEmail,
            Hit::Retry,
        ] {
            view.activate(hit);
            assert_eq!(view.take_action(), None, "{hit:?}");
        }
        assert_eq!(view.screen, Screen::Entry);

        // The one way in still leads to the local Profile form.
        let offline = layout.offline;
        view.mouse_down(offline.x + offline.w * 0.5, offline.y + offline.h * 0.5);
        view.mouse_up();
        assert_eq!(view.screen, Screen::Profile);
    }

    /// **One submit, carrying both halves.** Which journey it turns out to be
    /// is the worker's answer, not this screen's, so there is one action and
    /// it carries what was typed.
    #[test]
    fn the_welcome_screen_submits_the_address_and_the_password_together() {
        let mut view = a_welcome_screen();
        type_into(&mut view, Hit::EmailAddress, "ada@example.invalid");
        type_into(&mut view, Hit::EmailPassword, "Correct-Horse-9");
        view.activate(Hit::ContinueWithEmail);
        assert_eq!(
            view.take_action(),
            Some(Action::ContinueWithEmail {
                address: "ada@example.invalid".into(),
                password: "Correct-Horse-9".into(),
            })
        );
    }

    /// An empty half is not a submit. The button is drawn disabled and the
    /// press does nothing rather than sending half a form.
    #[test]
    fn the_welcome_screen_does_not_submit_half_a_form() {
        let mut view = a_welcome_screen();
        type_into(&mut view, Hit::EmailAddress, "ada@example.invalid");
        view.activate(Hit::ContinueWithEmail);
        assert_eq!(view.take_action(), None);
        assert_eq!(view.screen, Screen::Entry);
    }

    /// The four stages the worker publishes are the four screens, and nothing
    /// else decides which is up.
    #[test]
    fn the_stage_decides_which_email_screen_is_up() {
        let mut view = a_welcome_screen();
        at_stage(&mut view, koru_agent_wire::AccountStage::AwaitingEmailCode);
        assert_eq!(view.screen, Screen::EmailCode);
        at_stage(
            &mut view,
            koru_agent_wire::AccountStage::AwaitingEmailPassword,
        );
        assert_eq!(view.screen, Screen::EmailPassword);
    }

    /// Six digits and nothing else, however they arrive.
    #[test]
    fn the_code_screen_keeps_six_digits_and_drops_everything_else() {
        let mut view = a_welcome_screen();
        at_stage(&mut view, koru_agent_wire::AccountStage::AwaitingEmailCode);
        type_into(&mut view, Hit::EmailCode, "12a34b567");
        assert_eq!(
            view.email_code.value(),
            "1234567".chars().take(6).collect::<String>()
        );
        assert!(view.email_code_complete());
        view.activate(Hit::Primary);
        assert_eq!(
            view.take_action(),
            Some(Action::SubmitEmailCode {
                code: "123456".into()
            })
        );
    }

    /// **A mistyped digit is not an error page.** The code in somebody's hand
    /// is still good, so the screen stays and says what happened.
    #[test]
    fn a_wrong_code_keeps_the_code_screen_up_with_a_reason() {
        let mut view = a_welcome_screen();
        at_stage(&mut view, koru_agent_wire::AccountStage::AwaitingEmailCode);
        failed_with(&mut view, "email_code_incorrect");
        assert_eq!(view.screen, Screen::EmailCode);
        assert!(view.error.is_some());
    }

    /// The one moment Koru says an address is taken, and what it does with
    /// the password that was not it.
    #[test]
    fn an_already_registered_address_goes_back_to_the_welcome_screen() {
        let mut view = a_welcome_screen();
        type_into(&mut view, Hit::EmailAddress, "ada@example.invalid");
        type_into(&mut view, Hit::EmailPassword, "Wrong-Password-9");
        at_stage(&mut view, koru_agent_wire::AccountStage::AwaitingEmailCode);
        failed_with(&mut view, "email_already_registered");
        assert_eq!(view.screen, Screen::Entry);
        assert_eq!(view.email_address.value(), "ada@example.invalid");
        assert!(
            view.email_password.is_empty(),
            "the password that did not match is not left in the field"
        );
        assert!(view.error.is_some());
    }

    /// Two boxes that disagree are not a password. The primary is out of
    /// reach until they agree, which is the one rule no server can check.
    #[test]
    fn the_password_screen_waits_for_both_boxes_to_agree() {
        let mut view = a_welcome_screen();
        at_stage(
            &mut view,
            koru_agent_wire::AccountStage::AwaitingEmailPassword,
        );
        type_into(&mut view, Hit::NewPassword, "Correct-Horse-9");
        type_into(&mut view, Hit::ConfirmPassword, "Correct-Horse-8");
        assert!(!view.controls().contains(&Hit::Primary));
        view.activate(Hit::Primary);
        assert_eq!(view.take_action(), None);

        view.confirm_password
            .set_text(&mut view.engine, "Correct-Horse-9");
        assert!(view.controls().contains(&Hit::Primary));
        view.activate(Hit::Primary);
        assert_eq!(
            view.take_action(),
            Some(Action::SubmitEmailPassword {
                password: "Correct-Horse-9".into()
            })
        );
    }

    #[test]
    fn the_code_screen_can_ask_for_another_code_or_a_different_address() {
        let mut view = a_welcome_screen();
        type_into(&mut view, Hit::EmailAddress, "ada@example.invalid");
        at_stage(&mut view, koru_agent_wire::AccountStage::AwaitingEmailCode);
        view.activate(Hit::ResendCode);
        assert_eq!(view.take_action(), Some(Action::ResendEmailCode));

        view.activate(Hit::AnotherAddress);
        assert_eq!(view.screen, Screen::Entry);
        assert_eq!(view.take_action(), None);
    }

    #[test]
    fn entry_draws_the_welcome_sidebar_exactly_once() {
        // `welcome::draw` paints the backdrop itself; the shell drawing it too
        // was the double-sidebar the inspector showed.
        let mut view = OnboardingView::default();
        view.set_viewport(1000.0, 680.0);
        assert_eq!(sidebar_count(&mut view), 1);
    }

    #[test]
    fn an_embedded_form_draws_no_sidebar_and_lines_up_with_the_host_head() {
        let mut view = OnboardingView::default();
        view.set_embedded(true);
        view.set_viewport(1000.0, 680.0);
        assert_eq!(sidebar_count(&mut view), 0, "entry");
        let layout = view.welcome_layout();
        assert_eq!(layout.sidebar.w, 0.0);
        assert_eq!(layout.right.x, 0.0);
        assert_eq!(layout.heading.x, 0.0, "the column starts at the left edge");
        view.show_profile();
        assert_eq!(sidebar_count(&mut view), 0, "a form screen");
        let content = view.content();
        assert_eq!((content.x, content.y), (0.0, 0.0));
    }

    #[test]
    fn a_form_screen_keeps_exactly_one_welcome_sidebar() {
        let mut view = OnboardingView::default();
        view.set_viewport(1000.0, 680.0);
        view.show_profile();
        assert_eq!(sidebar_count(&mut view), 1);
    }

    /// The two email fields are the welcome surface's content in the
    /// inspector, not siblings drawn after it.
    #[test]
    fn the_email_fields_sit_inside_the_welcome_content() {
        let mut view = a_welcome_screen();
        let records = capture(&mut view);
        let content = records
            .iter()
            .position(|r| r.name == "WelcomeContent")
            .expect("the welcome content");
        for field in ["EmailAddressField", "EmailPasswordField"] {
            let index = records
                .iter()
                .position(|r| r.name == field)
                .unwrap_or_else(|| panic!("{field} drawn"));
            let mut ancestor = records[index].parent;
            while let Some(a) = ancestor {
                if a == content {
                    break;
                }
                ancestor = records[a].parent;
            }
            assert_eq!(ancestor, Some(content), "{field} is inside WelcomeContent");
        }
    }

    fn capture(view: &mut OnboardingView) -> Vec<koru_draw::inspect::Record> {
        let mut canvas = Scene::new();
        let root = Affine::IDENTITY;
        let capture = koru_draw::inspect::Frame::begin(
            1,
            root,
            koru_draw::scene::rect(0.0, 0.0, 1000.0, 680.0),
        );
        view.draw(&mut canvas, root);
        capture.finish().records
    }

    /// The window's caption and a form's components are siblings, never nested
    /// inside one another: the caption is window furniture that hangs under
    /// the window root (nothing, in this view-level capture), and a card or
    /// field answers directly to its screen.
    #[test]
    fn the_caption_and_every_form_component_are_siblings_under_their_screen() {
        fn assert_siblings(records: &[koru_draw::inspect::Record], components: &[&str]) {
            let screen_index = records
                .iter()
                .position(|r| r.kind == "screen")
                .expect("the screen node is captured");
            let caption = records
                .iter()
                .position(|r| r.name == "Caption")
                .expect("the custom caption is captured");
            assert_ne!(
                records[caption].parent,
                Some(screen_index),
                "the caption is window furniture, not screen content"
            );
            for component in components {
                let hits: Vec<usize> = records
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| (r.name == *component).then_some(i))
                    .collect();
                assert!(!hits.is_empty(), "{component} is captured");
                for i in hits {
                    assert_eq!(
                        records[i].parent,
                        Some(screen_index),
                        "{component} is a sibling under the screen, not nested in another component"
                    );
                }
            }
        }

        let mut view = OnboardingView::default();
        view.set_viewport(1000.0, 680.0);
        view.set_custom_caption(true);
        view.show_profile();
        assert_siblings(
            &capture(&mut view),
            &[
                "NameField",
                "PurposeCard",
                "ProfileIcon",
                "ProfileNameField",
            ],
        );

        view.owner_name.set_text(&mut view.engine, "Ana");
        view.activate(Hit::Purpose(Purpose::Work));
        view.activate(Hit::Primary);
        assert_eq!(view.screen, Screen::Workspace);
        assert_siblings(
            &capture(&mut view),
            &["TemplateCard", "WorkspaceIcon", "WorkspaceNameField"],
        );
    }

    #[test]
    fn zero_profile_account_switch_retains_offline_form_and_exact_pending_submission() {
        let mut view = OnboardingView::default();
        let a = AccountState {
            active_scope: 1,
            stage: koru_agent_wire::AccountStage::AwaitingAccountCreation,
            ..Default::default()
        };
        let b = AccountState {
            active_scope: 2,
            ..Default::default()
        };
        view.select_account_context(&a);
        view.account_creation_required(&a);
        let confirm = view
            .account_surface
            .as_ref()
            .unwrap()
            .actions
            .iter()
            .position(|action| *action == crate::account_ui::Action::ConfirmAccountCreation)
            .unwrap();
        view.activate_account_action(confirm);
        let submission = view.account_submission().unwrap();
        view.take_action();
        view.show_profile();
        view.owner_name
            .set_text(&mut view.engine, "Retained A draft");
        view.select_account_context(&b);
        assert_eq!(view.screen, Screen::Entry);
        assert!(
            view.account_command_failed(crate::account_ui::CommandFailure {
                submission,
                uncertain: true
            })
        );
        assert_eq!(view.screen, Screen::Entry);
        assert_eq!(view.account_scope(), 2);
        view.select_account_context(&a);
        assert_eq!(view.owner_name.raw_text(), "Retained A draft");
        assert_eq!(view.screen, Screen::AccountStatus);
        assert!(!view.creation_waiting);
        assert_eq!(view.account_submission(), Some(submission));
        assert_eq!(view.controls(), view.form_controls());
    }

    #[test]
    fn delayed_profile_connection_retry_reuses_the_created_id_and_never_local_creation() {
        let mut view = OnboardingView::default();
        let profile = koru_core::ProfileId::new();
        let retry = koru_agent_wire::AccountCommand::CreateProfile {
            profile_id: profile.to_storage(),
            owner_display_name: "A".into(),
            name: "Saved Profile".into(),
            locale: Some("en".into()),
        };
        view.connection_paused(profile, retry.clone());
        view.activate_account_action(0);
        assert_eq!(view.take_action(), Some(Action::Account(retry.clone())));
        assert!(view.account_surface.as_ref().unwrap().actions.is_empty());
        view.connection_paused(profile, retry);
        view.activate_account_action(1);
        assert_eq!(
            view.take_action(),
            Some(Action::OpenCreatedProfile(profile))
        );
    }

    #[test]
    fn confirmation_copy_and_hit_targets_fit_minimum_window_in_every_locale() {
        for lang in koru_i18n::Lang::ALL {
            for width in [452.0, 680.0, 900.0] {
                let mut view = OnboardingView::default();
                view.language = lang;
                view.set_viewport(width, 640.0);
                view.account_creation_required(&AccountState::default());
                let (_, _, bottom) = view.account_copy_geometry();
                let surface = view.account_surface.clone().unwrap();
                let c = view.content();
                let mut previous = bottom;
                for (index, action) in surface.actions.iter().enumerate() {
                    let label = view.account_action_label(*action);
                    let rect = view.account_action_rect(index, label);
                    assert!(
                        rect.y >= previous,
                        "{lang:?}/{width}: overlapping copy/action"
                    );
                    assert!(
                        rect.x >= c.x && rect.x + rect.w <= c.x + c.w,
                        "{lang:?}/{width}: label outside content {rect:?}"
                    );
                    assert!(
                        rect.y + rect.h <= c.y + c.h,
                        "{lang:?}/{width}: action below window {rect:?}"
                    );
                    assert_eq!(
                        view.hit(rect.x + rect.w * 0.5, rect.y + rect.h * 0.5),
                        Some(Hit::AccountAction(index))
                    );
                    previous = rect.y + rect.h;
                }
                view.draw(&mut Scene::new(), Affine::IDENTITY);
            }
        }
    }

    #[test]
    fn confirmation_offline_return_and_exact_failure_recovery_do_not_start_google() {
        use crate::account_ui::{Action as AccountAction, CommandFailure};
        let mut view = OnboardingView::default();
        view.account_creation_required(&AccountState::default());
        view.activate_account_action(2);
        assert_eq!(view.screen, Screen::Profile);
        assert!(view.controls().contains(&Hit::ResumeAccount));
        let resume = view.resume_account_rect();
        assert_eq!(
            view.hit(resume.x + 4.0, resume.y + 4.0),
            Some(Hit::ResumeAccount)
        );
        view.activate(Hit::ResumeAccount);
        assert_eq!(view.screen, Screen::AccountStatus);
        assert_eq!(view.take_action(), None);
        view.activate_account_action(0);
        assert_eq!(view.take_action(), Some(Action::ConfirmAccountCreation));
        let first = view.account_submission().unwrap();
        assert!(view.account_command_failed(CommandFailure {
            submission: first,
            uncertain: true
        }));
        assert_eq!(
            view.account_surface.as_ref().unwrap().actions[0],
            AccountAction::Retry
        );
        view.show_account_state(&AccountState {
            stage: koru_agent_wire::AccountStage::AwaitingAccountCreation,
            ..Default::default()
        });
        assert_eq!(
            view.account_surface.as_ref().unwrap().actions[0],
            AccountAction::Retry
        );
        view.activate_account_action(0);
        assert_eq!(view.take_action(), Some(Action::RetryAccount));
        let retry = view.account_submission().unwrap();
        assert_ne!(first, retry);
        assert!(!view.account_command_failed(CommandFailure {
            submission: first,
            uncertain: true
        }));
        view.account_creation_finished();
        assert_eq!(
            view.account_surface.as_ref().unwrap().kind,
            crate::account_ui::SurfaceKind::Finishing
        );
        assert!(view.account_surface.as_ref().unwrap().actions.is_empty());
        assert!(!view.account_command_failed(CommandFailure {
            submission: retry,
            uncertain: true
        }));
        view.account_creation_required(&AccountState::default());
        view.activate_account_action(1);
        assert_eq!(view.take_action(), Some(Action::CancelAccountCreation));
        assert!(!view.account_command_failed(CommandFailure {
            submission: retry,
            uncertain: true
        }));
    }

    #[test]
    fn confirmation_keyboard_uses_the_displayed_choice_once() {
        for cancel in [false, true] {
            let mut view = OnboardingView::default();
            view.account_creation_required(&AccountState::default());
            let key = Key::Named(if cancel {
                NamedKey::Escape
            } else {
                NamedKey::Enter
            });
            assert!(view.key(&key, None, ModifiersState::empty()));
            assert_eq!(
                view.take_action(),
                Some(if cancel {
                    Action::CancelAccountCreation
                } else {
                    Action::ConfirmAccountCreation
                })
            );
            view.key(&key, None, ModifiersState::empty());
            assert_eq!(view.take_action(), None);
        }
    }

    #[test]
    fn purpose_marks_suggestions_without_reordering_templates() {
        assert!(template_is_suggested(Some(Purpose::Work), 0));
        assert!(template_is_suggested(Some(Purpose::Work), 1));
        assert!(template_is_suggested(Some(Purpose::School), 2));
        assert!(template_is_suggested(Some(Purpose::Personal), 3));
        assert!(template_is_suggested(Some(Purpose::Personal), 4));
        assert!(!template_is_suggested(Some(Purpose::Work), 2));
        assert_eq!(TEMPLATES[0].key, "project");
        assert_eq!(TEMPLATES[5].key, "empty");
        assert_eq!(TEMPLATES[0].emoji, "🚀");
        assert_eq!(
            TEMPLATES[5].description,
            "One untitled document, nothing else"
        );
    }

    #[test]
    fn card_breakpoints_match_the_approved_appendix() {
        assert_eq!(card_columns(528.0), 3);
        assert_eq!(card_columns(527.0), 2);
        assert_eq!(card_columns(344.0), 2);
        assert_eq!(card_columns(343.0), 1);
    }

    #[test]
    fn tab_moves_keyboard_focus_across_purpose_cards_without_changing_the_choice() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.activate(Hit::Purpose(Purpose::Work));
        assert_eq!(view.purpose, Some(Purpose::Work));
        view.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty());
        view.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty());
        assert_eq!(
            view.controls().get(view.focus),
            Some(&Hit::Purpose(Purpose::School))
        );
        assert_eq!(view.purpose, Some(Purpose::Work));
        view.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty());
        assert_eq!(view.purpose, Some(Purpose::School));
    }

    #[test]
    fn a_typed_workspace_name_is_created_empty_without_a_card() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Ana");
        view.activate(Hit::Purpose(Purpose::Work));
        view.activate(Hit::Primary);
        view.workspace_name.set_text(&mut view.engine, "Notes");
        assert!(view.controls().contains(&Hit::Primary));
        view.activate(Hit::Primary);
        assert!(matches!(
            view.take_action(),
            Some(Action::CreateLocalProfile { template, .. }) if template == "empty"
        ));
    }

    #[test]
    fn a_creation_retry_preserves_identity_but_a_new_form_does_not() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Ana");
        view.activate(Hit::Purpose(Purpose::Work));
        view.activate(Hit::Primary);
        view.workspace_name.set_text(&mut view.engine, "Notes");
        view.activate(Hit::Primary);
        let Some(Action::CreateLocalProfile { id, .. }) = view.take_action() else {
            panic!("complete form did not submit");
        };
        view.creation_failed("response was lost");
        view.activate(Hit::Primary);
        assert!(
            matches!(view.take_action(), Some(Action::CreateLocalProfile { id: retry, .. }) if id == retry)
        );
        assert_ne!(id, OnboardingView::default().local_creation_id);
    }

    #[test]
    fn the_empty_template_constant_names_the_empty_card() {
        assert_eq!(TEMPLATES[EMPTY_TEMPLATE].key, "empty");
    }

    #[test]
    fn tab_moves_keyboard_focus_across_workspace_cards_without_changing_the_choice() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Steven");
        view.activate(Hit::Purpose(Purpose::Personal));
        view.activate(Hit::Primary);
        assert_eq!(view.template, None);
        view.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty());
        view.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty());
        view.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty());
        assert_eq!(view.controls().get(view.focus), Some(&Hit::Template(1)));
        assert_eq!(view.template, None);
        view.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty());
        assert_eq!(view.template, Some(1));
    }

    #[test]
    fn offline_creation_requires_name_and_purpose() {
        let mut view = OnboardingView::default();
        view.show_profile();
        assert!(!view.controls().contains(&Hit::Primary));
        let next = view.button("Continue", 64.0);
        assert!(!view.mouse_down(next.x + 4.0, next.y + 4.0));
        view.activate(Hit::Primary);
        assert_eq!(view.take_action(), None);
        view.owner_name.set_text(&mut view.engine, "Steven");
        assert!(!view.controls().contains(&Hit::Primary));
        view.activate(Hit::Purpose(Purpose::Personal));
        assert!(view.controls().contains(&Hit::Primary));
        view.activate(Hit::Primary);
        assert_eq!(view.screen, Screen::Workspace);
    }

    #[test]
    fn a_typed_profile_name_continues_without_a_purpose_card() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Steven");
        view.profile_name
            .set_text(&mut view.engine, "Design Updates");
        view.profile_name_edited = true;
        assert!(view.purpose.is_none());
        assert!(view.controls().contains(&Hit::Primary));
        view.activate(Hit::Primary);
        assert_eq!(view.screen, Screen::Workspace);
    }

    #[test]
    fn profile_back_returns_to_welcome_and_sits_beside_the_primary_action() {
        let mut view = OnboardingView::default();
        view.show_profile();
        let (primary, back) = view.form_action_buttons("Continue");

        assert_eq!(back.y, primary.y);
        assert!(back.x >= primary.x + primary.w + BUTTON_GAP);
        assert!(view.mouse_down(back.x + back.w * 0.5, back.y + back.h * 0.5));
        assert_eq!(view.screen, Screen::Entry);
    }

    #[test]
    fn workspace_back_uses_the_same_horizontal_action_row() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Steven");
        view.activate(Hit::Purpose(Purpose::Personal));
        view.activate(Hit::Primary);
        let (primary, back) = view.form_action_buttons("Create workspace");

        assert_eq!(back.y, primary.y);
        assert!(back.x >= primary.x + primary.w + BUTTON_GAP);
        assert!(view.mouse_down(back.x + back.w * 0.5, back.y + back.h * 0.5));
        assert_eq!(view.screen, Screen::Profile);
        assert_eq!(view.owner_name.raw_text(), "Steven");
        assert_eq!(view.purpose, Some(Purpose::Personal));
    }

    #[test]
    fn profile_name_uses_shared_text_editing_for_shift_and_control_backspace() {
        let mut view = OnboardingView::default();
        view.show_profile();

        assert!(view.key(
            &Key::Character("S".into()),
            Some("S"),
            ModifiersState::SHIFT
        ));
        for value in ["teven", " Example"] {
            assert!(view.key(
                &Key::Character(value.into()),
                Some(value),
                ModifiersState::empty()
            ));
        }
        assert_eq!(view.owner_name.raw_text(), "Steven Example");

        assert!(view.key(
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::CONTROL
        ));
        assert_eq!(view.owner_name.raw_text(), "Steven ");
    }

    #[test]
    fn selected_profile_form_continues_through_the_real_pointer_path() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Steven");
        let c = view.content();
        let (columns, card_h, pitch, top) = view.profile_card_metrics();
        let card_w = (c.w - CARD_GAP * (columns - 1) as f32) / columns as f32;
        let personal = Rect::new(
            c.x + (2 % columns) as f32 * (card_w + CARD_GAP),
            top + (2 / columns) as f32 * pitch,
            card_w,
            card_h,
        );
        assert!(view.mouse_down(personal.x + personal.w * 0.5, personal.y + personal.h * 0.5));
        assert_eq!(view.purpose, Some(Purpose::Personal));

        let next = view.form_action_buttons("Continue").0;
        assert!(view.mouse_down(next.x + next.w * 0.5, next.y + next.h * 0.5));
        assert_eq!(view.screen, Screen::Workspace);
    }

    #[test]
    fn profile_appearance_modal_blocks_cards_under_its_blank_space() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.activate(Hit::ProfileAppearance);
        let panel = view
            .profile_appearance
            .as_ref()
            .expect("profile appearance")
            .panel(view.viewport);

        // This point is in the panel below the swatches and also in the
        // Personal card. The floating panel owns it.
        assert!(view.mouse_down(panel.x + panel.w - 8.0, panel.y + panel.h - 4.0));
        assert_eq!(view.purpose, None);
    }

    #[test]
    fn profile_appearance_is_explicit_and_leaves_with_the_creation_action() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Ana");
        view.activate(Hit::ProfileAppearance);
        assert!(view.profile_appearance.is_some());
        view.settle_profile_appearance(AppearanceOutcome::Save {
            icon: Some("🧭".into()),
            color: Some("purple".into()),
        });
        assert!(view.profile_appearance.is_none());
        view.activate(Hit::Purpose(Purpose::Work));
        view.activate(Hit::Primary);
        view.workspace_name.set_text(&mut view.engine, "Project");
        view.activate(Hit::Template(0));
        view.activate(Hit::Primary);
        assert!(matches!(
            view.take_action(),
            Some(Action::CreateLocalProfile {
                profile_icon: Some(icon),
                profile_color: Some(color),
                ..
            }) if icon == "🧭" && color == "purple"
        ));
    }

    #[test]
    fn no_card_is_chosen_initially_and_typing_locks_the_template_choice() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Ana");
        view.activate(Hit::Purpose(Purpose::Work));
        view.activate(Hit::Primary);

        assert_eq!(view.template, None);
        assert_eq!(view.workspace_icon, None);
        assert!(!view.controls().contains(&Hit::Primary));
        assert!(view.controls().contains(&Hit::Template(1)));
        let (columns, card_h, _, top) = view.workspace_card_metrics();
        let content = view.content();
        let card_w = (content.w - CARD_GAP * (columns - 1) as f32) / columns as f32;
        let research = Rect::new(content.x + card_w + CARD_GAP, top, card_w, card_h);
        assert_eq!(
            view.workspace_hit(research.centre().0, research.centre().1),
            Some(Hit::Template(1))
        );
        view.activate(Hit::Template(1));
        assert_eq!(view.template, Some(1));
        assert_eq!(view.workspace_name.value(), "Research");
        view.activate(Hit::Template(2));
        assert_eq!(view.workspace_name.value(), "Course");
        assert!(view.key(
            &Key::Character("A".into()),
            Some("A"),
            ModifiersState::empty()
        ));
        assert!(view.workspace_name_edited);
        assert!(!view.controls().contains(&Hit::Template(1)));
        assert_eq!(
            view.workspace_hit(research.centre().0, research.centre().1),
            None
        );
        view.activate(Hit::Template(1));
        assert_eq!(view.template, Some(2));
        assert_eq!(view.workspace_name.value(), "CourseA");
        assert_eq!(view.workspace_icon.as_deref(), Some("📚"));
    }

    #[test]
    fn deleting_the_workspace_name_hands_the_choice_back_to_the_cards() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Ana");
        view.activate(Hit::Purpose(Purpose::Work));
        view.activate(Hit::Primary);
        view.activate(Hit::Template(1));
        assert_eq!(view.workspace_name.value(), "Research");

        // A manual edit makes the text authoritative and locks the cards...
        assert!(view.key(
            &Key::Character("A".into()),
            Some("A"),
            ModifiersState::empty()
        ));
        assert!(view.workspace_name_edited);
        assert!(!view.controls().contains(&Hit::Template(0)));

        // ...until the text is gone: an empty field holds no authoritative
        // text, so the cards become pickable again.
        for _ in 0.."ResearchA".len() {
            assert!(view.key(
                &Key::Named(NamedKey::Backspace),
                None,
                ModifiersState::empty()
            ));
        }
        assert!(view.workspace_name.is_empty());
        assert!(!view.workspace_name_edited);
        assert!(view.controls().contains(&Hit::Template(2)));
        view.activate(Hit::Template(2));
        assert_eq!(view.template, Some(2));
        assert_eq!(view.workspace_name.value(), "Course");
    }

    #[test]
    fn enter_on_the_name_field_submits_a_complete_form() {
        let enter = |view: &mut OnboardingView| {
            view.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty())
        };

        // Profile form: Enter on Profile name does nothing while a required
        // field is missing, and advances once everything is filled.
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Ana");
        view.activate(Hit::Purpose(Purpose::Work));
        view.focus = view
            .controls()
            .iter()
            .position(|hit| *hit == Hit::ProfileName)
            .unwrap();
        view.profile_name.set_text(&mut view.engine, "");
        assert!(enter(&mut view));
        assert_eq!(view.screen, Screen::Profile);
        view.profile_name.set_text(&mut view.engine, "Ana's things");
        assert!(enter(&mut view));
        assert_eq!(view.screen, Screen::Workspace);

        // Workspace form: Enter on Workspace name, after a template pick,
        // finishes the same way the Create workspace button does.
        assert_eq!(view.focus, 0, "the workspace name field starts focused");
        view.activate(Hit::Template(0));
        assert!(enter(&mut view));
        assert_eq!(view.screen, Screen::Finishing);
        assert!(matches!(
            view.take_action(),
            Some(Action::CreateLocalProfile { .. })
        ));
    }

    #[test]
    fn compact_workspace_layout_keeps_cards_name_and_actions_separate() {
        let mut view = OnboardingView::default();
        view.set_viewport(452.0, 640.0);
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Ana");
        view.activate(Hit::Purpose(Purpose::Work));
        view.activate(Hit::Primary);
        view.workspace_name.set_text(&mut view.engine, "Launch");

        let (columns, card_h, pitch, top) = view.workspace_card_metrics();
        let rows = TEMPLATES.len().div_ceil(columns);
        let last_card_bottom = top + (rows - 1) as f32 * pitch + card_h;
        let icon = view.workspace_icon_rect();
        let name = view.workspace_name_rect();
        let primary = view.form_action_buttons("Create workspace").0;

        assert!(last_card_bottom < icon.y);
        assert!(icon.x + icon.w < name.x);
        assert!(name.y + name.h < primary.y);
        assert!(primary.y + primary.h <= view.viewport.1);
    }

    #[test]
    fn profile_purpose_cards_precede_the_derived_name_at_minimum_size() {
        let mut view = OnboardingView::default();
        view.set_viewport(452.0, 640.0);
        view.show_profile();

        let (columns, card_h, pitch, top) = view.profile_card_metrics();
        let rows = 3_usize.div_ceil(columns);
        let last_card_bottom = top + (rows - 1) as f32 * pitch + card_h;
        let icon = view.profile_icon_rect();
        let name = view.profile_name_rect();
        let primary = view.form_action_buttons("Continue").0;

        assert!(last_card_bottom < icon.y);
        assert!(icon.x + icon.w < name.x);
        assert!(name.y + name.h < primary.y);
        assert_eq!(
            view.controls()[1..4],
            [
                Hit::Purpose(Purpose::Work),
                Hit::Purpose(Purpose::School),
                Hit::Purpose(Purpose::Personal),
            ]
        );
    }

    #[test]
    fn appearance_choice_leaves_with_the_workspace_creation_action() {
        let mut view = OnboardingView::default();
        view.show_profile();
        view.owner_name.set_text(&mut view.engine, "Ana");
        view.activate(Hit::Purpose(Purpose::Work));
        view.activate(Hit::Primary);
        view.workspace_name.set_text(&mut view.engine, "Launch");
        view.settle_workspace_appearance(AppearanceOutcome::Save {
            icon: Some("🌊".into()),
            color: Some("blue".into()),
        });
        view.activate(Hit::Primary);

        assert!(matches!(
            view.take_action(),
            Some(Action::CreateLocalProfile {
                workspace_icon: Some(icon),
                workspace_color: Some(color),
                ..
            }) if icon == "🌊" && color == "blue"
        ));
    }

    #[test]
    fn custom_caption_reserves_only_direct_window_actions() {
        let mut view = OnboardingView::default();
        view.set_custom_caption(true);
        let caption = view.caption_rect();
        let close = koru_ui::caption::direct_controls(caption)[2].rect.centre();
        assert_eq!(
            view.caption_action_at(close.0, close.1),
            Some(koru_ui::caption::Action::Close)
        );
        assert!(view.is_caption_drag_area(300.0, theme::CAPTION_H * 0.5));
        let welcome = view.welcome_layout();
        assert!(welcome.heading.y >= theme::CAPTION_H);
        assert!(!view.is_caption_drag_area(welcome.address.centre().0, welcome.address.centre().1));
    }
    #[test]
    fn browser_page_retains_caption_and_has_no_accounts_overlay() {
        let mut view = OnboardingView::default();
        view.set_viewport(452.0, 640.0);
        view.set_custom_caption(true);
        view.begin_browser_signin(false);
        let close = koru_ui::caption::direct_controls(view.caption_rect())[2]
            .rect
            .centre();
        assert!(view.mouse_move(close.0, close.1));
        assert_eq!(view.caption_hover, Some(koru_ui::caption::Action::Close));
        assert_eq!(
            view.caption_action_at(close.0, close.1),
            Some(koru_ui::caption::Action::Close)
        );
        view.draw(&mut Scene::new(), Affine::IDENTITY);
        assert_eq!(view.controls(), vec![Hit::Secondary]);
        assert_eq!(view.hit(30.0, theme::CAPTION_H + 16.0), None);
    }

    #[test]
    fn reused_connected_form_emits_only_a_connected_typed_draft() {
        let mut view = OnboardingView::default();
        view.begin_connected_profile_creation();
        view.owner_name
            .set_text(&mut view.engine, "New account Profile");
        view.activate(Hit::Purpose(Purpose::Work));
        view.activate(Hit::Primary);
        view.workspace_name
            .set_text(&mut view.engine, "New account Workspace");
        view.template = Some(0);
        view.activate(Hit::Primary);

        assert_eq!(
            view.take_action(),
            Some(Action::CreateLocalProfile {
                id: view.local_creation_id,
                owner_display_name: "New account Profile".into(),
                name: "New account Profile (Work)".into(),
                workspace_name: "New account Workspace".into(),
                template: "project".into(),
                workspace_icon: None,
                workspace_color: None,
                profile_icon: Some("💼".into()),
                profile_color: None,
                connect_account: true,
            })
        );
    }

    #[test]
    fn google_cancel_returns_to_the_entry_choice() {
        let mut view = OnboardingView::default();
        view.activate(Hit::Secondary);
        assert_eq!(view.take_action(), Some(Action::StartGoogle));
        view.account_cancelled();
        assert_eq!(view.screen, Screen::Entry);
    }

    #[test]
    fn apple_is_pressable_waits_for_apple_and_cancels_back_to_the_entry() {
        let mut view = OnboardingView::default();
        assert!(view.controls().contains(&Hit::Apple));
        let layout = WelcomeLayout::new(&mut view.engine, view.viewport.0, view.viewport.1);
        assert!(view.mouse_down(
            layout.apple.x + layout.apple.w * 0.5,
            layout.apple.y + layout.apple.h * 0.5
        ));
        assert_eq!(view.take_action(), Some(Action::StartApple));
        assert_eq!(view.screen, Screen::Browser);
        assert!(view.browser_apple);
        assert_eq!(view.take_telemetry(), vec![Telemetry::EntryApple]);
        view.account_cancelled();
        assert_eq!(view.screen, Screen::Entry);

        // Google after Apple waits for Google again.
        view.activate(Hit::Secondary);
        assert_eq!(view.take_action(), Some(Action::StartGoogle));
        assert!(!view.browser_apple);
    }

    fn recoverable() -> (RecoverableProfile, UnassignedWorkspaceRow) {
        (
            RecoverableProfile {
                profile: koru_core::ProfileId::new(),
                deletion: koru_store_core::ids::DeletionId::new(),
                name: "Work".into(),
                at_ms: 1_000,
                workspaces: 2,
                unavailable: 1,
            },
            UnassignedWorkspaceRow {
                workspace: koru_core::WorkspaceId::new(),
                name: "Orphan".into(),
            },
        )
    }

    /// `NeedsProfile` recovery (root-catalog §6.3): offered from the Profile
    /// form only while the host found something, restores by the exact
    /// batch, and never from an embedded form.
    #[test]
    fn recovery_is_offered_only_with_something_to_recover_and_restores_by_batch() {
        let mut view = OnboardingView::default();
        view.show_profile();
        assert!(!view.controls().contains(&Hit::Recovery));
        let (deleted, unassigned) = recoverable();
        view.show_recovery(vec![deleted.clone()], vec![unassigned]);
        assert!(view.controls().contains(&Hit::Recovery));
        let link = view.recovery_link_rect();
        assert_eq!(
            view.hit(link.x + link.w * 0.5, link.y + link.h * 0.5),
            Some(Hit::Recovery)
        );
        view.activate(Hit::Recovery);
        assert_eq!(view.screen, Screen::Recovery);
        assert_eq!(view.controls(), vec![Hit::RestoreDeleted(0), Hit::Back]);
        let layout = view.recovery_layout();
        let (_, button) = layout.deleted[0];
        assert_eq!(
            view.hit(button.x + button.w * 0.5, button.y + button.h * 0.5),
            Some(Hit::RestoreDeleted(0))
        );
        assert_eq!(layout.unassigned.len(), 1);
        assert!(layout.note.is_some(), "move needs a Profile first");
        assert!(
            view.recovery_deleted_line(&deleted)
                .contains("1 no longer available"),
            "{}",
            view.recovery_deleted_line(&deleted)
        );
        view.activate(Hit::RestoreDeleted(0));
        assert_eq!(
            view.take_action(),
            Some(Action::RestoreDeletedProfile {
                profile: deleted.profile,
                deletion: deleted.deletion,
            })
        );
        assert_eq!(view.screen, Screen::RestoringProfile);
        assert!(view.controls().is_empty(), "nothing to press while it runs");
        view.recovery_failed("refused");
        assert_eq!(view.screen, Screen::Recovery);
        assert_eq!(view.error.as_deref(), Some("refused"));
        view.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty());
        assert_eq!(view.screen, Screen::Profile);
        // Nothing left to recover: the offer is withdrawn.
        view.show_recovery(Vec::new(), Vec::new());
        assert!(!view.controls().contains(&Hit::Recovery));

        let mut embedded = OnboardingView::default();
        embedded.set_embedded(true);
        embedded.show_profile();
        let (deleted, unassigned) = recoverable();
        embedded.show_recovery(vec![deleted], vec![unassigned]);
        assert!(
            !embedded.controls().contains(&Hit::Recovery),
            "Manage has its own recovery sections"
        );
    }

    /// The recovery screen publishes its lists, and a client's click on a
    /// row's *Restore* — keyed by the Profile itself — is the same batch
    /// restore the keyboard raises.
    #[test]
    fn the_recovery_screen_publishes_its_rows_and_routes_a_click_to_the_restore() {
        use crate::accessibility::{ControlPart, Occurrence, ViewSlot, WindowSlot};
        let mut view = OnboardingView::default();
        view.show_profile();
        let (deleted, unassigned) = recoverable();
        view.show_recovery(vec![deleted.clone()], vec![unassigned.clone()]);
        view.activate(Hit::Recovery);
        let (window, slot) = (WindowSlot::from_raw(1), ViewSlot::from_raw(1));
        let nodes = view.accessibility_semantics(window, slot);
        assert_eq!(nodes.len(), 1);
        let lists: Vec<&crate::accessibility::SemanticNode> = nodes[0]
            .children
            .iter()
            .filter(|child| child.role == accesskit::Role::ListBox)
            .collect();
        assert_eq!(
            lists.len(),
            2,
            "deleted Profiles, then unassigned workspaces"
        );
        assert_eq!(lists[0].label.as_deref(), Some("Restore a deleted profile"));
        assert_eq!(lists[0].children.len(), 1);
        assert!(
            lists[0].children[0]
                .label
                .as_deref()
                .is_some_and(|label| label.starts_with("Work, "))
        );
        let restore = &lists[0].children[0].children[0];
        assert_eq!(restore.role, accesskit::Role::Button);
        assert_eq!(
            restore.occurrence,
            Occurrence::model(
                window,
                slot,
                deleted.profile.as_uuid(),
                ControlPart::new("restore")
            )
        );
        assert_eq!(lists[1].label.as_deref(), Some("Unassigned workspaces"));
        assert_eq!(lists[1].children[0].label.as_deref(), Some("Orphan"));
        assert_eq!(
            view.accessibility_focus(window, slot),
            Some(restore.occurrence.clone())
        );
        assert!(view.accessibility_action(
            restore.occurrence.clone(),
            accesskit::Action::Click,
            None
        ));
        assert_eq!(
            view.take_action(),
            Some(Action::RestoreDeletedProfile {
                profile: deleted.profile,
                deletion: deleted.deletion,
            })
        );
    }

    #[test]
    fn welcome_legal_links_leave_as_typed_uri_actions() {
        let mut view = OnboardingView::default();
        view.activate(Hit::Terms);
        assert_eq!(
            view.take_action(),
            Some(Action::OpenUri(welcome::TERMS_URL))
        );
        view.activate(Hit::Privacy);
        assert_eq!(
            view.take_action(),
            Some(Action::OpenUri(welcome::PRIVACY_URL))
        );
    }

    #[test]
    fn welcome_pointer_geometry_uses_the_shared_component() {
        let mut view = OnboardingView::default();
        let layout = WelcomeLayout::new(&mut view.engine, view.viewport.0, view.viewport.1);

        assert!(view.mouse_down(
            layout.terms.x + layout.terms.w * 0.5,
            layout.terms.y + layout.terms.h * 0.5
        ));
        assert_eq!(
            view.take_action(),
            Some(Action::OpenUri(welcome::TERMS_URL))
        );
    }

    #[test]
    fn existing_profiles_require_an_explicit_choice() {
        let mut view = OnboardingView::default();
        view.show_account_profiles(vec![AccountProfileRow {
            koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            name: Some("Steven".into()),
            locale: Some("en".into()),
            auth_identities: Vec::new(),
        }]);
        assert_eq!(view.screen, Screen::ExistingProfiles);
        assert_eq!(view.take_action(), None);
        view.activate(Hit::Existing(0));
        assert_eq!(view.take_action(), None);
        view.activate(Hit::Primary);
        assert_eq!(
            view.take_action(),
            Some(Action::SelectAccountProfiles {
                profile_ids: vec!["10000000-0000-4000-8000-000000000001".into()],
            })
        );
        assert_eq!(view.screen, Screen::ExistingProfiles);
    }

    #[test]
    fn retained_profile_restores_locally_instead_of_starting_enrollment() {
        let mut view = OnboardingView::default();
        view.show_account_profile_choices(
            "Steven",
            vec![AccountProfileChoice {
                profile: AccountProfileRow {
                    koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
                    profile_id: "10000000-0000-4000-8000-000000000002".into(),
                    name: Some("Steven".into()),
                    locale: Some("en".into()),
                    auth_identities: Vec::new(),
                },
                available_on_this_device: true,
                available_on_another_device: false,
            }],
        );

        view.activate(Hit::OpenExisting(0));

        assert_eq!(
            view.take_action(),
            Some(Action::RestoreLocalProfile {
                profile_id: "10000000-0000-4000-8000-000000000002".into(),
            })
        );
        assert_eq!(view.screen, Screen::RestoringProfile);
    }

    #[test]
    fn hosted_profile_without_a_content_source_can_be_selected_to_wait() {
        let mut view = OnboardingView::default();
        view.show_account_profile_choices(
            "Steven",
            vec![AccountProfileChoice {
                profile: AccountProfileRow {
                    koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
                    profile_id: "10000000-0000-4000-8000-000000000003".into(),
                    name: Some("Steven".into()),
                    locale: Some("en".into()),
                    auth_identities: Vec::new(),
                },
                available_on_this_device: false,
                available_on_another_device: false,
            }],
        );

        assert_eq!(view.account_profiles.len(), 1);
        view.activate(Hit::Existing(0));
        assert!(view.controls().contains(&Hit::Primary));
        view.activate(Hit::Primary);
        assert!(
            matches!(view.take_action(),Some(Action::SelectAccountProfiles { profile_ids }) if profile_ids.len()==1)
        );
        assert_eq!(view.screen, Screen::ExistingProfiles);
    }

    #[test]
    fn many_hosted_profiles_scroll_inside_the_choice_area_without_covering_actions() {
        let mut view = OnboardingView::default();
        view.set_viewport(760.0, 520.0);
        let choices = (0..8)
            .map(|index| AccountProfileChoice {
                profile: AccountProfileRow {
                    koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
                    profile_id: format!("10000000-0000-4000-8000-{index:012}"),
                    name: Some(format!("Profile {index}")),
                    locale: Some("en".into()),
                    auth_identities: Vec::new(),
                },
                available_on_this_device: false,
                available_on_another_device: true,
            })
            .collect();
        view.show_account_profile_choices("Steven", choices);

        let area = view.existing_profiles_viewport();
        let action = view.selection_layout().footer[0].1;
        assert!(area.y + area.h < action.y);
        assert!(view.existing_profile_scrollbar().is_some());

        assert!(view.mouse_move(area.x + 12.0, area.y + 12.0));
        assert!(view.scroll_by(winit::event::MouseScrollDelta::LineDelta(0.0, -2.0,)));
        assert!(view.existing_profile_scroll > 0.0);
        assert!(matches!(
            view.existing_profiles_hit(area.x + 12.0, area.y + 4.0),
            Some(Hit::Existing(index)) if index > 0
        ));
    }

    #[test]
    fn zero_hosted_profiles_is_the_only_result_that_enters_connected_creation() {
        let mut view = OnboardingView::default();
        view.show_account_profile_choices("Steven", Vec::new());

        assert_eq!(view.screen, Screen::Profile);
        assert!(view.connect_account);
        assert_eq!(view.owner_name.value(), "Steven");
    }

    #[test]
    fn purpose_prefills_profile_name_until_the_person_edits_it() {
        let mut view = OnboardingView::default();
        view.show_account_profile_choices("Steven", Vec::new());

        view.activate(Hit::Purpose(Purpose::Work));
        assert_eq!(view.profile_name.value(), "Steven (Work)");
        view.activate(Hit::ProfileName);
        assert!(view.key(&Key::Character("a".into()), None, ModifiersState::CONTROL));
        assert!(view.key(
            &Key::Character("Custom Profile".into()),
            Some("Custom Profile"),
            ModifiersState::empty()
        ));
        assert!(view.profile_name_edited);

        view.activate(Hit::Purpose(Purpose::School));
        assert_eq!(view.profile_name.value(), "Custom Profile");
    }

    #[test]
    fn first_run_choices_are_buffered_as_closed_telemetry_categories() {
        let mut view = OnboardingView::default();
        view.activate(Hit::Secondary);
        view.account_cancelled();
        view.activate(Hit::Primary);
        view.owner_name.set_text(&mut view.engine, "Ana");
        view.activate(Hit::Purpose(Purpose::Personal));

        assert_eq!(
            view.take_telemetry(),
            vec![
                Telemetry::EntryGoogle,
                Telemetry::EntryOffline,
                Telemetry::ProfilePurpose(Purpose::Personal),
            ]
        );
    }

    #[test]
    fn one_existing_profile_is_visible_and_requires_an_explicit_selection() {
        let mut view = OnboardingView::default();
        view.show_account_profiles(vec![AccountProfileRow {
            koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            name: Some("Steven".into()),
            locale: Some("en".into()),
            auth_identities: Vec::new(),
        }]);

        assert_eq!(view.selected_existing, None);
        view.activate(Hit::Primary);
        assert_eq!(view.take_action(), None);
        assert_eq!(view.screen, Screen::ExistingProfiles);

        view.activate(Hit::Existing(0));
        view.activate(Hit::Primary);

        assert_eq!(
            view.take_action(),
            Some(Action::SelectAccountProfiles {
                profile_ids: vec!["10000000-0000-4000-8000-000000000001".into()],
            })
        );
        assert_eq!(view.screen, Screen::ExistingProfiles);
        assert_eq!(view.selected_profile_name(), "Steven");
    }

    #[test]
    fn multiple_existing_profiles_still_require_an_explicit_selection() {
        let mut view = OnboardingView::default();
        view.show_account_profiles(vec![
            AccountProfileRow {
                koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
                profile_id: "10000000-0000-4000-8000-000000000004".into(),
                name: Some("First".into()),
                locale: Some("en".into()),
                auth_identities: Vec::new(),
            },
            AccountProfileRow {
                koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
                profile_id: "10000000-0000-4000-8000-000000000005".into(),
                name: Some("Second".into()),
                locale: Some("en".into()),
                auth_identities: Vec::new(),
            },
        ]);

        view.activate(Hit::Primary);

        assert_eq!(view.take_action(), None);
        assert_eq!(view.screen, Screen::ExistingProfiles);
    }

    #[test]
    fn enrollment_offers_offline_and_explicit_cancellation() {
        let mut view = OnboardingView::default();
        view.show_account_profiles(vec![AccountProfileRow {
            koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            name: Some("Steven".into()),
            locale: Some("en".into()),
            auth_identities: Vec::new(),
        }]);
        view.activate(Hit::Existing(0));
        view.activate(Hit::Primary);
        let _ = view.take_action();

        view.profile_selected();
        view.account_state.enrollments = vec![koru_agent_wire::AccountProfileEnrollment {
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            state: koru_agent_wire::EnrollmentState::Waiting,
            failure_code: None,
            retry_at_millis: None,
        }];
        view.activate(Hit::CancelSelection);

        assert_eq!(
            view.take_action(),
            Some(Action::Account(
                koru_agent_wire::AccountCommand::CancelEnrollment
            ))
        );
        assert_eq!(view.screen, Screen::ExistingProfiles);
        assert!(view.enrollment_cancelling);
        assert!(view.controls().contains(&Hit::ContinueOffline));
        view.activate(Hit::CancelSelection);
        assert_eq!(
            view.take_action(),
            None,
            "duplicate cancellation is disabled"
        );
        view.account_cancelled();
        assert_eq!(view.screen, Screen::ExistingProfiles);
        assert!(!view.enrollment_cancelling);
    }

    #[test]
    fn existing_profile_actions_follow_the_approved_visual_order() {
        let mut view = OnboardingView::default();
        let layout = view.selection_layout();
        assert_eq!(
            layout
                .footer
                .iter()
                .map(|(hit, _, _, _)| *hit)
                .collect::<Vec<_>>(),
            vec![
                Hit::Primary,
                Hit::RefreshProfiles,
                Hit::CreateNew,
                Hit::ContinueOffline
            ]
        );
        assert!(
            layout
                .footer
                .windows(2)
                .all(|pair| pair[0].1.y <= pair[1].1.y)
        );
    }

    #[test]
    fn signed_in_create_and_offline_remain_distinct_choices() {
        let mut connected = OnboardingView::default();
        connected.show_account_profiles(Vec::new());
        assert!(connected.connect_account);

        let mut chooser = OnboardingView::default();
        chooser.show_account_profiles(vec![AccountProfileRow {
            koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            name: Some("Steven".into()),
            locale: Some("en".into()),
            auth_identities: Vec::new(),
        }]);
        chooser.activate(Hit::CreateNew);
        assert!(chooser.connect_account);

        let mut offline = OnboardingView::default();
        offline.show_account_profiles(vec![AccountProfileRow {
            koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            name: Some("Steven".into()),
            locale: Some("en".into()),
            auth_identities: Vec::new(),
        }]);
        offline.activate(Hit::ContinueOffline);
        assert!(!offline.connect_account);
    }

    #[test]
    fn existing_profile_chooser_still_offers_offline_creation() {
        let mut view = OnboardingView::default();
        view.show_account_profiles(vec![AccountProfileRow {
            koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            name: Some("Steven".into()),
            locale: Some("en".into()),
            auth_identities: Vec::new(),
        }]);
        view.activate(Hit::ContinueOffline);
        assert_eq!(view.screen, Screen::Profile);
        assert_eq!(view.take_action(), None);
    }
    #[test]
    fn enrollment_failure_routes_a_typed_enrollment_retry() {
        let mut view = OnboardingView::default();
        let state = AccountState {
            enrollment: koru_agent_wire::EnrollmentState::Failed,
            enrollment_failure_code: Some("trust_persistence_failed".into()),
            ..AccountState::default()
        };
        view.show_account_state(&state);
        view.activate(Hit::AccountAction(0));
        assert_eq!(view.take_action(), Some(Action::RetryEnrollment));
    }

    #[test]
    fn collision_and_erased_account_choose_distinct_typed_actions() {
        let mut view = OnboardingView::default();
        let collision = AccountState {
            stage: koru_agent_wire::AccountStage::Failed,
            capability: true,
            failure_code: Some("collision_required".into()),
            ..AccountState::default()
        };
        view.show_account_state(&collision);
        assert_eq!(
            view.account_surface.as_ref().map(|surface| surface.kind),
            Some(crate::account_ui::SurfaceKind::Collision)
        );
        view.activate(Hit::AccountAction(0));
        assert_eq!(view.take_action(), Some(Action::RetryAccount));

        let erased = AccountState {
            failure_code: Some("account_erased".into()),
            ..collision
        };
        view.show_account_state(&erased);
        assert_eq!(
            view.account_surface.as_ref().map(|surface| surface.kind),
            Some(crate::account_ui::SurfaceKind::ErasedAccount)
        );
        view.activate(Hit::AccountAction(0));
        assert_eq!(view.take_action(), Some(Action::StartGoogle));
    }
}
