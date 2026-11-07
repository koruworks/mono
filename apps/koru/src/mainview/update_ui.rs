// SPDX-License-Identifier: MPL-2.0

//! The update banner: what the agent is doing about updating, and the two
//! moments it asks a person.
//!
//! **This window draws the conversation and holds none of it.** `koru-agent`
//! checks, downloads, verifies and installs; what arrives here is an
//! [`UpdateReport`], whole, on every change, and every button sends one
//! [`UpdateChoice`] back. There is no state machine on this side to fall out of
//! step with the agent's — the banner is a function of the last report.
use koru_agent_wire::{UpdateChoice, UpdateReport, UpdateStage};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use koru_ui::button::{self, ButtonRole};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, NamedKey};
use winit::window::CursorIcon;

use std::time::{Duration, Instant};

use super::MainView;

/// How long a clicked consent claims the banner while the agent's answer is
/// in flight. Generous against a real round-trip (normally milliseconds);
/// finite so an agent that died mid-choice cannot leave the banner deaf until
/// the window restarts.
const UPDATE_ACTION_TIMEOUT: Duration = Duration::from_secs(10);

/// The banner line while a consent is on its way to the agent.
pub(super) const ASKING_AGENT: &str = "Asking Koru's agent…";
/// The consent round-trip timed out. The button re-arms beside this line, so
/// "try again" is an instruction the person can actually follow.
pub(super) const AGENT_SILENT: &str = "Koru's agent did not answer. Try again, or restart Koru.";
/// The click could not even be queued — no agent, or a link the reader has
/// already recorded as gone.
pub(super) const AGENT_GONE: &str = "Koru's agent is not connected. Restart Koru to update.";

const MARGIN: f32 = 16.0;
const BUTTON_H: f32 = 30.0;
/// Title baseline to the first detail line.
const TITLE_H: f32 = 44.0;
const LINE_H: f32 = 19.0;
const LINES_MAX: usize = 3;
const PAD_BOTTOM: f32 = 11.0;
/// Detail lines to the button row, and the button row to the bottom edge.
const BUTTON_GAP: f32 = 11.0;

const MODAL_WIDTH: f32 = 700.0;
const MODAL_HEIGHT: f32 = 300.0;
const MODAL_PAD: f32 = 20.0;
const MODAL_RADIUS: f32 = 10.0;
const MODAL_EDGE: f32 = 24.0;
const MODAL_TITLE: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::UI_BODY, theme::FontWeight::SemiBold);
const MODAL_BODY: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::SM, theme::FontWeight::Medium);

/// `pub(super)` for the tests only: what a button *sends* is the interesting
/// half of the two-step consent, and the honest way to assert it is to find the
/// button's rect and press it rather than to call the handler with a number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Action {
    Primary,
    Later,
    Changes,
    Cancel,
    Retry,
}

fn updates_modal_rects(viewport: (f32, f32)) -> (Rect, Rect, Rect, Rect) {
    let w = MODAL_WIDTH.min((viewport.0 - MODAL_EDGE * 2.0).max(0.0));
    let h = MODAL_HEIGHT.min((viewport.1 - MODAL_EDGE * 2.0).max(0.0));
    let panel = Rect::new((viewport.0 - w) * 0.5, (viewport.1 - h) * 0.5, w, h);
    let download = Rect::new(
        panel.x + MODAL_PAD,
        panel.y + panel.h - 104.0,
        224.0,
        BUTTON_H,
    );
    let row_y = panel.y + panel.h - 58.0;
    let row_w = ((panel.w - MODAL_PAD * 2.0 - 12.0) * 0.5).max(0.0);
    let automatic = Rect::new(panel.x + MODAL_PAD, row_y, row_w, BUTTON_H);
    let check = Rect::new(panel.x + MODAL_PAD + row_w + 12.0, row_y, row_w, BUTTON_H);
    (panel, download, automatic, check)
}

impl MainView {
    pub(super) fn updates_modal_key(&mut self, key: &Key) -> bool {
        if !self.updates_modal {
            return false;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.updates_modal = false;
        }
        true
    }

    pub(super) fn updates_modal_mouse_down(&mut self, x: f32, y: f32) -> bool {
        if !self.updates_modal {
            return false;
        }
        let (_, download, automatic, check) = updates_modal_rects(self.viewport);
        if download.contains(x, y) {
            let _ = koru_editor::browser::open(crate::welcome::DOWNLOAD_URL);
            return true;
        }
        if automatic.contains(x, y) {
            if let Some(report) = self.updates.report() {
                let _ = self.updates.choose(UpdateChoice::SetAutomatic {
                    on: !report.automatic_checks,
                });
            }
            return true;
        }
        if check.contains(x, y) {
            let _ = self.updates.choose(UpdateChoice::CheckNow);
            return true;
        }
        let (panel, ..) = updates_modal_rects(self.viewport);
        if !panel.contains(x, y) {
            // Clicking away closes the modal — but on the *release*, not here:
            // dismissing on mouse-down let the matching mouse-up reach the
            // document beneath (ADR-150).
            self.updates_modal_outside_press = true;
        }
        true
    }

    /// The header's update-management page, presented as a compact modal so it
    /// is available from every tab without creating another special document.
    pub(super) fn draw_updates_modal(&mut self, scene: &mut Scene, root: Affine) {
        if !self.updates_modal {
            return;
        }
        let viewport = self.viewport;
        let p = theme::p();
        scene::fill(
            scene,
            root,
            p.scrim,
            &scene::rect(0.0, 0.0, viewport.0, viewport.1),
        );
        let (panel, download, automatic_rect, check_rect) = updates_modal_rects(viewport);
        scene::floating_panel(
            scene,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(MODAL_RADIUS),
        );
        let title = self.engine.simple_layout("Updates", MODAL_TITLE);
        scene::draw_layout(
            scene,
            root,
            &title,
            panel.x + MODAL_PAD,
            panel.y + MODAL_PAD,
            Some(p.text),
        );
        let info = koru_channel::build_info();
        let token = self
            .updates
            .report()
            .map(|report| report.channel)
            .unwrap_or_else(|| {
                koru_channel::ReleaseChannel::of_this_build()
                    .as_str()
                    .to_owned()
            });
        let stamp =
            crate::welcome::build_stamp().map_or(String::new(), |stamp| format!(" · {stamp}"));
        let note = crate::welcome::channel_note(&token)
            .map(|note| format!("\n{note}"))
            .unwrap_or_default();
        let body = format!(
            "Channel: {}\nCurrent: {} · build {}{stamp} · {}{note}\n\nKoru updates within its channel. Feedback: hello@koru.do",
            crate::welcome::channel_label(&token),
            info.version,
            info.build_number,
            info.build_label(),
        );
        let text = self.engine.simple_layout_wrapped(
            &body,
            MODAL_BODY,
            (panel.w - MODAL_PAD * 2.0).max(80.0),
        );
        scene::draw_layout(
            scene,
            root,
            &text,
            panel.x + MODAL_PAD,
            panel.y + MODAL_PAD + title.height() + 10.0,
            Some(p.text_dim),
        );
        let automatic = self
            .updates
            .report()
            .is_some_and(|report| report.automatic_checks);
        button::draw_button(
            scene,
            root,
            &mut self.engine,
            download,
            "Download another channel",
            ButtonRole::Secondary,
            false,
            true,
        );
        button::draw_button(
            scene,
            root,
            &mut self.engine,
            automatic_rect,
            if automatic {
                "Automatic checks: On"
            } else {
                "Automatic checks: Off"
            },
            ButtonRole::Secondary,
            false,
            self.updates.report().is_some(),
        );
        button::draw_button(
            scene,
            root,
            &mut self.engine,
            check_rect,
            "Check for updates",
            ButtonRole::Primary,
            false,
            self.updates.report().is_some(),
        );
    }

    /// Tall enough for what this report actually says, and no taller. A fixed
    /// height reserved a button row for every stage, so stages with no
    /// buttons sat over the document as a mostly-empty panel.
    fn banner_height(&self, report: &UpdateReport) -> f32 {
        let lines = self.banner_text(report).1.len().min(LINES_MAX) as f32;
        let content = TITLE_H + lines * LINE_H;
        if self.action_specs(report).is_empty() {
            content + PAD_BOTTOM
        } else {
            content + BUTTON_GAP + BUTTON_H + BUTTON_GAP
        }
    }

    /// The top of the update banner, if one is showing — what anything else
    /// floating at the bottom of the window has to stay clear of.
    pub(super) fn update_banner_top(&self) -> Option<f32> {
        let report = self.visible_report()?;
        Some(self.banner_rect(&report).y)
    }

    fn banner_rect(&self, report: &UpdateReport) -> Rect {
        let width = (self.viewport.0 - MARGIN * 2.0).min(680.0).max(0.0);
        let height = self.banner_height(report);
        Rect::new(
            (self.viewport.0 - width) * 0.5,
            (self.viewport.1 - height - MARGIN).max(theme::CAPTION_H + MARGIN),
            width,
            height,
        )
    }

    /// Separate from `actions` because `banner_rect` needs to know whether
    /// there are any before it can give them somewhere to sit.
    ///
    /// **The two primary labels name their own step.** On
    /// [`UpdateStage::Available`] the button says *Download update* and sends
    /// [`UpdateChoice::Download`]; on [`UpdateStage::Ready`] it says *Restart
    /// and install* and sends [`UpdateChoice::Install`]. They used to share one
    /// label — *Update and restart* — which made the honest sequence read as a
    /// failure: click, progress bar, and then what looked like the same banner
    /// asking the same question again. The agent still enforces the order
    /// rather than trusting the drawing, so a banner cannot skip a step even by
    /// sending the message for the one it wants.
    ///
    /// `can_install` is **the agent's answer, not ours**. It used to be a probe
    /// of the directory beside *this* executable, which answers a question about
    /// this process's privileges; the agent is what will do the replacing.
    fn action_specs(&self, report: &UpdateReport) -> &'static [(Action, &'static str, f32)] {
        match report.stage {
            UpdateStage::Available if report.can_install => &[
                (Action::Primary, "Download update", 136.0),
                (Action::Later, "Later", 76.0),
                (Action::Changes, "View all changes", 136.0),
            ],
            UpdateStage::Available => &[
                (Action::Primary, "Download manually", 142.0),
                (Action::Later, "Later", 76.0),
                (Action::Changes, "View all changes", 136.0),
            ],
            UpdateStage::Downloading => &[(Action::Cancel, "Cancel", 82.0)],
            UpdateStage::Ready => &[
                (Action::Primary, "Restart and install", 154.0),
                (Action::Later, "Later", 76.0),
                (Action::Changes, "View all changes", 136.0),
            ],
            UpdateStage::Failed if self.update_was_offered => {
                &[(Action::Retry, "Try again", 100.0)]
            }
            _ => &[],
        }
    }

    pub(super) fn actions(&self, report: &UpdateReport) -> Vec<(Action, &'static str, Rect)> {
        let banner = self.banner_rect(report);
        let y = banner.y + banner.h - BUTTON_H - BUTTON_GAP;
        let mut x = banner.x + 18.0;
        self.action_specs(report)
            .iter()
            // A release that named no notes page gets no button, rather than a
            // button that does nothing — the report says so by carrying `None`.
            .filter(|(action, _, _)| {
                !matches!(action, Action::Changes) || report.notes_url.is_some()
            })
            .map(|(action, label, width)| {
                let rect = Rect::new(x, y, *width, BUTTON_H);
                x += *width + 8.0;
                (*action, *label, rect)
            })
            .collect()
    }

    /// Bottom banner only — the "installed build is newer" notice is a modal.
    fn visible_report(&self) -> Option<UpdateReport> {
        let report = self.updates.report()?;
        match report.stage {
            UpdateStage::Available
            | UpdateStage::Downloading
            | UpdateStage::Verifying
            | UpdateStage::Ready => Some(report),
            UpdateStage::Failed if self.update_was_offered => Some(report),
            _ => None,
        }
    }

    /// True while the informational "newer than channel" modal should own input.
    pub(super) fn update_newer_modal_open(&self) -> bool {
        self.updates
            .report()
            .is_some_and(|report| report.stage == UpdateStage::InstalledNewer)
    }

    fn newer_modal_copy(&self) -> Option<(String, String)> {
        let report = self.updates.report()?;
        if report.stage != UpdateStage::InstalledNewer {
            return None;
        }
        let (title, lines) = banner_copy(&report);
        Some((title, lines.into_iter().next().unwrap_or_default()))
    }

    pub(super) fn update_newer_key(&mut self, key: &Key) -> bool {
        if !self.update_newer_modal_open() {
            return false;
        }
        match key {
            Key::Named(NamedKey::Escape | NamedKey::Enter) => {
                self.dismiss_update_newer();
            }
            _ => {}
        }
        // Modal: swallow everything so the document does not take dictation.
        true
    }

    pub(super) fn update_newer_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let Some((title, body)) = self.newer_modal_copy() else {
            return false;
        };
        let ok = newer_modal_ok_rect(&mut self.engine, self.viewport, &title, &body);
        if ok.contains(x, y) {
            self.dismiss_update_newer();
        }
        // Press outside does not dismiss — same register as ErrorDialog / Confirm.
        true
    }

    pub(super) fn update_newer_mouse_move(&mut self, x: f32, y: f32) -> bool {
        let Some((title, body)) = self.newer_modal_copy() else {
            if self.update_newer_ok_hot {
                self.update_newer_ok_hot = false;
                return true;
            }
            return false;
        };
        let hot =
            newer_modal_ok_rect(&mut self.engine, self.viewport, &title, &body).contains(x, y);
        if hot != self.update_newer_ok_hot {
            self.update_newer_ok_hot = hot;
            true
        } else {
            false
        }
    }

    pub(super) fn update_newer_cursor(&self) -> Option<CursorIcon> {
        if !self.update_newer_modal_open() {
            return None;
        }
        Some(if self.update_newer_ok_hot {
            koru_ui::cursor::PRESS
        } else {
            CursorIcon::Default
        })
    }

    fn dismiss_update_newer(&mut self) {
        let _ = self.updates.choose(UpdateChoice::DismissNewer);
        self.update_newer_ok_hot = false;
    }

    /// Whether a consent for `stage` is still in flight. Clears the mark when
    /// the report moved past it, and when it has waited longer than an agent
    /// round-trip plausibly takes - the answer is not coming, so the buttons
    /// re-arm rather than staying deaf until a restart.
    pub(super) fn update_action_in_flight(&mut self, stage: UpdateStage) -> bool {
        match self.update_action_pending {
            Some((pending, at)) => {
                if pending != stage {
                    // The report moved past the consent — it was heard.
                    self.update_action_pending = None;
                    self.update_action_stalled = None;
                    false
                } else if at.elapsed() >= UPDATE_ACTION_TIMEOUT {
                    // The answer is not coming. Re-arm the buttons, and say so
                    // — a button that silently turns blue again reads as a
                    // reset, not a failure.
                    self.update_action_pending = None;
                    self.update_action_stalled = Some((stage, AGENT_SILENT));
                    false
                } else {
                    true
                }
            }
            None => false,
        }
    }

    /// Send a consent and remember its fate. Pending while the round-trip is
    /// in flight; an immediate note when the link is already gone — a dropped
    /// click must not spend ten seconds pretending to be a slow one.
    pub(super) fn update_consent(&mut self, choice: UpdateChoice, stage: UpdateStage) {
        let accepted = self.updates.choose(choice);
        // The window-side date of the click, to line up against the agent's
        // "update choice arrived". Unconditional: a consent is rare, and a
        // stalled one used to be undatable from this side.
        tracing::info!(target: "koru::update", ?choice, ?stage, accepted, "consent sent");
        if accepted {
            self.update_action_pending = Some((stage, Instant::now()));
            self.update_action_stalled = None;
        } else {
            self.update_action_stalled = Some((stage, AGENT_GONE));
        }
    }

    /// The banner's copy plus this window's own state: a consent in flight is
    /// said out loud, and one that stalled is admitted. The note always fits —
    /// the release notes give up a line before the fate of a click does.
    pub(super) fn banner_text(&self, report: &UpdateReport) -> (String, Vec<String>) {
        let (title, mut lines) = banner_copy(report);
        let note = if self.update_action_pending.map(|(stage, _)| stage) == Some(report.stage) {
            Some(ASKING_AGENT)
        } else {
            self.update_action_stalled
                .filter(|(stage, _)| *stage == report.stage)
                .map(|(_, note)| note)
        };
        if let Some(note) = note {
            lines.truncate(LINES_MAX - 1);
            lines.push(note.to_string());
        }
        (title, lines)
    }

    pub(super) fn update_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let Some(report) = self.visible_report() else {
            return false;
        };
        if !self.banner_rect(&report).contains(x, y) {
            return false;
        }
        // A choice travels to the agent over a writer thread. Keep claiming the
        // banner while that round-trip is in flight, and do not let a second
        // press enqueue another install.
        if self.update_action_in_flight(report.stage) {
            return true;
        }
        let action = self
            .actions(&report)
            .into_iter()
            .find(|(_, _, rect)| rect.contains(x, y))
            .map(|(action, _, _)| action);
        match action {
            // The two consent steps, and the one thing this window does itself:
            // when the agent cannot replace the installation, the only honest
            // button opens the download page.
            Some(Action::Primary) => match report.stage {
                UpdateStage::Available if report.can_install => {
                    self.update_consent(UpdateChoice::Download, report.stage);
                }
                UpdateStage::Available => {
                    if let Some(url) = report.download_url.as_deref() {
                        let _ = koru_editor::browser::open(url);
                    }
                }
                UpdateStage::Ready => {
                    if self.updates.is_test_upgrade() {
                        self.window_request = Some(koru_window::WindowRequest::TestUpgrade);
                        self.update_action_pending = Some((report.stage, Instant::now()));
                    } else {
                        self.update_consent(
                            UpdateChoice::Install {
                                session_generation: report.session_generation,
                            },
                            report.stage,
                        );
                    }
                }
                _ => {}
            },
            Some(Action::Later) => {
                let _ = self.updates.choose(UpdateChoice::Later);
            }
            Some(Action::Changes) => {
                if let Some(url) = report.notes_url.as_deref() {
                    let _ = koru_editor::browser::open(url);
                }
            }
            Some(Action::Cancel) => {
                let _ = self.updates.choose(UpdateChoice::Cancel);
            }
            Some(Action::Retry) => {
                let _ = self.updates.choose(UpdateChoice::Retry);
            }
            None => {}
        }
        true
    }

    /// The banner is a floating surface, not decoration on the document below
    /// it. Claiming movement prevents the editor from keeping its I-beam, row
    /// hovers, and drag affordances active through the panel.
    pub(super) fn update_mouse_move(&mut self, x: f32, y: f32) -> bool {
        let Some(report) = self.visible_report() else {
            return false;
        };
        let _ = self.update_action_in_flight(report.stage);
        self.banner_rect(&report).contains(x, y)
    }

    /// The wheel has no coordinates of its own, so the container routes it by
    /// the last pointer position. A banner under that pointer owns it too.
    pub(super) fn update_contains_pointer(&self) -> bool {
        self.visible_report().is_some_and(|report| {
            self.banner_rect(&report)
                .contains(self.pointer.0, self.pointer.1)
        })
    }

    pub(super) fn update_cursor(&self) -> Option<CursorIcon> {
        let report = self.visible_report()?;
        if !self
            .banner_rect(&report)
            .contains(self.pointer.0, self.pointer.1)
        {
            return None;
        }
        let on_action = self
            .actions(&report)
            .into_iter()
            .any(|(_, _, rect)| rect.contains(self.pointer.0, self.pointer.1));
        Some(
            if on_action && self.update_action_pending.map(|(stage, _)| stage) != Some(report.stage)
            {
                koru_ui::cursor::PRESS
            } else {
                CursorIcon::Default
            },
        )
    }

    pub(super) fn draw_update_banner(&mut self, scene: &mut Scene, root: Affine) {
        let Some(report) = self.visible_report() else {
            return;
        };
        let _ = self.update_action_in_flight(report.stage);
        let p = theme::p();
        let banner = self.banner_rect(&report);
        scene::fill(
            scene,
            root,
            p.panel_bg,
            &scene::rrect(banner.x, banner.y, banner.w, banner.h, 10.0),
        );
        scene::stroke(
            scene,
            root,
            p.panel_border,
            theme::BORDER_W,
            &scene::rrect(banner.x, banner.y, banner.w, banner.h, 10.0),
        );
        let (title, lines) = self.banner_text(&report);
        // Release-notes lines are whole sentences from the manifest and can be
        // wider than the panel; each keeps its single line and is elided to
        // fit, because the height budget is a line per entry.
        let text_w = (banner.w - 36.0).max(40.0);
        let title = self.engine.elided(&title, theme::role::ui(), text_w);
        let title = self.engine.simple_layout(&title, theme::role::ui());
        scene::draw_layout(
            scene,
            root,
            &title,
            banner.x + 18.0,
            banner.y + 15.0,
            Some(p.text),
        );
        let mut y = banner.y + TITLE_H;
        for line in lines.into_iter().take(LINES_MAX) {
            let line = self.engine.elided(&line, theme::role::meta(), text_w);
            let layout = self.engine.simple_layout(&line, theme::role::meta());
            scene::draw_layout(scene, root, &layout, banner.x + 18.0, y, Some(p.text_dim));
            y += 19.0;
        }
        for (action, label, rect) in self.actions(&report) {
            let primary = matches!(action, Action::Primary);
            let enabled = self.update_action_pending.map(|(stage, _)| stage) != Some(report.stage);
            scene::fill(
                scene,
                root,
                if primary && enabled {
                    p.accent
                } else {
                    p.chip_bg
                },
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 5.0),
            );
            if !primary {
                scene::stroke(
                    scene,
                    root,
                    p.panel_border,
                    theme::BORDER_W,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 5.0),
                );
            }
            let layout = self.engine.simple_layout(label, theme::role::meta());
            scene::draw_layout(
                scene,
                root,
                &layout,
                rect.x + (rect.w - layout.width()) * 0.5,
                rect.y + (rect.h - layout.height()) * 0.5,
                Some(if primary && enabled {
                    p.on_accent
                } else {
                    p.text_dim
                }),
            );
        }
    }

    /// Scrim + centred panel for "this build is newer than the selected channel".
    pub(super) fn draw_update_newer_modal(&mut self, scene: &mut Scene, root: Affine) {
        let Some((title, body)) = self.newer_modal_copy() else {
            return;
        };
        let hot = self.update_newer_ok_hot;
        let viewport = self.viewport;
        let p = theme::p();
        scene::fill(
            scene,
            root,
            p.scrim,
            &scene::rect(0.0, 0.0, viewport.0, viewport.1),
        );
        let panel = newer_modal_panel(&mut self.engine, viewport, &title, &body);
        scene::floating_panel(
            scene,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(MODAL_RADIUS),
        );
        let text_w = (panel.w - MODAL_PAD * 2.0).max(80.0);
        let tx = panel.x + MODAL_PAD;
        let ty = panel.y + MODAL_PAD;
        let title_layout = self
            .engine
            .simple_layout_wrapped(&title, MODAL_TITLE, text_w);
        scene::draw_layout(scene, root, &title_layout, tx, ty, Some(p.text));
        let body_y = ty + title_layout.height() + 8.0;
        let body_layout = self.engine.simple_layout_wrapped(&body, MODAL_BODY, text_w);
        scene::draw_layout(scene, root, &body_layout, tx, body_y, Some(p.text_dim));
        let ok = newer_modal_ok_rect(&mut self.engine, viewport, &title, &body);
        button::draw_button(
            scene,
            root,
            &mut self.engine,
            ok,
            "OK",
            ButtonRole::Primary,
            hot,
            true,
        );
    }
}

fn newer_modal_panel(
    engine: &mut TextEngine,
    viewport: (f32, f32),
    title: &str,
    body: &str,
) -> Rect {
    let w = MODAL_WIDTH.min(viewport.0 - MODAL_EDGE * 2.0).max(0.0);
    let text_w = (w - MODAL_PAD * 2.0).max(80.0);
    let title_h = engine
        .simple_layout_wrapped(title, MODAL_TITLE, text_w)
        .height();
    let body_h = engine
        .simple_layout_wrapped(body, MODAL_BODY, text_w)
        .height();
    let h = (MODAL_PAD + title_h + 8.0 + body_h + 18.0 + button::H + MODAL_PAD)
        .min(viewport.1 - MODAL_EDGE * 2.0);
    Rect::new((viewport.0 - w) * 0.5, (viewport.1 - h) * 0.5, w, h)
}

fn newer_modal_ok_rect(
    engine: &mut TextEngine,
    viewport: (f32, f32),
    title: &str,
    body: &str,
) -> Rect {
    let panel = newer_modal_panel(engine, viewport, title, body);
    let bw = button::button_width(engine, "OK");
    Rect::new(
        panel.x + panel.w - MODAL_PAD - bw,
        panel.y + panel.h - MODAL_PAD - button::H,
        bw,
        button::H,
    )
}

/// The heading and the detail lines, straight off the report.
///
/// The notes' **title** leads the detail lines on an offer. It is the "what's
/// new" a person reads before consenting, and it is present from
/// [`UpdateStage::Available`] onwards rather than appearing at install time —
/// which is the whole point of asking twice.
pub(super) fn banner_copy(report: &UpdateReport) -> (String, Vec<String>) {
    match report.stage {
        UpdateStage::Available => (
            format!("{} is available", offered_build(report)),
            notes_lines(report),
        ),
        UpdateStage::Downloading => (
            "Downloading Koru".into(),
            vec![format!(
                "{}% · {:.1} MB of {:.1} MB",
                report.received.saturating_mul(100) / report.total.max(1),
                report.received as f64 / 1_048_576.0,
                report.total as f64 / 1_048_576.0
            )],
        ),
        UpdateStage::Verifying => (
            "Verifying update".into(),
            vec!["Checking the package signature and archive.".into()],
        ),
        // **Closing is no longer the person's job.** This used to read "Save
        // your work, close other Koru instances, then restart", which was true
        // when the window installed its own update and had to be the last one
        // standing. The agent drives the swap and asks every window to close
        // first, so the sentence says what will happen rather than what to do.
        UpdateStage::Ready => (
            format!("{} is ready", offered_build(report)),
            if report.active_session_count == 0 {
                vec!["Koru will close and restart to install this.".into()]
            } else {
                vec![koru_i18n::fill(
                    koru_i18n::t(
                        crate::format::lang(),
                        koru_i18n::Key::UpdateReadyWithSessions,
                    ),
                    &[&report.active_session_count.to_string()],
                )]
            },
        ),
        UpdateStage::InstalledNewer => (
            "This Koru build is newer than the selected channel".into(),
            vec![format!(
                "The selected channel currently points to build {}; Koru will not downgrade automatically.",
                report.build_number
            )],
        ),
        UpdateStage::Failed => (
            report.message.clone().unwrap_or_default(),
            report.detail.clone().into_iter().collect(),
        ),
        _ => (String::new(), Vec::new()),
    }
}

/// What the offered build is called: `Koru {version} · build {n}` — the same
/// spelling as the agent tray's About row, so the two surfaces agree. A report
/// with no allocated number says only its version, the About row's own guard.
fn offered_build(report: &UpdateReport) -> String {
    match report.build_number {
        0 => format!("Koru {}", report.version),
        n => format!("Koru {} · build {n}", report.version),
    }
}

fn notes_lines(report: &UpdateReport) -> Vec<String> {
    let mut lines = Vec::with_capacity(report.summary.len() + 1);
    if !report.title.is_empty() {
        lines.push(report.title.clone());
    }
    lines.extend(report.summary.iter().cloned());
    lines
}
