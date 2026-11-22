// SPDX-License-Identifier: MPL-2.0

//! `client.settings()` — the typed namespace every window uses.
//!
//! **A window must never wait on its settings to draw.** The bounded wait
//! below is the whole reason this namespace has a shape beyond "read and
//! patch": a window asks for a section, gets the registry's defaults back
//! immediately, renders, and swaps in the authoritative values when they land.
//! If the Gateway does not answer within [`koru_settings::STARTUP_WAIT_MILLIS`]
//! the window stays on defaults — never a hang, and never a splash screen
//! while somebody's editor waits on a preferences file.
//!
//! **A write needs a scope a default token does not hold.** `settings.write`
//! is out of the ordinary default set, so a caller that intends to change
//! settings names it at connect time:
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let client = koru_api_client::Client::ensure_with_scopes(
//!     "internal-debug",
//!     &[
//!         koru_settings::SETTINGS_READ_SCOPE,
//!         koru_settings::SETTINGS_WRITE_SCOPE,
//!     ],
//! )?;
//! let global = client.settings().global()?;
//! # let _ = global;
//! # Ok(())
//! # }
//! ```
//!
//! Asking for scopes *filters* rather than adds, so a caller that names these
//! two holds only these two — name every scope the process needs in one call.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use koru_settings::{
    AiPatch, AiSettings, GlobalPatch, GlobalSettings, Section, TerminalPatch, TerminalSettings,
};
use koru_store_client::http::RetryPolicy;

use crate::{Client, events::Subscription};

pub mod link;

#[cfg(test)]
mod tests;

pub type SettingsResult<T> = Result<T, SettingsError>;

/// What went wrong, in the authority's own vocabulary.
///
/// **A refusal is not a transport failure**, and a caller that wants to say
/// "that value is not allowed" rather than "settings are unavailable" needs to
/// be able to tell them apart.
///
/// After a [`SettingsError::Api`] on a patch, **read the section again**. The
/// authority re-reads the files before it answers, so a refetch is what is
/// actually true — which is the only thing a control that could not save may
/// go on showing.
#[derive(Debug)]
pub enum SettingsError {
    /// Discovery, authentication, connection, or response decoding.
    Transport(koru_store_core::errors::Error),
    /// A structured refusal from the settings routes.
    Api(SettingsApiFailure),
    /// Rejected before anything crossed the process boundary.
    Local(String),
}

#[derive(Debug)]
pub struct SettingsApiFailure {
    pub status: u16,
    /// `permission_denied`, `invalid_request`, `settings_write_failed`,
    /// `capability_unavailable`, `not_found`.
    pub code: String,
    pub message: String,
}

impl SettingsError {
    /// Whether the session simply may not do this.
    ///
    /// The common case a Settings surface has to render differently: a window
    /// without `settings.write` shows its controls read-only rather than
    /// showing an error.
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Self::Api(failure) if failure.code == "permission_denied")
    }
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => error.fmt(formatter),
            Self::Api(failure) => write!(formatter, "{}: {}", failure.code, failure.message),
            Self::Local(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SettingsError {}

pub(crate) fn map_public(error: koru_store_client::http::PublicError) -> SettingsError {
    match error {
        koru_store_client::http::PublicError::Transport(error) => SettingsError::Transport(error),
        koru_store_client::http::PublicError::Failure(failure) => {
            SettingsError::Api(SettingsApiFailure {
                status: failure.status,
                code: failure.code,
                message: failure.message,
            })
        }
    }
}

/// The typed settings namespace.
pub struct SettingsClient<'a> {
    pub(crate) client: &'a Client,
}

impl SettingsClient<'_> {
    // ------------------------------------------------------------ reading

    pub fn global(&self) -> SettingsResult<GlobalSettings> {
        match self.read(Section::Global)? {
            SectionBody::Global(settings) => Ok(settings),
            other => Err(mismatched(Section::Global, other)),
        }
    }

    pub fn terminal(&self) -> SettingsResult<TerminalSettings> {
        match self.read(Section::Terminal)? {
            SectionBody::Terminal(settings) => Ok(settings),
            other => Err(mismatched(Section::Terminal, other)),
        }
    }

    pub fn ai(&self) -> SettingsResult<AiSettings> {
        match self.read(Section::Ai)? {
            SectionBody::Ai(settings) => Ok(settings),
            other => Err(mismatched(Section::Ai, other)),
        }
    }

    // ------------------------------------------------------------ writing

    pub fn patch_global(&self, patch: &GlobalPatch) -> SettingsResult<GlobalSettings> {
        match self.patch(Section::Global, patch)? {
            SectionBody::Global(settings) => Ok(settings),
            other => Err(mismatched(Section::Global, other)),
        }
    }

    pub fn patch_terminal(&self, patch: &TerminalPatch) -> SettingsResult<TerminalSettings> {
        match self.patch(Section::Terminal, patch)? {
            SectionBody::Terminal(settings) => Ok(settings),
            other => Err(mismatched(Section::Terminal, other)),
        }
    }

    pub fn patch_ai(&self, patch: &AiPatch) -> SettingsResult<AiSettings> {
        match self.patch(Section::Ai, patch)? {
            SectionBody::Ai(settings) => Ok(settings),
            other => Err(mismatched(Section::Ai, other)),
        }
    }

    // ------------------------------------------------------- the bounded wait

    /// Ask for a section without blocking, and take the defaults meanwhile.
    ///
    /// The read runs on its own thread and retries until it succeeds or the
    /// budget runs out; the caller renders [`Pending::defaults`] straight away
    /// and polls [`Pending::take`] on whatever cadence it already has — a
    /// frame, a tick, an event. **Nothing here blocks the caller's thread**,
    /// which for a window is the event thread, and a settings read that could
    /// stall it would be a settings read that can freeze the app.
    pub fn global_when_ready(&self) -> Pending<GlobalSettings> {
        self.pending(
            GlobalSettings::default(),
            Section::Global,
            |body| match body {
                SectionBody::Global(settings) => Some(settings),
                _ => None,
            },
        )
    }

    pub fn terminal_when_ready(&self) -> Pending<TerminalSettings> {
        self.pending(
            TerminalSettings::default(),
            Section::Terminal,
            |body| match body {
                SectionBody::Terminal(settings) => Some(settings),
                _ => None,
            },
        )
    }

    pub fn ai_when_ready(&self) -> Pending<AiSettings> {
        self.pending(AiSettings::default(), Section::Ai, |body| match body {
            SectionBody::Ai(settings) => Some(settings),
            _ => None,
        })
    }

    // ------------------------------------------------------------ watching

    /// Invalidate-and-refetch, the sibling of `watch_catalog`.
    ///
    /// The handler is told which section changed and nothing more, because
    /// that is all a change fact carries. A window marks the section stale and
    /// refetches it on its own thread — never inside `wake`, which runs on the
    /// subscription thread.
    pub fn watch(
        &self,
        mut wake: impl FnMut(Section) + Send + 'static,
    ) -> Result<SettingsWatch, koru_store_core::errors::Error> {
        let flags = Arc::new(SectionFlags::default());
        let theirs = Arc::clone(&flags);
        let subscription =
            self.client
                .events()
                .on(None, crate::events::Filter::all(), move |stored| {
                    if let Some(section) = section_changed(stored.envelope.name.as_str()) {
                        theirs.invalidate(section);
                        wake(section);
                    }
                })?;
        Ok(SettingsWatch {
            flags,
            subscription,
        })
    }

    // ----------------------------------------------------------- internals

    fn read(&self, section: Section) -> SettingsResult<SectionBody> {
        self.client
            .store()
            .get_public_json(&path(section))
            .map_err(map_public)
    }

    fn patch<B: serde::Serialize>(
        &self,
        section: Section,
        body: &B,
    ) -> SettingsResult<SectionBody> {
        // A field-scoped patch names what it changes and nothing else, so
        // replaying one after a dropped connection produces the same section:
        // it is safe to retry, unlike a whole-body write.
        self.client
            .store()
            .patch_public_json(&path(section), body, RetryPolicy::Safe)
            .map_err(map_public)
    }

    fn pending<T: Send + 'static>(
        &self,
        defaults: T,
        section: Section,
        extract: fn(SectionBody) -> Option<T>,
    ) -> Pending<T> {
        let (sender, receiver) = std::sync::mpsc::channel();
        // A cheap clone of the discovery-and-credential machinery. The worker
        // must not borrow the caller's client: the caller is about to go on
        // rendering with it.
        let store = self.client.store().clone();
        let path = path(section);
        let budget = std::time::Duration::from_millis(koru_settings::STARTUP_WAIT_MILLIS);
        let _ = std::thread::Builder::new()
            .name("koru-settings-wait".into())
            .spawn(move || {
                let deadline = std::time::Instant::now() + budget;
                loop {
                    match store.get_public_json::<SectionBody>(&path) {
                        Ok(body) => {
                            let _ = sender.send(extract(body).ok_or(Timeout));
                            return;
                        }
                        Err(_) if std::time::Instant::now() >= deadline => {
                            let _ = sender.send(Err(Timeout));
                            return;
                        }
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
                    }
                }
            });
        Pending {
            defaults,
            receiver,
            resolved: false,
        }
    }
}

/// The budget ran out, or the section that came back was not the one asked
/// for. Either way the caller keeps its defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeout;

/// A section that will arrive, or will not, within the startup budget.
///
/// **Defaults now, correct later.** Holding one of these is holding a usable
/// section: `defaults()` answers immediately and keeps answering until
/// `take()` produces the authoritative values.
pub struct Pending<T> {
    defaults: T,
    receiver: std::sync::mpsc::Receiver<Result<T, Timeout>>,
    resolved: bool,
}

impl<T> Pending<T> {
    /// The registry's defaults, available before anything has been asked.
    pub fn defaults(&self) -> &T {
        &self.defaults
    }

    /// Whether the wait has already produced its answer, either way.
    pub fn is_resolved(&self) -> bool {
        self.resolved
    }

    /// The authoritative section, if it has arrived.
    ///
    /// **Never blocks.** `None` means "not yet, go on drawing"; `Some(Err)`
    /// means the budget ran out and the defaults stand — which is S9, and a
    /// caller that wants to say so out loud can. Returns an answer once; after
    /// that it is always `None`.
    pub fn take(&mut self) -> Option<Result<T, Timeout>> {
        if self.resolved {
            return None;
        }
        match self.receiver.try_recv() {
            Ok(answer) => {
                self.resolved = true;
                Some(answer)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            // The worker thread went away without answering, which is the same
            // outcome as running out of time.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.resolved = true;
                Some(Err(Timeout))
            }
        }
    }
}

/// Which section a registry row is about, from its name alone.
///
/// Prefix-based like `catalog_change`, so a later settings row invalidates
/// without a second table to keep true.
pub fn section_changed(event_name: &str) -> Option<Section> {
    match event_name {
        "settings.global_changed" => Some(Section::Global),
        "settings.ai_changed" => Some(Section::Ai),
        "settings.terminal_changed" => Some(Section::Terminal),
        _ => None,
    }
}

/// One sticky "this section moved" flag per section.
#[derive(Debug, Default)]
pub struct SectionFlags {
    global: AtomicBool,
    ai: AtomicBool,
    terminal: AtomicBool,
}

impl SectionFlags {
    fn cell(&self, section: Section) -> &AtomicBool {
        match section {
            Section::Global => &self.global,
            Section::Ai => &self.ai,
            Section::Terminal => &self.terminal,
        }
    }

    pub fn invalidate(&self, section: Section) {
        self.cell(section).store(true, Ordering::Relaxed);
    }

    pub fn is_stale(&self, section: Section) -> bool {
        self.cell(section).load(Ordering::Relaxed)
    }

    /// Clear the flag and say whether it was set — the fetch-on-open call.
    pub fn take(&self, section: Section) -> bool {
        self.cell(section).swap(false, Ordering::Relaxed)
    }
}

/// A live settings subscription.
pub struct SettingsWatch {
    flags: Arc<SectionFlags>,
    subscription: Subscription,
}

impl SettingsWatch {
    pub fn flags(&self) -> Arc<SectionFlags> {
        Arc::clone(&self.flags)
    }

    pub fn take(&self, section: Section) -> bool {
        self.flags.take(section)
    }

    pub fn close(self) {
        self.subscription.close();
    }
}

/// The wire shape of a section, which names itself.
///
/// The route answers one path with three shapes, so the body carries the
/// section tag — a client that asked for `terminal` and was handed `global`
/// says so rather than deserializing whatever fits.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "section", rename_all = "snake_case")]
pub enum SectionBody {
    Global(GlobalSettings),
    Ai(AiSettings),
    Terminal(TerminalSettings),
}

fn path(section: Section) -> String {
    format!("/v1/settings/{}", section.token())
}

fn mismatched(wanted: Section, got: SectionBody) -> SettingsError {
    let got = match got {
        SectionBody::Global(_) => Section::Global,
        SectionBody::Ai(_) => Section::Ai,
        SectionBody::Terminal(_) => Section::Terminal,
    };
    SettingsError::Local(format!(
        "asked the settings authority for `{}` and it answered `{}`",
        wanted.token(),
        got.token(),
    ))
}
