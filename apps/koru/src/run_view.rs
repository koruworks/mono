// SPDX-License-Identifier: MPL-2.0

//! **A delegated run as a tab, and the three things only this window can do
//! with it.**
//!
//! `koru-runs-ui` draws Overview, Activity and Result and reads the run
//! through `/v1/runs` on a thread of its own. What it cannot do is anything
//! that needs *this window's session connection*: asking the agent for the
//! controller, giving it back, and opening the session tab the person then
//! types in. All three are one connection's business — the session
//! backend's — because a controller lease belongs to the connection holding
//! it, and a second connection asking for one would be this window
//! competing with itself (§7.4).
//!
//! So this file is the seam, and it is deliberately thin, the way
//! `workspaces.rs` is: hold the surface, tell it the things only a window
//! knows (the language, the clock, who is rating, what the devices are
//! called, where the session handover stands), drain what it asks for, and
//! perform it here.

use koru_core::schema::WorkspaceDoc;
use koru_draw::ImageCache;
use koru_runs_ui::{Request, RunLinks, RunsSurface};
use koru_workspace_host::{Settings, View, WorkspaceStat};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

#[cfg(test)]
mod tests;

pub struct RunView {
    surface: RunsSurface,
    /// The presentation cache key. `IanaZoneRules` is comparable, so a
    /// maintenance pass can prove the device zone is unchanged without
    /// converting the retained epoch again.
    presentation_zone: Option<koru_time_zone::IanaZoneRules>,
    presented_occurrence_ms: Option<i64>,
    #[cfg(test)]
    presentation_counts: PresentationCounts,
}

#[cfg(test)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct PresentationCounts {
    locale_clones: u32,
    zone_clones: u32,
}

impl Default for RunView {
    fn default() -> Self {
        Self::new()
    }
}

impl RunView {
    /// Open the live question through the shared run handover path.
    pub fn respond(&mut self) {
        self.surface.respond();
    }

    pub fn new() -> Self {
        // **Under test it dials nothing.** A dependency is never compiled
        // with `cfg(test)`, so a guard inside `koru-runs-ui` could not stop
        // this binary's suite from asking whatever gateway the machine has
        // running — and this view's suite would be asking it to end runs.
        let links = match cfg!(test) {
            true => RunLinks::inert(),
            // A second gateway client inside this Koru process, so it uses
            // the same explicit channel root as the shell and the Agent. A
            // rooted window that fell back to the ordinary channel would
            // look healthy everywhere except here.
            false => RunLinks::open(
                koru_channel::segment(),
                koru_workspace_host::cli::root_from_args(),
            ),
        };
        let mut surface = RunsSurface::with_links(links);
        surface.set_lang(crate::format::lang());
        // **Whether this platform can show a folder at all**, from the one
        // place that knows. `koru-runs-ui` is chartered not to know which
        // platform it is on, and `reveal`'s own rule is that `is_supported`
        // and `reveal` are written as one pair — so the answer is asked for
        // here rather than guessed there, and *Open location* is simply
        // absent where pressing it would fail.
        surface.set_can_reveal(koru_workspace_host::reveal::is_supported());
        // **Whether *Re-run* is offered**: it starts a delegation, which this
        // build may hide (koru-features) while still showing the run.
        // `begin_delegation` would refuse it anyway; asked here so the
        // control is absent rather than inert.
        surface.set_can_re_run(koru_features::delegate());
        Self {
            surface,
            presentation_zone: None,
            presented_occurrence_ms: None,
            #[cfg(test)]
            presentation_counts: PresentationCounts::default(),
        }
    }

    /// Follow a run. `workspace` is what a run this device did not create
    /// is found by — one that arrived by sync — so it is always given.
    pub fn open(
        &mut self,
        run: koru_delegate_core::RunId,
        workspace: koru_delegate_core::WorkspaceId,
    ) {
        // `RunsSurface::open` clears the projected wall fields. Clear their
        // cache key with them: two different Runs may carry the same epoch,
        // and the first maintenance pass for the new one must repopulate it.
        self.presentation_zone = None;
        self.presented_occurrence_ms = None;
        self.surface.open(run, Some(workspace));
    }

    pub fn run(&self) -> Option<koru_delegate_core::RunId> {
        self.surface.run()
    }

    /// The provider session this run is being driven in, when this device
    /// is driving one. What the container matches a `Handover` event and a
    /// session summary against.
    pub fn session(&self) -> Option<koru_session_protocol::SessionId> {
        self.surface
            .snapshot()
            .and_then(|snapshot| snapshot.session)
    }

    /// What only the host can do. Drained by the container.
    pub fn take_requests(&mut self) -> Vec<Request> {
        self.surface.take_requests()
    }

    /// Take whatever the surface's link has. Answers whether to repaint.
    pub fn poll(&mut self) -> bool {
        self.surface.poll()
    }

    /// Where the session handover stands, as the session host reports it.
    /// **The surface never decides this** (§7.4): a takeover is recorded
    /// when the lease is granted, and the orchestrator is what records it.
    pub fn set_handover(&mut self, handover: koru_session_protocol::HandoverState) {
        self.surface.set_handover(handover);
    }

    /// The wall clock, so every time on screen is relative to now. The
    /// surface reads no clock of its own — a surface with a clock is a
    /// surface with a timer.
    pub fn set_now_ms(&mut self, now_ms: i64) {
        self.surface.set_now_ms(now_ms);
    }

    /// Who a rating is recorded as.
    pub fn set_profile(&mut self, profile: Option<koru_delegate_core::ProfileId>) {
        self.surface.set_profile(profile);
    }

    /// What a device is called today (D16). The run stores an id; the name
    /// is resolved here, at display time, from the Agent's trust catalog.
    pub fn set_device_name(&mut self, device: koru_delegate_core::DeviceId, name: String) {
        self.surface.set_device_name(device, name);
    }

    /// Refresh the reader-local pieces of retained provenance from the
    /// environment the window already samples during maintenance.
    ///
    /// Keeping the explicit zone here means a live device-zone change
    /// reprojects an already-open Run without an API answer, while the leaf
    /// surface still receives only inert fields and formats them through
    /// i18n. Both setters compare before invalidating, so an unchanged
    /// maintenance pass neither asks for repaint nor starts a reflow loop.
    pub fn maintain_presentation(
        &mut self,
        locale: &koru_i18n::time::LocaleFormat,
        zone: &koru_time_zone::IanaZoneRules,
    ) -> bool {
        self.maintain_presentation_with_rules(locale, zone, zone)
    }

    fn maintain_presentation_with_rules(
        &mut self,
        locale: &koru_i18n::time::LocaleFormat,
        zone: &koru_time_zone::IanaZoneRules,
        rules: &dyn koru_core::time::ZoneRules,
    ) -> bool {
        let locale_changed = self.surface.set_locale(locale);
        #[cfg(test)]
        if locale_changed {
            self.presentation_counts.locale_clones += 1;
        }
        let occurrence_ms = self.surface.snapshot().and_then(|snapshot| {
            let koru_delegate_core::RunOrigin::Scheduled { occurrence_ms, .. } =
                snapshot.header.origin
            else {
                return None;
            };
            Some(occurrence_ms)
        });
        // No scheduled provenance means there is no zone-dependent value to
        // cache or convert. If one used to be present, clear it exactly once.
        let Some(occurrence_ms) = occurrence_ms else {
            if self.presented_occurrence_ms.is_none() {
                return locale_changed;
            }
            self.presented_occurrence_ms = None;
            let occurrence_changed = self.surface.set_occurrence_local(None);
            return locale_changed || occurrence_changed;
        };

        let zone_changed = self.presentation_zone.as_ref() != Some(zone);
        if !zone_changed && self.presented_occurrence_ms == Some(occurrence_ms) {
            return locale_changed;
        }

        let local =
            crate::format::parts_in_zone(occurrence_ms, rules).map(|parts| (occurrence_ms, parts));
        let occurrence_changed = self.surface.set_occurrence_local(local);
        if zone_changed {
            match &mut self.presentation_zone {
                Some(cached) => cached.clone_from(zone),
                None => self.presentation_zone = Some(zone.clone()),
            }
            #[cfg(test)]
            {
                self.presentation_counts.zone_clones += 1;
            }
        }
        self.presented_occurrence_ms = Some(occurrence_ms);
        locale_changed || occurrence_changed
    }

    #[cfg(test)]
    pub(crate) fn surface_mut(&mut self) -> &mut RunsSurface {
        &mut self.surface
    }
}

impl View for RunView {
    fn key(
        &mut self,
        _ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        self.surface.key(key, text, mods)
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, _ctrl: bool) -> bool {
        self.surface.mouse_down(x, y)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.surface.mouse_move(x, y)
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        self.surface.mouse_up()
    }

    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, _x: f32, _y: f32) -> bool {
        false
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        self.surface.scroll_by(delta);
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.surface.set_viewport(w, h);
    }

    fn cursor_icon(&self) -> CursorIcon {
        self.surface.cursor_icon()
    }

    /// A run is not a document, and nothing here reads the open one. The
    /// run's authority is its own document, read through the gateway.
    fn adopt(&mut self, _ws: &WorkspaceDoc) {}

    fn reseat(&mut self, _ws: &WorkspaceDoc) {}

    fn invalidate(&mut self) {
        self.surface.invalidate();
    }

    fn set_workspace_stats(&mut self, _stats: Vec<WorkspaceStat>) -> bool {
        false
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        _ws: &koru_workspace::Workspace,
        _images: &ImageCache,
        root: Affine,
        _caret_on: bool,
        _pointer: (f32, f32),
    ) {
        self.surface.draw(scene, root);
    }

    fn restore(&mut self, _ws: &koru_workspace::Workspace, _settings: &Settings) {}

    fn capture(&self, _settings: &mut Settings) {}
}
