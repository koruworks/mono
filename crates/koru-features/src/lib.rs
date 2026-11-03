// SPDX-License-Identifier: MPL-2.0

//! **Which product surfaces this build shows.**
//!
//! Koru's next release shows documents, files, terminal/shell sessions and
//! agent sessions; seven larger surfaces are built, tested and *hidden* until
//! they are switched on, and agents can be switched off:
//!
//! | Flag | Env var | Hides |
//! | --- | --- | --- |
//! | [`ENABLE_CLOUD_SYNC`] | `KORU_ENABLE_CLOUD_SYNC` | Koru Cloud: sign-in, OAuth, the Cloud page, subscribe pointers |
//! | [`ENABLE_AGENTS`] | `KORU_ENABLE_AGENTS` | agent sessions as tabs (shell sessions stay); **on unless set off** |
//! | [`ENABLE_AGENT_VIEWS`] | `KORU_ENABLE_AGENT_VIEWS` | Explore's Agents and Artifacts views (need agents on as well) |
//! | [`ENABLE_TASKS`] | `KORU_ENABLE_TASKS` | the Tasks view and its entry points (`/due` stays) |
//! | [`ENABLE_AI`] | `KORU_ENABLE_AI` | Koru AI chat and Delegate (runs and results) |
//! | [`ENABLE_REMINDERS`] | `KORU_ENABLE_REMINDERS` | the reminders UI (the worker still runs) |
//! | [`ENABLE_CANVAS`] | `KORU_ENABLE_CANVAS` | canvases: the Canvases view and canvas tabs |
//! | [`ENABLE_SOCIALS`] | `KORU_ENABLE_SOCIALS` | Socials, and the Agent's Socials publisher |
//!
//! `guide/contributing/FEATURE-FLAGS.md` is the per-surface list.
//!
//! ## Baked in, with an environment override
//!
//! A flag's default is decided when *this crate* is compiled, from
//! `option_env!` on each `KORU_ENABLE_*` name: unset is **off** — except
//! agents, which are **on** when unset (maintainers, 2026-10-15) — so a release
//! built with nothing set ships documents, files, shells and agents. The release
//! tool forwards each of the eight that is set, as `1` or `0`, to
//! `cargo build` (`koru-release::package::run_cargo`), so a release bakes a
//! surface on, or agents off, by setting it in its environment or in
//! `.env.production`.
//!
//! At run time a variable that is **set** wins over the baked default in both
//! directions — `KORU_ENABLE_AI=1` shows Koru AI in a build that baked it off,
//! `KORU_ENABLE_AI=0` hides it in one that baked it on — and the answer is
//! latched for the life of the process ([`features`]). The truthiness rule is
//! `koru_env::switch_value`'s, replicated here so this crate stays a leaf (a
//! test holds the two together).
//!
//! **Every read is in this crate.** `option_env!` is evaluated per compiled
//! crate; a second crate baking one of these names with `option_env!` could be
//! compiled under a different environment and disagree.
//!
//! ## UI only
//!
//! A flag hides what a person sees and reaches — menus, shortcuts, tabs,
//! session restore, the accessibility tree. Background workers start exactly
//! as they do with the flag on — with one exception: Koru Agent starts the
//! Socials publisher only while [`socials`] is on, because nothing should
//! publish from a build that hides Socials (ADR-450's 2026-10-10 amendment).
//! Nothing here is a security boundary.
//!
//! ## Tests
//!
//! Under the `testing` feature (turned on by dependents' `[dev-dependencies]`)
//! and in this crate's own tests, the default is [`Features::ALL_ON`] and the
//! environment is ignored, so suites written before the flags keep exercising
//! every surface. A test that means the simplified product says so with
//! [`testing::with`], which overrides the answer on the calling thread only.

use std::sync::OnceLock;

/// The eight surfaces, as one value.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Features {
    /// Koru Cloud: account sign-in (email, Google, Apple), the Cloud page,
    /// "Subscribe to Koru Cloud", the Cloud sync column section.
    pub cloud_sync: bool,
    /// Agent sessions as tabs: the `+` menu's *Agents ▸*, agent tabs and
    /// their restore, the tray's Agents submenu. Shell sessions are not an
    /// agent.
    pub agents: bool,
    /// Explore's Agents and Artifacts views (`Alt+5`, `Alt+6`), and every
    /// way into them: revealing a run or a schedule, opening a result from
    /// the Artifacts column. Shown only with [`Self::agents`] on as well —
    /// see [`agent_views`].
    pub agent_views: bool,
    /// The Tasks view and every way into it.
    pub tasks: bool,
    /// Koru AI chat and Delegate.
    pub ai: bool,
    /// The reminders UI.
    pub reminders: bool,
    /// Canvases: the Canvases view, canvas tabs and every way into them.
    pub canvas: bool,
    /// Socials: the Socials view, its tab and every way into it — and the
    /// Agent's publishing worker, which starts only while this is on.
    pub socials: bool,
}

impl Features {
    pub const ALL_ON: Features = Features {
        cloud_sync: true,
        agents: true,
        agent_views: true,
        tasks: true,
        ai: true,
        reminders: true,
        canvas: true,
        socials: true,
    };
    /// The simplified product: documents, files and shell sessions.
    pub const ALL_OFF: Features = Features {
        cloud_sync: false,
        agents: false,
        agent_views: false,
        tasks: false,
        ai: false,
        reminders: false,
        canvas: false,
        socials: false,
    };
    /// What this build baked in, before any run-time override.
    pub const BAKED: Features = Features {
        cloud_sync: ENABLE_CLOUD_SYNC,
        agents: ENABLE_AGENTS,
        agent_views: ENABLE_AGENT_VIEWS,
        tasks: ENABLE_TASKS,
        ai: ENABLE_AI,
        reminders: ENABLE_REMINDERS,
        canvas: ENABLE_CANVAS,
        socials: ENABLE_SOCIALS,
    };

    /// The baked defaults with each variable `lookup` reports as set taking
    /// over. Pure, so the rule is testable without touching the process
    /// environment.
    pub fn resolve(baked: Features, lookup: impl Fn(&str) -> Option<String>) -> Features {
        let flag = |name: &str, baked: bool| match lookup(name) {
            Some(value) => switch_value(Some(&value)),
            None => baked,
        };
        Features {
            cloud_sync: flag(env::KORU_ENABLE_CLOUD_SYNC, baked.cloud_sync),
            agents: flag(env::KORU_ENABLE_AGENTS, baked.agents),
            agent_views: flag(env::KORU_ENABLE_AGENT_VIEWS, baked.agent_views),
            tasks: flag(env::KORU_ENABLE_TASKS, baked.tasks),
            ai: flag(env::KORU_ENABLE_AI, baked.ai),
            reminders: flag(env::KORU_ENABLE_REMINDERS, baked.reminders),
            canvas: flag(env::KORU_ENABLE_CANVAS, baked.canvas),
            socials: flag(env::KORU_ENABLE_SOCIALS, baked.socials),
        }
    }
}

/// One line for a diagnostic: `cloud_sync=off agents=on ...`. Names only, so
/// it is safe in any log.
impl std::fmt::Display for Features {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let word = |on: bool| if on { "on" } else { "off" };
        write!(
            f,
            "cloud_sync={} agents={} agent_views={} tasks={} ai={} reminders={} canvas={} socials={}",
            word(self.cloud_sync),
            word(self.agents),
            word(self.agent_views),
            word(self.tasks),
            word(self.ai),
            word(self.reminders),
            word(self.canvas),
            word(self.socials)
        )
    }
}

/// The variable names. The registry row for each is in `koru-env`'s `VARS`.
pub mod env {
    pub const KORU_ENABLE_CLOUD_SYNC: &str = "KORU_ENABLE_CLOUD_SYNC";
    pub const KORU_ENABLE_AGENTS: &str = "KORU_ENABLE_AGENTS";
    pub const KORU_ENABLE_AGENT_VIEWS: &str = "KORU_ENABLE_AGENT_VIEWS";
    pub const KORU_ENABLE_TASKS: &str = "KORU_ENABLE_TASKS";
    pub const KORU_ENABLE_AI: &str = "KORU_ENABLE_AI";
    pub const KORU_ENABLE_REMINDERS: &str = "KORU_ENABLE_REMINDERS";
    pub const KORU_ENABLE_CANVAS: &str = "KORU_ENABLE_CANVAS";
    pub const KORU_ENABLE_SOCIALS: &str = "KORU_ENABLE_SOCIALS";
    /// All eight, in [`super::Features`] field order — what the release tool
    /// forwards to `cargo build`.
    pub const ALL: [&str; 8] = [
        KORU_ENABLE_CLOUD_SYNC,
        KORU_ENABLE_AGENTS,
        KORU_ENABLE_AGENT_VIEWS,
        KORU_ENABLE_TASKS,
        KORU_ENABLE_AI,
        KORU_ENABLE_REMINDERS,
        KORU_ENABLE_CANVAS,
        KORU_ENABLE_SOCIALS,
    ];
}

/// Koru Cloud / OAuth sign-in, as baked into this build.
pub const ENABLE_CLOUD_SYNC: bool = switch_value(option_env!("KORU_ENABLE_CLOUD_SYNC"));
/// Agent sessions and artifacts, as baked into this build. **On when unset**
/// — the one flag whose surface ships by default (ADR-450's 2026-10-15
/// amendment); `KORU_ENABLE_AGENTS=0` bakes it off.
pub const ENABLE_AGENTS: bool = switch_value_or(option_env!("KORU_ENABLE_AGENTS"), true);
/// Explore's Agents and Artifacts views, as baked into this build. Off when
/// unset: agent *tabs* ship, the views over their runs and results do not
/// (maintainers, 2026-10-16).
pub const ENABLE_AGENT_VIEWS: bool = switch_value(option_env!("KORU_ENABLE_AGENT_VIEWS"));
/// The Tasks view and its entry points, as baked into this build. **On when
/// unset** (the CTO's note, 2026-10-26, QUESTIONS.md E52): the view ships;
/// `KORU_ENABLE_TASKS=0` bakes it off.
pub const ENABLE_TASKS: bool = switch_value_or(option_env!("KORU_ENABLE_TASKS"), true);
/// Koru AI chat and Delegate, as baked into this build. **On when unset**
/// (the CTO's note, 2026-10-26, QUESTIONS.md E54: enable Chats): the Chats
/// view, the chat tab and Delegate ship; `KORU_ENABLE_AI=0` bakes them off.
pub const ENABLE_AI: bool = switch_value_or(option_env!("KORU_ENABLE_AI"), true);
/// The reminders UI, as baked into this build.
pub const ENABLE_REMINDERS: bool = switch_value(option_env!("KORU_ENABLE_REMINDERS"));
/// Canvases, as baked into this build.
pub const ENABLE_CANVAS: bool = switch_value(option_env!("KORU_ENABLE_CANVAS"));
/// Socials authoring, as baked into this build.
pub const ENABLE_SOCIALS: bool = switch_value(option_env!("KORU_ENABLE_SOCIALS"));

/// **What this process shows.** Latched on first call: a surface does not
/// appear or vanish mid-session.
pub fn features() -> &'static Features {
    #[cfg(any(test, feature = "testing"))]
    if let Some(features) = testing::current() {
        return features;
    }
    static LATCHED: OnceLock<Features> = OnceLock::new();
    LATCHED.get_or_init(process_features)
}

#[cfg(not(any(test, feature = "testing")))]
fn process_features() -> Features {
    Features::resolve(Features::BAKED, |name| std::env::var(name).ok())
}

/// A test build shows everything and ignores the shell — see the crate docs.
#[cfg(any(test, feature = "testing"))]
fn process_features() -> Features {
    Features::ALL_ON
}

/// Koru Cloud / OAuth sign-in is shown.
pub fn cloud_sync() -> bool {
    features().cloud_sync
}
/// Agent sessions are shown as tabs.
pub fn agents() -> bool {
    features().agents
}
/// Explore's Agents and Artifacts views are shown. They are views *of* agent
/// sessions, so they need agents on as well: setting this alone shows nothing.
pub fn agent_views() -> bool {
    let features = features();
    features.agents && features.agent_views
}
/// The Tasks view and its entry points are shown.
pub fn tasks() -> bool {
    features().tasks
}
/// Koru AI chat and Delegate are shown.
pub fn ai() -> bool {
    features().ai
}
/// The reminders UI is shown.
pub fn reminders() -> bool {
    features().reminders
}
/// Canvases are shown.
pub fn canvas() -> bool {
    features().canvas
}
/// Socials authoring is shown.
pub fn socials() -> bool {
    features().socials
}

// ------------------------------------------------------- hidden for now
//
// **Not flags: switches in the source.** A flag above is a surface a release
// may bake on and a person may turn on with a variable. What follows is a
// part of a shown surface that is hidden *for now* — nothing about it is
// configurable, it is one constant to flip back, and it is here, beside the
// flags, so "what does this build hide" has one place to look. The code
// behind each stays built and tested where it can be; turning the constant
// to `true` puts every way into it back.

/// **Home** — the house at the top of the left rail, its page (Inbox, Today,
/// Upcoming, Recent) and its `Alt+H`. Hidden (the user, 2026-11-03): "we
/// don't have anything functional for now". With it off the rail starts at
/// Documents, `Alt+H` does nothing and is taught nowhere, a window that last
/// showed Home reopens on Documents, and nothing opens the Home tab. The
/// other views keep their `Alt+digit`.
pub const HOME_SHOWN: bool = false;

/// **The composer's private-chat button** — the detective mask at the left
/// of the Koru AI composer's footer, which makes the chat being written
/// private, or public again. Hidden (the user, 2026-11-03): no chat is made
/// private from the composer. It has no shortcut. A chat that is already
/// private stays private, works as before and still wears its mark in the
/// Chats list; the footer's other controls start where the mask did.
pub const PRIVATE_CHAT_BUTTON_SHOWN: bool = false;

/// Whether Home is shown ([`HOME_SHOWN`]).
pub fn home() -> bool {
    HOME_SHOWN
}

/// Whether the composer offers its private-chat button
/// ([`PRIVATE_CHAT_BUTTON_SHOWN`]).
pub fn private_chat_button() -> bool {
    PRIVATE_CHAT_BUTTON_SHOWN
}

/// `koru_env::switch_value`, as a `const fn` so a baked value is a constant:
/// on when set to anything except `""`, `0`, `false`, `no`, `off` (trimmed,
/// case-insensitive); unset is off.
pub const fn switch_value(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let bytes = value.as_bytes();
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let len = end - start;
    !(len == 0
        || eq_ignore_case(bytes, start, len, b"0")
        || eq_ignore_case(bytes, start, len, b"false")
        || eq_ignore_case(bytes, start, len, b"no")
        || eq_ignore_case(bytes, start, len, b"off"))
}

/// [`switch_value`] with the answer for an unset variable chosen by the
/// caller: a set value follows the truthiness rule, unset is `unset`.
pub const fn switch_value_or(value: Option<&str>, unset: bool) -> bool {
    match value {
        Some(_) => switch_value(value),
        None => unset,
    }
}

const fn eq_ignore_case(bytes: &[u8], start: usize, len: usize, word: &[u8]) -> bool {
    if len != word.len() {
        return false;
    }
    let mut i = 0;
    while i < len {
        if bytes[start + i].to_ascii_lowercase() != word[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Test-only overrides, on the calling thread.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use super::Features;
    use std::cell::Cell;

    thread_local! {
        static OVERRIDE: Cell<Option<&'static Features>> = const { Cell::new(None) };
    }

    pub(crate) fn current() -> Option<&'static Features> {
        OVERRIDE.with(Cell::get)
    }

    /// Restores the previous override when dropped.
    #[must_use = "the override ends when the guard is dropped"]
    pub struct Guard {
        previous: Option<&'static Features>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            OVERRIDE.with(|cell| cell.set(self.previous));
        }
    }

    /// Answer `features` on this thread until the guard drops. Leaks one
    /// small value per call, which a test can afford.
    pub fn set(features: Features) -> Guard {
        let leaked: &'static Features = Box::leak(Box::new(features));
        let previous = OVERRIDE.with(|cell| cell.replace(Some(leaked)));
        Guard { previous }
    }

    /// Run `body` with `features` answered on this thread.
    pub fn with<R>(features: Features, body: impl FnOnce() -> R) -> R {
        let _guard = set(features);
        body()
    }
}

#[cfg(test)]
mod tests;
