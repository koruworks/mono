// SPDX-License-Identifier: MPL-2.0

//! **The locator a case names a control by, and the pattern it may name it with.**
//!
//! Koru publishes an identity on every node it projects —
//! `koru:w<window>/v<view>/<object>#<part>` on `UIA_AutomationIdPropertyId`. The
//! grammar is `crates/koru-workspace-host/src/accessibility/identity.rs`'s and
//! this module is the *reader's* half of it: nothing here mints a locator, and
//! nothing here may drift from what the projection writes.
//!
//! ```text
//! koru:w1/v0/s:window#close                      a caption command
//! koru:w1/v1/s:body#root                         the document body
//! koru:w1/v1/n:6f9619ff8b86d011b42d00cf4fc964ff#text   a paragraph
//! koru:w1/v1/tab:7#root                          one tab instance
//! koru:w1/v1/onb:3#status                        one first-run screen instance
//! ```
//!
//! ## Why a pattern, and not just a string
//!
//! Window and view numbers are **slots handed out at runtime and never reused**.
//! A case cannot know them, and a case that hard-codes `w1/v1` — which every
//! interactive proof in the tree does today, with a comment saying "the first
//! slot the allocator hands out" — is a case that passes on a fresh launch and
//! fails on the second window. So `*` is allowed where a slot goes:
//!
//! ```text
//! koru:w*/v*/s:window#close
//! ```
//!
//! and the runner binds `w*` to the window it was actually pointed at before it
//! searches. What `*` is emphatically **not** allowed to do is stand for a
//! part or an object keyword: `koru:w1/v1/s:*#root` would be a case that does
//! not know what it is looking for, and the honest failure for that is at parse
//! time rather than as an ambiguity three minutes later.
//!
//! ## An exact pattern is a different search from a wildcarded one
//!
//! [`LocatorPattern::exact`] is `Some` when every component is fixed, and that
//! is the fast path: one `CreatePropertyCondition` on the id and one
//! `FindFirst`. A wildcard cannot be expressed as a property condition — UIA
//! has no prefix match — so it costs a `FindAll` over the subtree and a filter
//! here. That is `O(the tree)`, and over a thousand-paragraph document it is a
//! real cost, which is why binding `w*` from the root **before** searching is
//! part of the design rather than a nicety.

use std::fmt;

/// The prefix every Koru locator starts with.
///
/// Not imported from `koru-workspace-host`: this crate deliberately does not
/// depend on the shell it inspects, for the same reason Narrator does not — a
/// client that linked the provider would be asserting against itself.
///
/// **Nothing compiles the two together, so nothing catches a divergence.** The
/// other copy is `koru_workspace_host::accessibility::identity::LOCATOR_PREFIX`
/// and the grammar beside it; the interactive proof is where a change to either
/// shows up, because it is the only place a real projection and a real reader
/// meet. That is the cost of not depending on the shell, and it is recorded
/// here rather than discovered later.
pub const PREFIX: &str = "koru:";

/// A window or view slot: a number the projection handed out, or "whichever".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Any,
    Exact(u64),
}

impl Slot {
    fn parse(text: &str) -> Option<Self> {
        if text == "*" {
            Some(Self::Any)
        } else {
            text.parse().ok().map(Self::Exact)
        }
    }

    fn matches(self, value: u64) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(wanted) => wanted == value,
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("*"),
            Self::Exact(n) => write!(f, "{n}"),
        }
    }
}

/// What the occurrence is an occurrence *of*.
///
/// The three arms are `ObjectKey`'s: a surface keyword, a model uuid, or one
/// live instance of something a view opens and closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Object {
    /// `s:<keyword>`
    Surface(String),
    /// `n:<32 lowercase hex digits>` — a `Uuid` with its hyphens stripped,
    /// which is what `Uuid::simple` writes.
    Model(String),
    /// `<kind>:<id>` — `tab:7`, `onb:3`. The id may be a wildcard, because an
    /// instance number is handed out at runtime exactly like a slot is.
    Instance { kind: String, id: Slot },
}

impl fmt::Display for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Surface(name) => write!(f, "s:{name}"),
            Self::Model(id) => write!(f, "n:{id}"),
            Self::Instance { kind, id } => write!(f, "{kind}:{id}"),
        }
    }
}

/// A locator, possibly with wildcards where slots go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatorPattern {
    pub window: Slot,
    pub view: Slot,
    pub object: Object,
    pub part: String,
}

/// Why a string is not a locator.
///
/// Every arm names the component, because "invalid locator" in a runner's
/// output is a message that sends somebody back to the grammar rather than to
/// their own typo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocatorError {
    MissingPrefix,
    MissingPart,
    EmptyPart,
    /// Not `w<n>` or `w*`.
    Window(String),
    /// Not `v<n>` or `v*`.
    View(String),
    Object(String),
    /// `s:` and `n:` name one thing each; `*` there would be a case that does
    /// not know what it is looking for.
    WildcardObject,
    /// `n:` takes a uuid with its hyphens stripped: 32 lowercase hex digits.
    ModelId(String),
    TrailingSegments,
}

impl fmt::Display for LocatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPrefix => write!(f, "a locator starts with `{PREFIX}`"),
            Self::MissingPart => f.write_str("a locator ends with `#<part>`"),
            Self::EmptyPart => f.write_str("the part after `#` is empty"),
            Self::Window(got) => write!(f, "`{got}` is not a window slot: expected `w<n>` or `w*`"),
            Self::View(got) => write!(f, "`{got}` is not a view slot: expected `v<n>` or `v*`"),
            Self::Object(got) => write!(
                f,
                "`{got}` is not an object: expected `s:<keyword>`, `n:<uuid>` or `<kind>:<id>`"
            ),
            Self::WildcardObject => f.write_str(
                "`*` may stand for a window, a view or an instance number, and never for what \
                 the control is",
            ),
            Self::ModelId(got) => write!(
                f,
                "`{got}` is not a model id: expected 32 lowercase hex digits, which is a uuid \
                 with its hyphens removed"
            ),
            Self::TrailingSegments => {
                f.write_str("a locator has exactly three `/`-separated segments after the prefix")
            }
        }
    }
}

impl LocatorPattern {
    /// Read a locator or a pattern.
    pub fn parse(text: &str) -> Result<Self, LocatorError> {
        let body = text
            .strip_prefix(PREFIX)
            .ok_or(LocatorError::MissingPrefix)?;
        let (path, part) = body.rsplit_once('#').ok_or(LocatorError::MissingPart)?;
        if part.is_empty() {
            return Err(LocatorError::EmptyPart);
        }

        let mut segments = path.split('/');
        let window = segments.next().unwrap_or_default();
        let view = segments.next().ok_or(LocatorError::TrailingSegments)?;
        let object = segments.next().ok_or(LocatorError::TrailingSegments)?;
        if segments.next().is_some() {
            return Err(LocatorError::TrailingSegments);
        }

        let window = window
            .strip_prefix('w')
            .and_then(Slot::parse)
            .ok_or_else(|| LocatorError::Window(window.to_owned()))?;
        let view = view
            .strip_prefix('v')
            .and_then(Slot::parse)
            .ok_or_else(|| LocatorError::View(view.to_owned()))?;

        Ok(Self {
            window,
            view,
            object: parse_object(object)?,
            part: part.to_owned(),
        })
    }

    /// Bind a wildcarded window to the window the runner is actually pointed at.
    ///
    /// This is what keeps the common pattern — `koru:w*/v1/...` — off the
    /// `FindAll` path: the root element knows its own window number, so the
    /// runner reads it once and the search that follows is an exact property
    /// condition.
    pub fn bound_to_window(&self, window: u64) -> Self {
        Self {
            window: match self.window {
                Slot::Any => Slot::Exact(window),
                exact => exact,
            },
            ..self.clone()
        }
    }

    /// The one locator this pattern can only mean, when it has no wildcard left.
    pub fn exact(&self) -> Option<String> {
        let (Slot::Exact(window), Slot::Exact(view)) = (self.window, self.view) else {
            return None;
        };
        if let Object::Instance { id: Slot::Any, .. } = self.object {
            return None;
        }
        Some(format!(
            "{PREFIX}w{window}/v{view}/{}#{}",
            self.object, self.part
        ))
    }

    /// Whether a locator a client read off an element is one this names.
    ///
    /// A string that is not a locator at all does not match — including the
    /// empty one, which is what `CurrentAutomationId` gives for a node with no
    /// `author_id` and for `accesskit_windows`' placeholder root.
    pub fn matches(&self, locator: &str) -> bool {
        let Ok(other) = Self::parse(locator) else {
            return false;
        };
        let object_matches = match (&self.object, &other.object) {
            (Object::Surface(ours), Object::Surface(theirs)) => ours == theirs,
            (Object::Model(ours), Object::Model(theirs)) => ours == theirs,
            (
                Object::Instance { kind: ours, id },
                Object::Instance {
                    kind: theirs,
                    id: found,
                },
            ) => ours == theirs && matches!(found, Slot::Exact(n) if id.matches(*n)),
            _ => false,
        };
        object_matches
            && self.part == other.part
            && matches!(other.window, Slot::Exact(n) if self.window.matches(n))
            && matches!(other.view, Slot::Exact(n) if self.view.matches(n))
    }
}

impl fmt::Display for LocatorPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{PREFIX}w{}/v{}/{}#{}",
            self.window, self.view, self.object, self.part
        )
    }
}

fn parse_object(text: &str) -> Result<Object, LocatorError> {
    if text == "*" {
        return Err(LocatorError::WildcardObject);
    }
    let (kind, rest) = text
        .split_once(':')
        .ok_or_else(|| LocatorError::Object(text.to_owned()))?;
    if kind.is_empty() || rest.is_empty() {
        return Err(LocatorError::Object(text.to_owned()));
    }
    match kind {
        "s" => {
            if rest == "*" {
                Err(LocatorError::WildcardObject)
            } else {
                Ok(Object::Surface(rest.to_owned()))
            }
        }
        "n" => {
            if rest == "*" {
                return Err(LocatorError::WildcardObject);
            }
            if rest.len() == 32
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                Ok(Object::Model(rest.to_owned()))
            } else {
                Err(LocatorError::ModelId(rest.to_owned()))
            }
        }
        kind => Slot::parse(rest)
            .map(|id| Object::Instance {
                kind: kind.to_owned(),
                id,
            })
            .ok_or_else(|| LocatorError::Object(text.to_owned())),
    }
}

/// The window number a root element's own locator carries.
///
/// How an external tool bootstraps: it is handed an `HWND`, reads that window's
/// `AutomationId`, and every locator it then builds is scoped by the number it
/// found there. The interactive proofs in the tree do exactly this, by
/// `trim_start_matches` and `split('/')`; this is the same thing with the
/// failure spelled out, because a runner that guessed `w1` would search the
/// wrong window on the second one a person opens.
pub fn window_slot_of(locator: &str) -> Option<u64> {
    match LocatorPattern::parse(locator).ok()?.window {
        Slot::Exact(window) => Some(window),
        Slot::Any => None,
    }
}

#[cfg(test)]
mod tests;
