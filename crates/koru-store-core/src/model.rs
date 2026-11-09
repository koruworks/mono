// SPDX-License-Identifier: MPL-2.0

//! What the store holds: profiles, workspaces, documents, blobs and lineage.
//!
//! Plain data with no persistence in it. `koru-store-sqlite` reads and writes
//! these; `koru-store-api` serialises them; neither shape leaks into the other.
//!
//! Three conventions run through every type here, and each is an invariant
//! rather than a style:
//!
//! - **Time is a number.** [`Timestamp`] is epoch milliseconds UTC. Never a
//!   formatted string — `"2026-08-09"` is a rendering of an instant in some
//!   locale and calendar, and the moment it is stored, the store has picked
//!   both on the reader's behalf. Clients format.
//! - **Enums are tokens.** [`DocumentKind`], [`LineageKind`] and [`Scope`]
//!   store stable ASCII, never a label a person reads. A translated value in a
//!   column is a column that means something different in another language.
//! - **Nothing is keyed by a name.** Every identity is a v4 UUID or a content
//!   hash. Names are translatable; identities are not.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::errors::{Code, Error, Result};
use crate::ids::{ClientId, DocumentId, ProfileId, WorkspaceId};
use crate::text::Locale;

// ------------------------------------------------------------------ time

/// Epoch milliseconds, UTC.
///
/// Signed because that is what SQLite stores and what every other language's
/// epoch type is; an unsigned column would be a conversion at every boundary
/// for a range nobody needs.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// **The published description is not the doc comment.** A doc comment explains
// the implementation to whoever maintains this; a schema description explains
// the contract to whoever calls it. The paragraph above names SQLite, which the
// API document is tested never to mention — a client does not need to learn
// what is underneath, and telling them makes it a compatibility surface.
#[cfg_attr(
    feature = "schema",
    schemars(
        description = "Epoch milliseconds, UTC. Signed, so instants before 1970 are \
                            representable."
    )
)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }

    pub const fn millis(self) -> i64 {
        self.0
    }

    /// The wall clock, once. Callers that need determinism take a `Timestamp`
    /// as an argument instead — which is why the model's constructors do.
    pub fn now() -> Self {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            // Before 1970 only if the clock is wrong, and a wrong clock must not
            // be a panic in a persistence layer.
            .unwrap_or(0);
        Self(ms)
    }

    /// The form used in a filename — backups, quarantine directories.
    ///
    /// Digits, so it sorts lexically as it sorts chronologically, and so it
    /// carries no calendar. `koru_docstore::backup::stamp` writes
    /// `YYYYMMDD-HHMMSS`; the store deliberately does not copy that.
    pub fn for_filename(self) -> String {
        self.0.to_string()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ------------------------------------------------------------------ profile

/// Compatibility classification for catalog rows.
///
/// `Holder` was written by an unreleased dev schema-9 lineage. The merged
/// catalog preserves and decodes it so those databases migrate safely, but no
/// production flow creates a holder. Any legacy holder row is excluded from
/// persona surfaces.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    /// A persona a person made. Everything a profile can do, it can do.
    #[default]
    Normal,
    /// A legacy non-persona row retained only for schema-lineage compatibility.
    /// It must never become active or appear in a persona selector.
    Holder,
}

impl ProfileKind {
    /// The token that is stored and crosses the wire. Stable forever.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Holder => "holder",
        }
    }

    /// **An unrecognised token reads as [`Normal`](Self::Normal), never as an
    /// error.** A row written by a future build that grew a third kind must
    /// still list, for the same reason an unparseable `locale` falls back to
    /// `und`: a catalog that refuses to read itself is worse than one that reads
    /// a stranger conservatively. Conservatively here means "a persona", which
    /// is the kind that is safe to show and safe to leave alone.
    pub fn parse(value: &str) -> Self {
        match value {
            "holder" => Self::Holder,
            _ => Self::Normal,
        }
    }

    pub const ALL: &'static [Self] = &[Self::Normal, Self::Holder];
}

impl fmt::Display for ProfileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A persona, not a folder: "Steven — Work", "Steven — Personal".
///
/// **Creating one requires no login.** [`Profile::account`] remains `None`
/// until an explicit compare-and-set binds a hosted Koru Identity. A `ProfileId`
/// remains distinct from account identity and content identity never changes.
///
/// A legacy `Holder` row can still be decoded for migration compatibility,
/// but current code creates personas only.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    /// Free text the person chose — "work", "school". Not an enum: it is theirs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// **A token, never a colour value and never a path.** `color:<name>` today;
    /// `blob:<hash>` when a person can upload a picture. The prefix is what
    /// makes that addition free — a reader splits on the first colon and
    /// branches — where a bare `purple` would have needed a migration.
    ///
    /// The store never interprets it. A client maps `color:purple` back to
    /// whatever purple is in the theme it is drawing, which is what keeps one
    /// theme's rendering of a colour out of the person's profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// Emoji and named tile colour used by profile switchers. Kept separate
    /// from `avatar` so a profile can have both an icon and a colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Exact `koru:<canonical-v4-identity-uuid>` after acknowledged attachment;
    /// `None` while local-only. Never an email or provider subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// BCP 47. Belongs to the person, never to their content.
    pub locale: Locale,
    pub created_at: Timestamp,
    /// Persona or legacy compatibility holder. See [`ProfileKind`].
    ///
    /// `#[serde(default)]` keeps profiles written before the field readable.
    /// Current creation always writes `Normal`.
    #[serde(default)]
    pub kind: ProfileKind,
}

impl Profile {
    pub fn new(name: impl Into<String>, locale: Locale, at: Timestamp) -> Result<Self> {
        let name = validate_name(name.into(), "profile name")?;
        Ok(Self {
            id: ProfileId::new(),
            name,
            purpose: None,
            avatar: None,
            icon: None,
            color: None,
            account: None,
            locale,
            created_at: at,
            kind: ProfileKind::Normal,
        })
    }

    /// Construct a legacy holder fixture for migration/compatibility tests.
    /// Production Profile creation must use [`Profile::new`]; no active flow
    /// calls this constructor or migrates a Workspace to its result.
    pub fn holder(locale: Locale, at: Timestamp) -> Result<Self> {
        Ok(Self {
            kind: ProfileKind::Holder,
            ..Self::new(HOLDER_NAME, locale, at)?
        })
    }

    /// Whether this is a legacy holder row rather than a persona.
    /// Surfaces and persona-only operations use this single exclusion check.
    pub fn is_holder(&self) -> bool {
        self.kind == ProfileKind::Holder
    }

    /// Rename, under the same character-counted limits as creation — the
    /// counterpart of [`Workspace::rename`].
    pub fn rename(&mut self, name: impl Into<String>) -> Result<()> {
        self.name = validate_name(name.into(), "profile name")?;
        Ok(())
    }

    /// What this profile is for, as the person described it.
    ///
    /// Trimmed and dropped when empty, so an untouched field cannot become
    /// `Some("")` — which reads as "they chose nothing" everywhere except a
    /// comparison, where it is a third state nobody wrote a branch for.
    pub fn with_purpose(mut self, purpose: Option<&str>) -> Self {
        self.purpose = non_empty(purpose);
        self
    }

    /// See [`Profile::avatar`] for the token's shape. **Not validated here**:
    /// the store does not interpret it, and a persistence layer that refused a
    /// token it had not heard of would have to be updated before any client
    /// could adopt a new kind.
    pub fn with_avatar(mut self, avatar: Option<&str>) -> Self {
        self.avatar = non_empty(avatar);
        self
    }

    pub fn with_appearance(mut self, icon: Option<&str>, color: Option<&str>) -> Self {
        self.icon = non_empty(icon);
        self.color = non_empty(color);
        self
    }
}

/// Stored token used only by legacy holder compatibility fixtures.
pub const HOLDER_NAME: &str = "No profile";

/// `Some` only when there is something in it. See [`Profile::with_purpose`].
fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

// ------------------------------------------------------------------ workspace

/// The unit of export, backup and — later — sharing.
///
/// The same logical workspace may exist as several physical replicas: one per
/// release channel, one per machine, one in the cloud. They share [`id`] and
/// **never share a database file**.
///
/// [`id`]: Workspace::id
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Workspace {
    pub id: WorkspaceId,
    /// The Profile this Workspace is listed under.
    ///
    /// **A one-way projection, not the authority.** Since catalog 13 the
    /// Workspace's root catalog unit (`placement.current`, read through
    /// `Store::root_placement`) says where this user has placed it, and a
    /// *move* writes this field only through that root operation, in the
    /// same catalog transaction as the unit. Creation (Plan 1.3) and replica
    /// provisioning (Plan 2.1) publish the unit with the row; a remote root
    /// unit re-projects this field when it lands. A Workspace without a unit
    /// is read from here, as the only evidence there is.
    pub owner: ProfileId,
    pub name: String,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_opened_at: Option<Timestamp>,
    /// How the workspace appears in a switcher: a picture, a colour, and the two
    /// flags that decide where it sits in the list.
    ///
    /// **Stored as tokens, never as rendered values.** `icon` is an emoji or a
    /// `blob:<hash>`; `color` is one of the app's colour *names*, not `#6B4FC4`.
    /// Writing a hex value would freeze one theme's purple into somebody's
    /// workspace for good — the same rule onboarding follows for `avatar` and
    /// `purpose` ([ADR-82](../../../docs/decisions/ADR-076-100.md#adr-82-i18n--internationalization-constrains-what-is-stored-not-how-it-is-shown)).
    ///
    /// The store never interprets any of them. They are here because they have
    /// to survive a restart and the store is the only thing that persists — the
    /// app's `ProfileDoc` is a view rebuilt from this every launch, so a flag
    /// written only there was written to a cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    /// Soft deletion is independent of archive. The Store keeps every byte
    /// until retention purges it, and restoring clears only this timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<Timestamp>,
    /// Stable position in the owner's workspace list. The list may still group
    /// pinned, archived and deleted rows; this orders rows within those groups.
    #[serde(default)]
    pub position: i64,
}

impl Workspace {
    pub fn new(owner: ProfileId, name: impl Into<String>, at: Timestamp) -> Result<Self> {
        let name = validate_name(name.into(), "workspace name")?;
        Ok(Self {
            id: WorkspaceId::new(),
            owner,
            name,
            created_at: at,
            last_opened_at: None,
            icon: None,
            color: None,
            pinned: false,
            archived: false,
            deleted_at: None,
            position: 0,
        })
    }

    /// Rename it, applying the same validation a new one gets.
    ///
    /// Same rule as creation and not a looser one: a name that could not be
    /// given at creation must not be reachable by renaming into it. Length is
    /// counted in **characters**, so a 200-byte limit does not silently become
    /// 66 in Japanese (ADR-82).
    pub fn rename(&mut self, name: impl Into<String>) -> Result<()> {
        self.name = validate_name(name.into(), "workspace name")?;
        Ok(())
    }
}

// ------------------------------------------------------------------ document

/// What a Loro document is, to the store.
///
/// The store knows only these two. What is *inside* a `Document` is the
/// application's business — opaque checkpoint and update bytes — which is what
/// keeps the store out of the block model.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    /// The one per workspace that lists the others.
    Manifest,
    /// Everything else.
    Document,
}

impl DocumentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Document => "document",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "manifest" => Some(Self::Manifest),
            "document" => Some(Self::Document),
            _ => None,
        }
    }
}

impl fmt::Display for DocumentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An entry in a workspace's manifest — the store's view of one document.
///
/// This is the *derived* shape: it is mirrored out of the manifest `LoroDoc`
/// into SQL so a workspace can be listed without opening every document. The
/// manifest is the truth; this is a rebuildable index of it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// As with `Timestamp`: the doc comment above is for whoever maintains this and
// names what is underneath. What a caller needs is what an entry *is*.
#[cfg_attr(
    feature = "schema",
    schemars(
        description = "One document as its workspace lists it: identity, where it sits \
                            among its siblings, its title and kind, and whether it is in the \
                            trash. Listing a workspace opens none of its documents."
    )
)]
pub struct DocumentEntry {
    pub id: DocumentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<DocumentId>,
    /// Position among siblings. Data, not list position, so a reorder is a
    /// field change rather than a structural rewrite.
    pub ordinal: u32,
    pub title: String,
    /// The application's own kind — "page", "task-list", whatever it defines.
    /// A stable token, and the store never interprets it.
    pub kind: String,
    /// MIME type for opaque file entries. Mirrored into the manifest so a
    /// listing can choose an icon without opening the document snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// The note attached to a file, as its document id. `Some` only on a file
    /// that has one; the note itself is an ordinary document registered as the
    /// file's child, so the trash and the purge already follow it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<DocumentId>,
    /// When this document was put in the trash, if it was.
    ///
    /// **Deleting a document is a move, not a delete**
    /// ([ADR-69](../../../docs/decisions/ADR-051-075.md#adr-69--trashing-a-document-is-a-move-into-a-hidden-library-node-and-nothing-ever-purges-it)): losing content is the one
    /// unrecoverable bug, so *Move to trash* sets this and nothing else. The
    /// entry stays, its bytes stay, and it is absent from a listing only because
    /// whoever draws the listing filters on this field.
    ///
    /// A timestamp rather than a `bool` because the answer to "when did this go"
    /// is worth having and costs the same, and because a sweep that empties the
    /// trash on age has something to sort on if one is ever wanted. Nothing
    /// purges on a timer today.
    ///
    /// `#[serde(default)]` so a manifest written before this existed reads as
    /// *not trashed*, which is the truth about every document in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed_at: Option<Timestamp>,
    /// A self-declared display label for whoever binned it — "Steven", an email,
    /// whatever the profile says. `None` is normal.
    ///
    /// **A label, not an identity.** The store does not resolve it, compare it,
    /// or believe it: it exists so a Trash listing can say *who* without the
    /// window having to look up a profile that may belong to another machine.
    /// Anything that needed to *trust* this would need a `ProfileId` and a way
    /// to verify it, which is a different feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed_by: Option<String>,
}

// ------------------------------------------------------------------ blobs

/// `<algorithm>:<digest>` — `b3:<64 hex>`.
///
/// The algorithm is in the value because a hash that does not say what it is
/// cannot be migrated: adopting a second one has to be additive, or every
/// document already written is orphaned.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct BlobHash(String);

impl BlobHash {
    pub const BLAKE3: &'static str = "b3";

    // **There is no `BlobHash::of(bytes)` here.** Hashing belongs to the blob
    // store, which is the only component that sees blob bytes; core parses and
    // carries the value. A convenience constructor here would pull `blake3`
    // into a crate that mobile embeds and that never touches a byte of content.

    pub fn from_digest(algorithm: &str, hex_digest: &str) -> Result<Self> {
        if algorithm.is_empty() || !algorithm.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(Error::new(
                Code::InvalidRequest,
                "blob algorithm is not a token",
            ));
        }
        if hex_digest.is_empty() || !hex_digest.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::new(Code::InvalidRequest, "blob digest is not hex"));
        }
        Ok(Self(format!(
            "{algorithm}:{}",
            hex_digest.to_ascii_lowercase()
        )))
    }

    pub fn parse(s: &str) -> Result<Self> {
        let (alg, digest) = s
            .split_once(':')
            .ok_or_else(|| Error::new(Code::InvalidRequest, "blob hash has no algorithm prefix"))?;
        Self::from_digest(alg, digest)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn algorithm(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }

    pub fn digest(&self) -> &str {
        self.0.split_once(':').map(|(_, d)| d).unwrap_or("")
    }

    /// The on-disk name. **The hash, always** — never the user's filename,
    /// which is arbitrary Unicode and not a path component.
    pub fn file_name(&self) -> String {
        format!("{}-{}", self.algorithm(), self.digest())
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A typed value in a [`Blob`]'s attribute map.
///
/// The map is the extension point: a new fact about a blob is a new key, never
/// a new column, so adding one does not touch the schema, the wire framing or
/// older readers (they ignore keys they do not know). Values stay typed so a
/// reader never has to guess whether `"7"` is a number.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum BlobAttribute {
    Int(i64),
    Text(String),
    /// One of a fixed set of tokens, spelled in the key's documentation —
    /// the same rule as every other enum in this file.
    Token(String),
}

/// Well-known keys in [`Blob::attributes`]. A key is a stable storage name:
/// renaming one orphans the values already written under it.
pub mod blob_attr {
    /// The device that first stored the blob, as its DeviceId storage-form
    /// UUID, in a [`super::BlobAttribute::Text`]. **Provenance, not routing:**
    /// any peer holding the bytes may serve them, and a hydrated device holds
    /// them too without ever becoming the origin. First writer wins — a later
    /// store of the same bytes keeps the original value.
    pub const ORIGIN_DEVICE: &str = "origin_device";
}

/// What the CRDT references, and what the catalog records.
///
/// The bytes are never in Loro; this is.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Blob {
    pub hash: BlobHash,
    pub size: u64,
    pub mime: String,
    /// The name the person's file had. **Metadata only** — arbitrary Unicode,
    /// never identity, never a path component, never emitted into a header
    /// unencoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub created_at: Timestamp,
    /// Typed extra facts, keyed by a `blob_attr` constant. Additive on the
    /// wire and in storage: absent reads as empty, and an unknown key is kept
    /// but ignored.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub attributes: std::collections::BTreeMap<String, BlobAttribute>,
}

impl Blob {
    /// The value of one well-known attribute as text, when it is present and
    /// is text. A key documented as text that holds another type reads as
    /// absent rather than guessing.
    pub fn text_attribute(&self, key: &str) -> Option<&str> {
        match self.attributes.get(key) {
            Some(BlobAttribute::Text(value)) | Some(BlobAttribute::Token(value)) => Some(value),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ lineage

/// How a workspace came to exist here.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LineageKind {
    /// Another replica of the same logical workspace. The id is preserved, so
    /// it must stay able to converge with its origin.
    Restore,
    /// An independent copy. New workspace id *and* new document ids.
    Duplicate,
}

impl LineageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Restore => "restore",
            Self::Duplicate => "duplicate",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "restore" => Some(Self::Restore),
            "duplicate" => Some(Self::Duplicate),
            _ => None,
        }
    }
}

impl fmt::Display for LineageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Lineage {
    pub workspace: WorkspaceId,
    pub derived_from: WorkspaceId,
    pub kind: LineageKind,
    pub at: Timestamp,
}

// ------------------------------------------------------------------ api clients

/// What a credential is allowed to do.
///
/// Coarse on purpose. The MVP builds enforcement and no management UI, so the
/// set has to be one a person could reason about in a permission dialog that
/// does not exist yet.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    ReadDocuments,
    EditDocuments,
    ReadTasks,
    ManageWorkspaces,
    /// **Removing a workspace, as distinct from managing one.**
    ///
    /// Archiving, soft-deleting and restoring close every terminal the
    /// workspace owns and hide its projects; creating, renaming, recolouring,
    /// reordering and importing do not. Those were one scope until SP-8, which
    /// meant the narrowest credential that could rename a workspace could also
    /// delete it — and could do so through this route, around the session
    /// shutdown the Gateway's management operation performs first. That is the
    /// bypass this variant closes: the lower route now refuses the same caller
    /// the upper one refuses.
    DestroyWorkspaces,
    /// **Giving a workspace to another profile**, as distinct from both.
    ///
    /// A move destroys nothing and creates nothing — the id, the documents and
    /// the history are the same afterwards — so `destroy_workspaces` is the
    /// wrong permission to reuse, and `manage_workspaces` is far too wide: it
    /// sits in every ordinary token, and a move closes every terminal the
    /// workspace owns, which is not something a note-filing MCP tool should be
    /// able to cause. Its own variant, so a credential can be given exactly one
    /// of the three.
    MigrateWorkspaces,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadDocuments => "read_documents",
            Self::EditDocuments => "edit_documents",
            Self::ReadTasks => "read_tasks",
            Self::ManageWorkspaces => "manage_workspaces",
            Self::DestroyWorkspaces => "destroy_workspaces",
            Self::MigrateWorkspaces => "migrate_workspaces",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read_documents" => Some(Self::ReadDocuments),
            "edit_documents" => Some(Self::EditDocuments),
            "read_tasks" => Some(Self::ReadTasks),
            "manage_workspaces" => Some(Self::ManageWorkspaces),
            "destroy_workspaces" => Some(Self::DestroyWorkspaces),
            "migrate_workspaces" => Some(Self::MigrateWorkspaces),
            _ => None,
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::ReadDocuments,
        Self::EditDocuments,
        Self::ReadTasks,
        Self::ManageWorkspaces,
        Self::DestroyWorkspaces,
        Self::MigrateWorkspaces,
    ];
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A holder of a scoped credential. Not a person and not a device.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApiClient {
    pub id: ClientId,
    pub name: String,
    pub scopes: Vec<Scope>,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<Timestamp>,
}

impl ApiClient {
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }

    pub fn allows(&self, scope: Scope) -> bool {
        self.active() && self.scopes.contains(&scope)
    }
}

// ------------------------------------------------------------------ shared

/// The one rule every user-supplied name obeys.
///
/// Length is counted in **characters, not bytes** — a 64-byte limit is 64
/// letters in English and 21 in Japanese, which is not a limit, it is a
/// different product in each language.
///
/// Nothing else is enforced. Names are not normalised, not case-folded, not
/// stripped of bidi controls: they are the person's, and the store is not the
/// place that decides what a name may look like.
fn validate_name(name: String, what: &'static str) -> Result<String> {
    const MAX_CHARS: usize = 200;
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(Error::new(Code::InvalidRequest, format!("{what} is empty")));
    }
    if trimmed.chars().count() > MAX_CHARS {
        return Err(Error::new(
            Code::InvalidRequest,
            format!("{what} is longer than {MAX_CHARS} characters"),
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests;
