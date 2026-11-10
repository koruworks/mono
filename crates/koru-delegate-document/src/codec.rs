// SPDX-License-Identifier: MPL-2.0

//! The codec between the run vocabulary and the values a `LoroDoc` holds.
//!
//! One rule: **a stored value is the vocabulary's own serde shape.** A
//! `RunHeader` field, a `TimelineEntry` and a `UsageFact` cross into the
//! document as the `serde_json::Value` their `Serialize` produces and come
//! back through their `Deserialize`, so tolerance of what an older or newer
//! build wrote is `koru-delegate-core`'s promise (no `deny_unknown_fields`,
//! defaults on the fields a later build added) and not a second one made
//! here. `loro` converts a JSON value to a `LoroValue` and back; nothing is
//! ever written as JSON *text*.
//!
//! The two things this module adds are the key table — which header field
//! lives under which `run` key — and the entry envelope that tells a step
//! from a usage fact in the one `timeline` list.

use koru_delegate_core::{RunHeader, TimelineEntry, UsageFact};
use loro::LoroValue;
use serde::{Deserialize, Serialize};

/// Root containers. Stored names.
pub(crate) const ROOT_RUN: &str = "run";
pub(crate) const ROOT_TIMELINE: &str = "timeline";

/// Keys of the `run` map this crate writes itself, beside the header's
/// fields. Stored names.
pub(crate) const K_SCHEMA: &str = "schema";
pub(crate) const K_MODELS: &str = "models";
pub(crate) const K_STATUS: &str = "status";
pub(crate) const K_RESULTS: &str = "results";
pub(crate) const K_ECONOMICS: &str = "economics";
pub(crate) const K_RATING: &str = "rating";

/// Header fields whose stored key differs from the struct's field name — the
/// proposal's spelling (§1.3) over the struct's. Everything not listed is
/// stored under its own serde name.
const RENAMED: [(&str, &str); 2] = [("run", "id"), ("artifacts", K_RESULTS)];

fn stored_key(field: &str) -> &str {
    RENAMED
        .iter()
        .find(|(from, _)| *from == field)
        .map_or(field, |(_, to)| to)
}

fn field_name(stored: &str) -> &str {
    RENAMED
        .iter()
        .find(|(_, to)| *to == stored)
        .map_or(stored, |(from, _)| from)
}

/// The keys of the `run` map a header writes at creation, in the order they
/// are written, and their values.
pub(crate) fn header_to_keys(
    header: &RunHeader,
) -> Result<Vec<(String, LoroValue)>, RunDocumentError> {
    if !header.bot_fields_valid() {
        return Err(RunDocumentError::Malformed { part: Part::Header });
    }
    let serde_json::Value::Object(fields) = serde_json::to_value(header)
        .map_err(|_| RunDocumentError::Malformed { part: Part::Header })?
    else {
        return Err(RunDocumentError::Malformed { part: Part::Header });
    };
    Ok(fields
        .into_iter()
        .map(|(field, value)| (stored_key(&field).to_owned(), LoroValue::from(value)))
        .collect())
}

/// A header from the `run` map's deep value. Keys this build does not know
/// stay in the map untouched and are ignored here; `schema` and `models` are
/// this crate's, not the header's.
pub(crate) fn header_from_map(map: &LoroValue) -> Result<RunHeader, RunDocumentError> {
    let LoroValue::Map(entries) = map else {
        return Err(RunDocumentError::Malformed { part: Part::Header });
    };
    let object: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        .filter(|(key, _)| key.as_str() != K_SCHEMA && key.as_str() != K_MODELS)
        .map(|(key, value)| {
            (
                field_name(key).to_owned(),
                serde_json::Value::from(value.clone()),
            )
        })
        .collect();
    let header: RunHeader = serde_json::from_value(serde_json::Value::Object(object))
        .map_err(|_| RunDocumentError::Malformed { part: Part::Header })?;
    if !header.bot_fields_valid() {
        return Err(RunDocumentError::Malformed { part: Part::Header });
    }
    Ok(header)
}

/// Any serializable value, as the document stores it.
pub(crate) fn to_stored<T: Serialize>(
    value: &T,
    part: Part,
) -> Result<LoroValue, RunDocumentError> {
    serde_json::to_value(value)
        .map(LoroValue::from)
        .map_err(|_| RunDocumentError::Malformed { part })
}

/// Any stored value, back as its type.
pub(crate) fn from_stored<T: for<'de> Deserialize<'de>>(
    value: LoroValue,
    part: Part,
) -> Result<T, RunDocumentError> {
    serde_json::from_value(serde_json::Value::from(value))
        .map_err(|_| RunDocumentError::Malformed { part })
}

/// One value of the `timeline` list as it is stored: a step or a usage fact,
/// told apart by the `entry` tag beside the value's own fields.
///
/// Both are appended once and never edited (D45), which is why each is a
/// `LoroValue::Map` — one op — and not a container.
#[derive(Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub(crate) enum StoredItem {
    Step(TimelineEntry),
    Usage(UsageFact),
}

/// One item of the timeline as a reader sees it.
///
/// `Unrecognized` is an entry this build cannot decode — a step kind or an
/// envelope a newer build wrote. It stays in the document byte for byte
/// (nothing here rewrites what it read) and takes its place in the window,
/// so a surface can say "a step this build does not understand" at the
/// right position rather than silently showing fewer steps than there are.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TimelineItem {
    Step(TimelineEntry),
    Usage(UsageFact),
    Unrecognized,
}

impl TimelineItem {
    pub fn step(&self) -> Option<&TimelineEntry> {
        match self {
            Self::Step(entry) => Some(entry),
            Self::Usage(_) | Self::Unrecognized => None,
        }
    }

    pub fn usage(&self) -> Option<&UsageFact> {
        match self {
            Self::Usage(fact) => Some(fact),
            Self::Step(_) | Self::Unrecognized => None,
        }
    }

    pub(crate) fn decode(value: LoroValue) -> Self {
        match from_stored::<StoredItem>(value, Part::Timeline) {
            Ok(StoredItem::Step(entry)) => Self::Step(entry),
            Ok(StoredItem::Usage(fact)) => Self::Usage(fact),
            Err(_) => Self::Unrecognized,
        }
    }
}

/// Which part of the document a malformed value was found in. A closed
/// vocabulary, so an error names a place and never quotes a value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Header,
    Timeline,
    Models,
    Results,
    Economics,
}

/// Why the document could not be read or written.
///
/// No variant carries text from the document: the objective, a rating note
/// and an artifact title are content, and an error is one of the places
/// content must never reach (ROADMAP §13.7).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RunDocumentError {
    /// The document has no `run.schema`: a page, a manifest, an empty
    /// document — anything but a run.
    NotARun,
    /// `run.schema` is above [`crate::RUN_DOCUMENT_SCHEMA_VERSION`]: a run
    /// written by a newer build. It opens for reading; every write refuses
    /// with this.
    ReadOnly { found: i64, understood: i64 },
    /// `initialize` was handed a document that already carries a run.
    AlreadyInitialized,
    /// A stored value did not decode as the vocabulary type this build
    /// expects there.
    Malformed { part: Part },
    /// The CRDT refused an operation, an export or an import. Loro's own
    /// message, which describes structure and never quotes a value.
    Crdt(String),
}

impl std::fmt::Display for RunDocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARun => f.write_str("the document is not a run: it has no run.schema"),
            Self::ReadOnly { found, understood } => write!(
                f,
                "the run was written by a newer build (schema {found}, this build understands {understood}) and is read-only here"
            ),
            Self::AlreadyInitialized => f.write_str("the document already holds a run"),
            Self::Malformed { part } => {
                write!(f, "a stored value in the run's {part:?} did not decode")
            }
            Self::Crdt(message) => write!(f, "crdt: {message}"),
        }
    }
}

impl std::error::Error for RunDocumentError {}

impl From<loro::LoroError> for RunDocumentError {
    fn from(error: loro::LoroError) -> Self {
        Self::Crdt(error.to_string())
    }
}

impl From<loro::LoroEncodeError> for RunDocumentError {
    fn from(error: loro::LoroEncodeError) -> Self {
        Self::Crdt(error.to_string())
    }
}
