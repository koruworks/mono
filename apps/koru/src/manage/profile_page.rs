// SPDX-License-Identifier: MPL-2.0

//! A Profile's page: its header, what needs the person, its Workspaces
//! (`workspaces`), *Sign-in and sync* and *Delete profile…* — laid out as a
//! Settings page: a title, a lead, then captioned cards.
//!
//! ```text
//! [tile] Personal                       [Switch to this profile]   the title
//! Current profile · Local only                                     the lead
//! Can't reach Koru — see Koru Cloud        (account-wide, a link)
//! a notice from the last thing done
//! ┌ attention ────────────────────────────────┐   P19/P20: a deletion that
//! │ Koru couldn't delete this Profile          │   failed, a cleanup that
//! │ [Try again] [Remove from this device] [...]│   failed, or one found on
//! └────────────────────────────────────────────┘   another device
//! Workspaces · 3                      Import…  [New workspace]  see `workspaces`
//! ┌ the rows ──────────────────────────────────┐
//! └────────────────────────────────────────────┘
//! Show archived (2)   Show trash (1)
//! Sign-in and sync
//! ┌ repair, or the rows ───────────────────────┐   see `sign_in`
//! └────────────────────────────────────────────┘
//! Delete profile…
//! ```
//!
//! **The tile sits before the title, at the title's height** (32 px, the
//! title line's): it is the Profile's face as much as its name is, and it
//! stays one press from the icon and color panel without taking a card row
//! that would read as a setting.
//!
//! **One function places all of it** — [`ManageView::lay_profile_page`] —
//! and drawing, the hit test, keyboard order and the accessibility tree all
//! read its answer, kept in `ManageView::laid`. It is measured unscrolled, in
//! the tab's coordinates; whoever uses it applies the page's scroll.
//!
//! **While a deletion needs an answer, the page is that question.** The old
//! surface replaced its whole detail pane with it, and so does this page: the
//! sections below would offer to unlink or sync a Profile whose fate is not
//! decided yet.

use super::*;
use koru_draw::geometry::Rect;
use koru_draw::tile::Tile;
use koru_ui::button::{self, ActionButton, ActionStyle, ButtonRole};
use koru_ui::card::Card;
use koru_ui::text_paragraph::KoruTextParagraph;
use koru_ui::toggle::Toggle;

mod paint;
#[cfg(test)]
mod tests;

/// Between the attention card, each section and the footer — Settings'
/// gap between cards.
const SECTION_GAP: f32 = Card::GAP;
/// Inside an attention card.
const CARD_PAD: f32 = 16.0;
/// Between buttons in a row, and between rows of them when they wrap.
const BUTTON_GAP: f32 = 10.0;

/// Something on the page that can be pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Control {
    /// The header's tile: the icon and color panel.
    Tile,
    /// The header's name: rename in place.
    Name,
    /// *Switch to this profile*, on any Profile but the open one.
    Switch,
    /// *Can't reach Koru — see Koru Cloud*: the account-wide state's pointer.
    CloudPointer,
    /// One of the page's attention card's answers.
    Attention(Attention),
    /// The `n`th action of the Profile's account repair.
    Repair(usize),
    /// Bring back an account-creation question that was put aside.
    Resume,
    /// Sign in with Google, or stop waiting for the browser.
    Session,
    /// Take a local-only Profile into the signed-in account.
    Attach,
    /// The `n`th Google Account's *Unlink…*.
    Unlink(usize),
    AddGoogle,
    MakeOffline,
    SignOut,
    /// The Profile's Cloud sync switch.
    CloudSync,
    /// *Get Koru Cloud…*: the Koru Cloud page.
    GetCloud,
    /// *Delete profile…*.
    Delete,
    // ---- the Workspaces section (`workspaces`) ----
    /// *Import…*: content into the open workspace (decision 10: only on the
    /// open Profile's page).
    ImportWorkspaces,
    /// *New workspace*, made in this page's Profile.
    NewWorkspace,
    /// A workspace's row: a click selects it, a double-click or Enter opens
    /// it, a right-click opens the window's workspace menu.
    WorkspaceRow(workspaces::Wid),
    /// The row's `•••`: the same menu, anchored under the button.
    WorkspaceMore(workspaces::Wid),
    /// *Show archived (n)* / *Show trash (n)*.
    ShowArchived,
    ShowTrash,
    /// An archived row's *Unarchive*.
    Unarchive(workspaces::Wid),
    /// A trashed row's *Restore*.
    RestoreWorkspace(workspaces::Wid),
    /// The controller's notice: run it again, or put it away (S24).
    NoticeRetry,
    NoticeDismiss,
    // ---- the Linked devices page (`devices`) ----
    /// *Link a device…*: the Link dialog.
    LinkDevice,
    /// This computer's row: a double-click (or Enter) renames it.
    ThisDevice,
    /// The *Use relay* switch.
    Relay,
    /// The `n`th linked device's row: its history.
    DeviceRow(usize),
    /// The `n`th linked device's *Unlink…*.
    UnlinkDevice(usize),
    /// A device history's back row.
    HistoryBack,
    /// The `n`th event of the open history, when it opens a document.
    HistoryEvent(usize),
    /// *Load older*: the history's next page.
    OlderHistory,
    // ---- the New profile page (`new_profile`) ----
    /// The page's X: put the form away and land on the first Profile (the
    /// CTO's note).
    CloseCreation,
    // ---- the Koru Cloud page (`cloud`) ----
    /// *Accounts…*: the account switcher.
    Accounts,
    /// The `n`th action of the account-wide can't-connect card.
    Unreachable(usize),
    /// The different-account card's three answers (decision 4).
    DifferentCreate,
    DifferentSignIn,
    DifferentOffline,
    /// Subscribe, *Manage subscription*, *Try again* or sign in.
    CloudPrimary,
    CloudLearnMore,
    /// A Profile's Cloud sync switch on *Which profiles sync*.
    ProfileSync(ProfileId),
    /// *Sign in…*: that Profile's page.
    ProfileSignIn(ProfileId),
    /// A syncing Profile's progress disclosure (decision 20).
    Progress(ProfileId),
    /// A held workspace's row in the open progress: its waiting documents.
    ProgressRow(workspaces::Wid),
}

/// The answers on the page's attention card (P19/P20).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Attention {
    RetryDelete,
    RemoveAfterFailure,
    CancelFailure,
    FinishCleanup,
    CleanupLater,
    Restore,
    RemoveDiscovered,
    ContinueOffline,
}

impl Attention {
    fn intent(self) -> Intent {
        match self {
            Self::RetryDelete => Intent::RetryHostedDelete,
            Self::RemoveAfterFailure => Intent::RemoveAfterHostedDeleteFailure,
            Self::CancelFailure => Intent::CancelHostedDeleteFailure,
            Self::FinishCleanup => Intent::FinishHostedCleanup,
            Self::CleanupLater => Intent::PostponeHostedCleanup,
            Self::Restore => Intent::RestoreDiscoveredDeletion,
            Self::RemoveDiscovered => Intent::RemoveDiscoveredDeletion,
            Self::ContinueOffline => Intent::ContinueDeletedOffline,
        }
    }
}

/// How a control is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Look {
    /// Part of the header, which paints it; pressed where it is drawn.
    Header,
    /// A dialog-height button.
    Button(ButtonRole),
    /// The shared Google button.
    Google,
    /// A compact text button.
    Action(ActionStyle),
    Toggle(Toggle),
    /// A line of accent text.
    Link,
    /// A workspace row, painted by its section rather than as a control.
    Row,
    /// The shared icon button with a `•••`.
    More,
    /// The shared icon button with an X: the New profile page's close.
    Close,
    /// A disclosure: a triangle and a line of accent text; `true` while open.
    Disclosure(bool),
}

/// One control, where it goes and what it says.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placed {
    pub control: Control,
    pub rect: Rect,
    pub label: String,
    pub look: Look,
}

/// A titled block of copy: an attention card's words.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Words {
    /// The card's box, or the block's area inside a section.
    pub rect: Rect,
    pub title: String,
    pub title_at: Rect,
    pub body: String,
    pub body_at: Rect,
}

/// One row of a section: a name and a sentence on the left, its control on
/// the right — or, with no name, a strip of text buttons.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub rect: Rect,
    pub label: Option<String>,
    pub hint: Option<String>,
    pub text_w: f32,
}

/// *Sign-in and sync*: a titled card holding either the Profile's account
/// repair or its rows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Section {
    /// The whole card, its title line included.
    pub rect: Rect,
    pub repair: Option<Words>,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Header {
    pub rect: Rect,
    pub tile: Rect,
    pub name: Rect,
    pub name_text: String,
    /// Where the name wraps — the title's full width.
    pub text_x: f32,
    pub text_w: f32,
    pub status: Rect,
    pub status_text: String,
    pub notice: Option<(Rect, String)>,
}

impl Default for Header {
    fn default() -> Self {
        let none = Rect::new(0.0, 0.0, 0.0, 0.0);
        Self {
            rect: none,
            tile: none,
            name: none,
            name_text: String::new(),
            text_x: 0.0,
            text_w: 0.0,
            status: none,
            status_text: String::new(),
            notice: None,
        }
    }
}

/// Where everything on a Profile's page goes, unscrolled.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Laid {
    pub profile: Option<ProfileId>,
    pub header: Header,
    pub attention: Option<Words>,
    pub workspaces: Option<workspaces::WorkspacesSection>,
    pub sign_in: Option<Section>,
    pub footer: Option<Rect>,
    /// The Linked devices page, when that is the page.
    pub devices: Option<devices::DevicesLaid>,
    /// The Koru Cloud page, when that is the page.
    pub cloud: Option<cloud::CloudLaid>,
    /// The New profile page's head, when that is the page: the form goes
    /// under it.
    pub head: Option<page::Head>,
    /// Every control, top to bottom — which is also keyboard order.
    pub controls: Vec<Placed>,
    /// The controls drawn dimmed, whose press is consumed and does nothing:
    /// the controller gave a reason, which the page says.
    pub disabled: Vec<Control>,
    /// From the column's top to below the last thing on the page.
    pub height: f32,
}

impl Laid {
    pub(crate) fn placed(&self, control: Control) -> Option<&Placed> {
        self.controls
            .iter()
            .find(|placed| placed.control == control)
    }
}

/// What the Section title says.
pub(super) const SIGN_IN_TITLE: &str = "Sign-in and sync";
const CURRENT: &str = "Current profile · ";
const SWITCH: &str = "Switch to this profile";
const CLOUD_POINTER: &str = "Can't reach Koru — see Koru Cloud";
const DELETE: &str = "Delete profile…";

impl ManageView {
    /// The Profile this page is about, when the page is a Profile's.
    pub(super) fn page_profile(&self) -> Option<ProfileSummary> {
        let Some(Page::Profile(id)) = self.page else {
            return None;
        };
        self.c.profiles().iter().find(|row| row.id == id).cloned()
    }

    /// Lay the page out again from the controller's state. Cheap: it measures
    /// a handful of lines, and it runs after each change rather than on a clock.
    pub(super) fn relayout(&mut self) {
        // The compact band first: the page starts under it.
        self.relayout_nav();
        self.laid = match (self.page, self.page_profile()) {
            (_, Some(profile)) => self.lay_profile_page(&profile),
            (Some(Page::Devices), None) => self.lay_devices_page(),
            (Some(Page::Cloud), None) => self.lay_cloud_page(),
            (Some(Page::NewProfile), None) => self.lay_new_profile_page(),
            _ => Laid::default(),
        };
        let controls: Vec<Control> = self.laid.controls.iter().map(|p| p.control).collect();
        if self.page_hover.is_some_and(|c| !controls.contains(&c)) {
            self.page_hover = None;
        }
        if self.page_focus.is_some_and(|c| !controls.contains(&c)) {
            self.page_focus = None;
        }
    }

    fn lay_profile_page(&mut self, profile: &ProfileSummary) -> Laid {
        let column = self.column();
        let mut controls = Vec::new();
        let mut disabled = Vec::new();
        let header = self.lay_header(profile, column, &mut controls);
        let mut y = header.rect.y + header.rect.h + koru_ui::PageShell::LEAD_GAP;
        let attention = self.lay_attention(profile, column, y, &mut controls);
        let (workspaces, sign_in, footer) = match &attention {
            // The page is the question while it is up: see the module docs.
            Some(card) => {
                y = card.rect.y + card.rect.h;
                (None, None, None)
            }
            None => {
                let (workspaces, bottom) =
                    self.lay_workspaces_section(profile, column, y, &mut controls, &mut disabled);
                y = bottom + SECTION_GAP;
                // *Sign-in and sync* is Koru Cloud's; a build that hides it
                // has no such section (koru-features).
                let section = koru_features::cloud_sync().then(|| {
                    let section = self.lay_sign_in(profile, column, y, &mut controls);
                    y = section.rect.y + section.rect.h + SECTION_GAP;
                    section
                });
                let footer = self.lay_footer(column, y, &mut controls);
                y = footer.y + footer.h;
                (Some(workspaces), section, Some(footer))
            }
        };
        Laid {
            profile: Some(profile.id),
            header,
            attention,
            workspaces,
            sign_in,
            footer,
            devices: None,
            cloud: None,
            head: None,
            controls,
            disabled,
            height: y - column.y,
        }
    }

    /// The page's head: the tile and the name as its title, *Switch to this
    /// profile* at the title line's right end, and the status line as its
    /// lead — then the pointer to Koru Cloud and the notice, when there are.
    fn lay_header(
        &mut self,
        profile: &ProfileSummary,
        column: Rect,
        controls: &mut Vec<Placed>,
    ) -> Header {
        let line = theme::role::title().metrics.line_height;
        let tile = Rect::new(
            column.x,
            column.y + (line - page::TITLE_TILE) * 0.5,
            page::TITLE_TILE,
            page::TITLE_TILE,
        );
        controls.push(Placed {
            control: Control::Tile,
            rect: tile,
            label: "Change icon and color".into(),
            look: Look::Header,
        });
        let switch = (self.c.current() != Some(profile.id)).then(|| {
            let w = button::button_width(&mut self.engine, SWITCH);
            Rect::new(
                column.x + column.w - w,
                column.y + (line - button::H) * 0.5,
                w,
                button::H,
            )
        });
        let indent = page::TITLE_TILE + page::TITLE_TILE_GAP;
        let reserved = switch.map_or(0.0, |rect| rect.w + page::TITLE_TILE_GAP);
        let status_text = self.header_status(profile);
        let head = self.lay_head(column, indent, reserved, &profile.name, &status_text);
        let name_w = self
            .engine
            .simple_layout(&profile.name, theme::role::title())
            .width()
            .clamp(1.0, head.title.w);
        let name = Rect::new(head.title.x, head.title.y, name_w, head.title.h);
        controls.push(Placed {
            control: Control::Name,
            rect: name,
            label: "Rename profile".into(),
            look: Look::Header,
        });
        if let Some(rect) = switch {
            controls.push(Placed {
                control: Control::Switch,
                rect,
                label: SWITCH.into(),
                look: Look::Button(ButtonRole::Secondary),
            });
        }
        let status = head.lead;
        let mut bottom = status.y + status.h;
        if koru_features::cloud_sync() && self.account_wide_unreachable(profile) {
            let w = self
                .engine
                .simple_layout(CLOUD_POINTER, theme::role::meta())
                .width()
                .min(column.w);
            let line = theme::role::meta().metrics.line_height;
            let rect = Rect::new(column.x, bottom + 4.0, w, line);
            bottom = rect.y + rect.h;
            controls.push(Placed {
                control: Control::CloudPointer,
                rect,
                label: CLOUD_POINTER.into(),
                look: Look::Link,
            });
        }
        let notice = self.c.notice().map(str::to_owned).map(|text| {
            let h = KoruTextParagraph::new(&text, theme::role::meta(), "", theme::p().text)
                .measure(&mut self.engine, column.w);
            let rect = Rect::new(column.x, bottom + 12.0, column.w, h);
            (rect, text)
        });
        if let Some((rect, _)) = &notice {
            bottom = rect.y + rect.h;
        }
        Header {
            rect: Rect::new(column.x, column.y, column.w, bottom - column.y),
            tile,
            name,
            name_text: profile.name.clone(),
            text_x: head.title.x,
            text_w: head.title.w,
            status,
            status_text,
            notice,
        }
    }

    /// *Current profile · * and the controller's status line (maintainers' decision
    /// 8: today's vocabulary, nothing invented).
    fn header_status(&self, profile: &ProfileSummary) -> String {
        let line = self.c.status_line(profile);
        if self.c.current() == Some(profile.id) {
            format!("{CURRENT}{line}")
        } else {
            line
        }
    }

    /// The account-wide can't-connect state, which lives on Koru Cloud
    /// (decision 3); a Profile that syncs gets a pointer to it. A Profile
    /// only on this device has nothing to reach.
    fn account_wide_unreachable(&self, profile: &ProfileSummary) -> bool {
        profile.account.is_some()
            && matches!(
                self.c.account().stage,
                AccountStage::Unavailable | AccountStage::Failed
            )
            && self.repair_surface(profile.id).is_none()
    }

    /// The page's attention card: a deletion of this Profile that failed,
    /// whose cleanup here failed, that is still running, or that another
    /// device made.
    fn lay_attention(
        &mut self,
        profile: &ProfileSummary,
        column: Rect,
        y: f32,
        controls: &mut Vec<Placed>,
    ) -> Option<Words> {
        use Attention as A;
        let (title, body, answers): (String, String, Vec<(A, &str)>) = if let Some((found, name)) =
            self.c.discovered_deletion()
            && found == profile.id
        {
            (
                    format!("\u{201c}{name}\u{201d} was deleted from Koru"),
                    "This Profile no longer exists in your Koru account. Its local data is still on this device and is not syncing.".into(),
                    vec![
                        (A::Restore, "Add it again"),
                        (A::RemoveDiscovered, "Remove from this device"),
                        (A::ContinueOffline, "Continue offline"),
                    ],
                )
        } else {
            let (title, body) = self.c.hosted_delete_copy_for(profile.id)?;
            let answers = match self.c.hosted_delete().map(|(_, stage)| stage) {
                Some(HostedDeleteStage::Failed) => vec![
                    (A::RetryDelete, "Try again"),
                    (A::RemoveAfterFailure, "Remove from this device"),
                    (A::CancelFailure, "Cancel"),
                ],
                Some(HostedDeleteStage::CleanupFailed) => vec![
                    (A::FinishCleanup, "Finish removing from this device"),
                    (A::CleanupLater, "Try again later"),
                ],
                _ => Vec::new(),
            };
            (title, body, answers)
        };
        let buttons: Vec<(Control, String, ButtonRole)> = answers
            .into_iter()
            .enumerate()
            .map(|(index, (answer, label))| {
                let role = if index == 0 {
                    ButtonRole::Primary
                } else {
                    ButtonRole::Secondary
                };
                (Control::Attention(answer), label.to_owned(), role)
            })
            .collect();
        Some(self.lay_words(
            Rect::new(column.x, y, column.w, 0.0),
            title,
            body,
            buttons,
            controls,
        ))
    }

    /// A block of words with its buttons under them, padded inside `area`
    /// (whose height is ignored); returns the block with its height.
    pub(super) fn lay_words(
        &mut self,
        area: Rect,
        title: String,
        body: String,
        buttons: Vec<(Control, String, ButtonRole)>,
        controls: &mut Vec<Placed>,
    ) -> Words {
        let p = theme::p();
        let x = area.x + CARD_PAD;
        let w = (area.w - CARD_PAD * 2.0).max(1.0);
        let mut y = area.y + CARD_PAD;
        let title_h = KoruTextParagraph::new(&title, theme::role::section_heading(), "", p.text)
            .measure(&mut self.engine, w);
        let title_at = Rect::new(x, y, w, title_h);
        y += title_h + 6.0;
        let body_h = KoruTextParagraph::new(&body, theme::role::meta(), "", p.text_dim)
            .measure(&mut self.engine, w);
        let body_at = Rect::new(x, y, w, body_h);
        y += body_h;
        if !buttons.is_empty() {
            y += 14.0;
            let mut at = x;
            for (control, label, role) in buttons {
                let bw = button::button_width(&mut self.engine, &label).min(w);
                if at > x && at + bw > x + w {
                    at = x;
                    y += button::H + BUTTON_GAP;
                }
                controls.push(Placed {
                    control,
                    rect: Rect::new(at, y, bw, button::H),
                    label,
                    look: Look::Button(role),
                });
                at += bw + BUTTON_GAP;
            }
            y += button::H;
        }
        y += CARD_PAD;
        Words {
            rect: Rect::new(area.x, area.y, area.w, y - area.y),
            title,
            title_at,
            body,
            body_at,
        }
    }

    fn lay_footer(&mut self, column: Rect, y: f32, controls: &mut Vec<Placed>) -> Rect {
        let w = ActionButton::new(DELETE, ActionStyle::Destructive, None).width(&mut self.engine);
        let rect = Rect::new(column.x, y, w, ActionButton::H);
        controls.push(Placed {
            control: Control::Delete,
            rect,
            label: DELETE.into(),
            look: Look::Action(ActionStyle::Destructive),
        });
        rect
    }

    // ---- input ---------------------------------------------------------------

    /// The control under a point in the tab, if the page has one there. The
    /// last placed wins, so a row's `•••` answers before the row under it.
    pub(super) fn page_hit(&self, x: f32, y: f32) -> Option<Control> {
        if self.in_nav(x, y) || !self.page_area().contains(x, y) {
            return None;
        }
        let y = y + self.page_scroll;
        self.laid
            .controls
            .iter()
            .rev()
            .find(|placed| placed.rect.contains(x, y))
            .map(|placed| placed.control)
    }

    /// The page's controls, in the order Tab visits them.
    pub(super) fn page_focus_order(&self) -> Vec<Control> {
        self.laid
            .controls
            .iter()
            .map(|placed| placed.control)
            .collect()
    }

    /// Scroll the page so the focused control is in view.
    pub(super) fn reveal_page_focus(&mut self) {
        let Some(rect) = self
            .page_focus
            .and_then(|focus| self.laid.placed(focus))
            .map(|placed| placed.rect)
        else {
            return;
        };
        let vh = self.viewport.1;
        let margin = 16.0;
        if rect.y - margin < self.page_scroll {
            self.page_scroll = (rect.y - margin).max(0.0);
        } else if rect.y + rect.h + margin > self.page_scroll + vh {
            self.page_scroll = rect.y + rect.h + margin - vh;
        }
        self.clamp_scroll();
    }

    /// Do what a page control does.
    pub(super) fn activate_control(&mut self, control: Control) -> bool {
        self.page_focus = Some(control);
        self.focus = None;
        if control == Control::CloseCreation {
            return self.close_creation();
        }
        if let Some(done) = self.activate_device_control(control) {
            return done;
        }
        if let Some(done) = self.activate_cloud_control(control) {
            return done;
        }
        let Some(profile) = self.laid.profile else {
            return false;
        };
        // Dimmed: the reason is on the page, and the press has nothing to add.
        if self.laid.disabled.contains(&control) {
            return true;
        }
        if let Some(done) = self.activate_workspace_control(control) {
            return done;
        }
        match control {
            Control::Tile => self.requests.push(Request::ChangeIconAndColor(profile)),
            Control::Name => self.requests.push(Request::Rename(profile)),
            Control::Switch => {
                self.perform(Intent::SwitchTo(profile));
            }
            Control::CloudPointer | Control::GetCloud => self.show(Page::Cloud),
            Control::Attention(answer) => {
                self.perform(answer.intent());
            }
            Control::Repair(index) => {
                let Some(action) = self
                    .repair_surface(profile)
                    .and_then(|surface| surface.actions.get(index).copied())
                else {
                    return false;
                };
                self.perform(Intent::AccountAction(action));
            }
            Control::Resume => {
                self.perform(Intent::ResumeAccountChoice);
            }
            Control::Session => {
                self.perform(Intent::Session);
            }
            Control::Attach => {
                self.perform(Intent::Attach(profile));
            }
            Control::Unlink(index) => self.ask_to_unlink(profile, index),
            Control::AddGoogle => {
                self.perform(Intent::AddGoogle(profile));
            }
            // Offered only beside a single Google Account, where unlinking it
            // is exactly this: the controller asks its offline-only question.
            Control::MakeOffline => {
                self.perform(Intent::Unlink(profile, 0));
            }
            Control::SignOut => self.ask_to_sign_out(),
            Control::CloudSync => {
                self.perform(Intent::CloudSync(profile));
            }
            Control::Delete => self.ask_how_to_delete(profile),
            // The Workspaces section's, answered above.
            Control::ImportWorkspaces
            | Control::NewWorkspace
            | Control::WorkspaceRow(_)
            | Control::WorkspaceMore(_)
            | Control::ShowArchived
            | Control::ShowTrash
            | Control::Unarchive(_)
            | Control::RestoreWorkspace(_)
            | Control::NoticeRetry
            | Control::NoticeDismiss => return false,
            // The New profile page's, answered above.
            Control::CloseCreation => return false,
            // The Linked devices page's, answered above.
            Control::LinkDevice
            | Control::ThisDevice
            | Control::Relay
            | Control::DeviceRow(_)
            | Control::UnlinkDevice(_)
            | Control::HistoryBack
            | Control::HistoryEvent(_)
            | Control::OlderHistory => return false,
            // The Koru Cloud page's, answered above.
            Control::Accounts
            | Control::Unreachable(_)
            | Control::DifferentCreate
            | Control::DifferentSignIn
            | Control::DifferentOffline
            | Control::CloudPrimary
            | Control::CloudLearnMore
            | Control::ProfileSync(_)
            | Control::ProfileSignIn(_)
            | Control::Progress(_)
            | Control::ProgressRow(_) => return false,
        }
        true
    }
}
