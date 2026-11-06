// SPDX-License-Identifier: MPL-2.0

//! Stable identifiers.
//!
//! Everything the user can point at gets a UUID that survives restarts, moves
//! between scopes, and sync. The old model used a per-document `u64` counter,
//! which was fine for one session and useless as a persistent key.

use std::fmt;

use uuid::Uuid;

use crate::tasks::Bucket;

/// Declares a newtype over `Uuid` with the usual conversions.
macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }

            /// The form stored in Loro and used in URLs.
            pub fn to_storage(&self) -> String {
                self.0.to_string()
            }

            pub fn parse(s: &str) -> Option<Self> {
                Uuid::parse_str(s).ok().map(Self)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        /// Prefixed and truncated — ids appear in test failures constantly, and
        /// a full UUID drowns the message.
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let s = self.0.simple().to_string();
                write!(f, "{}:{}", $prefix, &s[..8])
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(
    /// A node: task, paragraph, document, table, image, canvas or contact.
    NodeId,
    "node"
);
id_type!(
    /// A profile — "work", "school". Owns a library and a set of workspaces.
    ProfileId,
    "prof"
);
id_type!(
    /// A workspace. Tasks live only inside one.
    WorkspaceId,
    "ws"
);
id_type!(
    /// One attribute on a node. A pill in the text renders one of these.
    AttrId,
    "attr"
);
id_type!(
    /// A tag — a workspace-scoped classification entity ([ADR-176]).
    ///
    /// **The id is the identity; the name is a label.** Nodes store the id, the
    /// workspace system document maps it to a name and an optional color token,
    /// and renaming a tag touches that one record rather than every node that
    /// carries it. Never derived from the name.
    ///
    /// [ADR-176]: ../../../docs/decisions/ADR-176-200.md#adr-199-attributes--attributes-are-typed-values-on-the-node-under-registered-keys-tags-are-entities-and-the-workspace-keeps-a-hidden-system-document
    TagId,
    "tag"
);
id_type!(
    /// A list — a workspace-scoped **filter** over tasks (SPEC §8).
    ///
    /// **A list is not a container.** Membership is one `koru.list` value on
    /// the task, so "the Bike Project list" is the query *select tasks where
    /// list = this id* — the same table Today and Upcoming are, over one more
    /// attribute. Nothing is moved into a list and nothing is nested under one.
    ///
    /// The id is the identity and the name is a label, exactly as for a
    /// [`TagId`]: the workspace system document maps this id to a name, an
    /// accent color and a rail order, and renaming a list touches that one
    /// record rather than any task that carries it.
    ListId,
    "list"
);
id_type!(
    /// A document — one `LoroDoc`, one node tree, one thing you open in a tab.
    ///
    /// **Not a `NodeId`.** A document used to *be* a node inside its workspace's
    /// one `LoroDoc`, so it had a `NodeId` and nothing else; the store addresses
    /// a document directly, gives it a room of its own (`document:<uuid>`) and
    /// lists it in the manifest without opening it. This is that address, and it
    /// is minted by the client like every other id here
    /// ([ADR-39](../../../docs/decisions/README.md)) — never derived from a title, a
    /// path or an ordinal, all three of which change.
    DocumentId,
    "doc"
);

impl DocumentId {
    /// **A document that carries no id of its own** — the nil UUID, and never
    /// an address.
    ///
    /// One reference's middle segment names the document its node is in
    /// ([`WorkspaceDoc::own_document_id`](crate::schema::WorkspaceDoc::own_document_id)),
    /// and every document the store hands out has an id stamped into it the
    /// moment it is opened. This is what is left: an in-memory document built
    /// by a test or by a headless caller, which was never registered anywhere
    /// and has no id to tell the truth with.
    ///
    /// It is a **constant** rather than a fresh UUID so that every reference
    /// minted in one such document still agrees with every other — which is the
    /// whole property a reference's document segment has to have — and it is
    /// nil so that a reader who does look at one sees *unknown* rather than a
    /// plausible-looking id naming nothing.
    pub const UNIDENTIFIED: Self = Self(Uuid::nil());
}

// ------------------------------------------------------- page documents

/// The namespace every `V1` page-document id and genesis peer is derived in
/// ([ADR-000-pagedocumentidentity]).
///
/// **`V1` names a genesis contract, not a format.** The namespace is what
/// makes `page_document_id` and `genesis_peer` the functions they are, and
/// [`WorkspaceDoc::page_genesis`](crate::schema::WorkspaceDoc::page_genesis)
/// writes the same operations under the same op ids on every device for an
/// id minted here. A change to what genesis writes would put two different
/// contents under one op id — which no amendment can make safe — so such a
/// change is a `V2`: a new namespace, therefore new ids, and every `V1`
/// document keeps `V1`.
///
/// [ADR-000-pagedocumentidentity]: ../../../docs/decisions/ADR-000-pagedocumentidentity.md
pub const PAGE_DOCUMENT_NS_V1: Uuid = uuid::uuid!("69e0aede-4a7b-439c-8912-c36bb3cb2d92");

/// Which Tasks page a page document is the document of.
///
/// A list, by its id, or an agenda bucket, by its token — never by a name or a
/// localized label, both of which change. The storage form is the key of the
/// page's record in the workspace system document and the middle of the
/// string a page document's id is derived from.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum PageKey {
    List(ListId),
    Bucket(Bucket),
}

impl PageKey {
    /// `list:<uuid>` or `bucket:<token>`.
    pub fn to_storage(&self) -> String {
        match self {
            Self::List(id) => format!("list:{}", id.to_storage()),
            Self::Bucket(bucket) => format!("bucket:{}", bucket.as_str()),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        if let Some(id) = s.strip_prefix("list:") {
            return ListId::parse(id).map(Self::List);
        }
        let token = s.strip_prefix("bucket:")?;
        Bucket::ALL
            .into_iter()
            .find(|bucket| bucket.as_str() == token)
            .map(Self::Bucket)
    }
}

impl fmt::Display for PageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_storage())
    }
}

/// The id of generation `generation` of `page`'s document in `workspace`.
///
/// **The one id in Koru that is derived rather than minted** (A1 of the
/// page-document plan; the exception [ADR-39]'s rule now carries). Two
/// devices that each start writing above the same list while offline have no
/// way to agree on a random id, and copying one document into the other is
/// not a CRDT merge; deriving the id is what lets both write to *one*
/// document from the first keystroke, with the genesis making the two starts
/// identical rather than merely mergeable.
///
/// `UUIDv5(PAGE_DOCUMENT_NS_V1, "{workspace}|{page}|{generation}")`, with the
/// ids in their hyphenated storage form and the generation in decimal. A
/// replacement after a deletion is the next generation, so it is a different
/// document with a different id.
///
/// [ADR-39]: ../../../docs/decisions/ADR-026-050.md#adr-39-identity--identity-is-client-minted-document-scoped-and-cloud-ready
pub fn page_document_id(workspace: WorkspaceId, page: PageKey, generation: u64) -> DocumentId {
    let name = format!("{workspace}|{page}|{generation}");
    DocumentId::from_uuid(Uuid::new_v5(&PAGE_DOCUMENT_NS_V1, name.as_bytes()))
}

/// The Loro peer a page document's genesis is written under, and nothing else
/// ever is.
///
/// Loro op ids are `(peer, counter)`, so two replicas writing the same
/// operations under the same peer produce the same op ids, and importing
/// one's genesis into the other is a no-op. Loro's own warning — never reuse
/// a peer across concurrent writers — is exactly why this peer is used for
/// the genesis alone: the genesis is the one write whose content every
/// writer agrees on in advance. [`WorkspaceDoc::page_genesis`] moves off it
/// before the document can take an edit.
///
/// The first eight bytes of `UUIDv5(PAGE_DOCUMENT_NS_V1, "genesis|{document}")`
/// as a big-endian integer; `u64::MAX` is Loro's reserved peer, so that one
/// value steps down by one. Loro's random peers span the whole range, so
/// there is no reserved band to keep this out of — the guard is that no
/// writer ever has two documents under one peer, which a derived peer per
/// document keeps.
///
/// [`WorkspaceDoc::page_genesis`]: crate::schema::WorkspaceDoc::page_genesis
pub fn genesis_peer(document: DocumentId) -> u64 {
    let id = Uuid::new_v5(
        &PAGE_DOCUMENT_NS_V1,
        format!("genesis|{document}").as_bytes(),
    );
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&id.as_bytes()[..8]);
    match u64::from_be_bytes(bytes) {
        u64::MAX => u64::MAX - 1,
        peer => peer,
    }
}

/// Which document holds a node.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum DocRef {
    Profile(ProfileId),
    Workspace(WorkspaceId),
    /// A document in this workspace.
    ///
    /// The variant that stops `Ref::home` being a hint and makes it the usual
    /// answer. While a workspace was one `LoroDoc`, a reference between two
    /// nodes was a reference *inside* one document and `home` was almost always
    /// `None`; with one `LoroDoc` per document, most references cross one.
    Document(DocumentId),
    /// A legacy routed document home, retained for older snapshots and generic
    /// non-document references such as attachments. A `document`-role
    /// attribute is normalized to [`DocumentReference`] and never treats this
    /// stored profile/workspace as authoritative.
    DocumentAt {
        profile: ProfileId,
        workspace: WorkspaceId,
        document: DocumentId,
    },
}

/// A pointer to a node, which may live in another document or another profile.
///
/// `home` is a *cache*, not truth. A node can be promoted from task scope to a
/// workspace or profile library, which moves it between documents; resolution
/// falls back to the profile's `locations` index, and failing that reports the
/// reference as dangling. See `Resolver`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Ref {
    pub node: NodeId,
    pub home: Option<DocRef>,
}

impl Ref {
    pub fn new(node: NodeId) -> Self {
        Self { node, home: None }
    }

    pub fn in_doc(node: NodeId, home: DocRef) -> Self {
        Self {
            node,
            home: Some(home),
        }
    }
}

/// What an attribute attaches to — an entity, named stably.
///
/// **A `NodeId` alone means nothing outside its document**, so a block carries
/// its workspace, its document and its node together, never the node id on its
/// own — and never an offset, a line number, or a tree position, all of which
/// change under concurrent edits. Roadmap D2; Foundation invariant 1.
///
/// The string form ([`to_storage`]) is **four colon-separated segments**,
/// `<kind>:<workspace>:<document>:<node>`, fixed jointly with the store's
/// personal-attribute table (docs/features/trackable/DECISIONS.md Q2), which uses the whole string as
/// an opaque key and validates shape only. A *document-level* entity fills the
/// node segment with the document's **root node id** — the title row's node,
/// which is the document (`Body::title_row`) — so every reference is four
/// segments and a document needs no sentinel.
///
/// [`to_storage`]: EntityRef::to_storage
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum EntityRef {
    /// A whole document — a page, or an opaque workspace file. `root` is the
    /// document's root node (its title row), filling the node segment by the
    /// Q2 convention.
    Document {
        workspace: WorkspaceId,
        document: DocumentId,
        root: NodeId,
    },
    /// One block inside a document. The triple travels together by
    /// construction.
    Block {
        workspace: WorkspaceId,
        document: DocumentId,
        node: NodeId,
    },
    /// The same node, addressed **as a task** — a distinct subject, never a
    /// spelling of the block ([ADR-285]).
    ///
    /// The kind is preserved and never canonicalized away: `Task` and `Block`
    /// over one triple are unequal by construction, their [`to_storage`]
    /// strings differ in the first segment, and the store's personal-attribute
    /// table keys on the whole string — so a note filed against a task does not
    /// leak onto the block and back. [`same_target`] is the one comparison
    /// allowed to cross kinds, named so a call site has to say it means to.
    ///
    /// By convention task surfaces speak `task:` and content surfaces speak the
    /// entity forms.
    ///
    /// [`to_storage`]: EntityRef::to_storage
    /// [`same_target`]: EntityRef::same_target
    /// [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
    Task {
        workspace: WorkspaceId,
        document: DocumentId,
        node: NodeId,
    },
}

impl EntityRef {
    pub fn document(workspace: WorkspaceId, document: DocumentId, root: NodeId) -> Self {
        Self::Document {
            workspace,
            document,
            root,
        }
    }

    pub fn block(workspace: WorkspaceId, document: DocumentId, node: NodeId) -> Self {
        Self::Block {
            workspace,
            document,
            node,
        }
    }

    /// The same node addressed as a task — the spelling task surfaces, event
    /// subjects and a task's personal attributes use.
    pub fn task(workspace: WorkspaceId, document: DocumentId, node: NodeId) -> Self {
        Self::Task {
            workspace,
            document,
            node,
        }
    }

    pub fn workspace_id(&self) -> WorkspaceId {
        match self {
            Self::Document { workspace, .. }
            | Self::Block { workspace, .. }
            | Self::Task { workspace, .. } => *workspace,
        }
    }

    /// The document this entity lives in — its own id for a document, its
    /// container's for a block or a task.
    pub fn document_id(&self) -> DocumentId {
        match self {
            Self::Document { document, .. }
            | Self::Block { document, .. }
            | Self::Task { document, .. } => *document,
        }
    }

    /// The node segment: the block's or task's node, or a document's root node.
    pub fn node_id(&self) -> NodeId {
        match self {
            Self::Document { root, .. } => *root,
            Self::Block { node, .. } | Self::Task { node, .. } => *node,
        }
    }

    /// `document`, `block` or `task` — the first storage segment, so a caller
    /// can tell two references apart without re-parsing either.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Document { .. } => "document",
            Self::Block { .. } => "block",
            Self::Task { .. } => "task",
        }
    }

    /// The triple, **ignoring the kind**. The one comparison allowed to cross
    /// kinds, named so a call site says it means to: `==` is the ordinary
    /// question, and it answers `false` for a task and the block it sits on.
    pub fn same_target(&self, other: &Self) -> bool {
        self.workspace_id() == other.workspace_id()
            && self.document_id() == other.document_id()
            && self.node_id() == other.node_id()
    }

    /// The stable string form, usable as a map key — four segments:
    /// `document:<ws>:<doc>:<root-node>`, `block:<ws>:<doc>:<node>` or
    /// `task:<ws>:<doc>:<node>`.
    pub fn to_storage(&self) -> String {
        let kind = self.kind();
        format!(
            "{kind}:{}:{}:{}",
            self.workspace_id().to_storage(),
            self.document_id().to_storage(),
            self.node_id().to_storage()
        )
    }

    /// Parse the storage form back. An unknown kind is `None`, not an error:
    /// a newer build may name entity kinds this one has never heard of, and a
    /// reader skips what it cannot address.
    pub fn parse(s: &str) -> Option<Self> {
        let mut segments = s.split(':');
        let kind = segments.next()?;
        let workspace = WorkspaceId::parse(segments.next()?)?;
        let document = DocumentId::parse(segments.next()?)?;
        let node = NodeId::parse(segments.next()?)?;
        if segments.next().is_some() {
            return None;
        }
        match kind {
            "document" => Some(Self::Document {
                workspace,
                document,
                root: node,
            }),
            "block" => Some(Self::Block {
                workspace,
                document,
                node,
            }),
            "task" => Some(Self::Task {
                workspace,
                document,
                node,
            }),
            _ => None,
        }
    }
}

/// A stable reference to a Store document and, optionally, one block inside it.
///
/// A document's profile and workspace are deliberately absent: both can change
/// while the document id remains stable. Navigation resolves the current
/// location through Store at the moment the reference is followed.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct DocumentReference {
    pub document: DocumentId,
    pub node: Option<NodeId>,
}

impl DocumentReference {
    pub fn new(document: DocumentId) -> Self {
        Self {
            document,
            node: None,
        }
    }

    pub fn at(document: DocumentId, node: NodeId) -> Self {
        Self {
            document,
            node: Some(node),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_storage() {
        let id = NodeId::new();
        assert_eq!(NodeId::parse(&id.to_storage()), Some(id));
    }

    #[test]
    fn ids_are_unique() {
        let a = NodeId::new();
        let b = NodeId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn parsing_rubbish_fails_rather_than_panicking() {
        assert_eq!(NodeId::parse("not-a-uuid"), None);
        assert_eq!(NodeId::parse(""), None);
    }

    /// Different id types must not be interchangeable — a `WorkspaceId` should
    /// never be usable where a `NodeId` is expected. This is a compile-time
    /// property; the test documents the intent.
    #[test]
    fn id_types_are_distinct() {
        let n = NodeId::new();
        let w = WorkspaceId::from_uuid(n.as_uuid());
        // Same underlying UUID, but the types cannot be compared or mixed.
        assert_eq!(n.to_storage(), w.to_storage());
    }

    #[test]
    fn an_entity_ref_round_trips_through_storage() {
        let doc = EntityRef::document(WorkspaceId::new(), DocumentId::new(), NodeId::new());
        assert_eq!(EntityRef::parse(&doc.to_storage()), Some(doc));

        let block = EntityRef::block(WorkspaceId::new(), DocumentId::new(), NodeId::new());
        assert_eq!(EntityRef::parse(&block.to_storage()), Some(block));

        let task = EntityRef::task(WorkspaceId::new(), DocumentId::new(), NodeId::new());
        assert_eq!(EntityRef::parse(&task.to_storage()), Some(task));
    }

    /// **The kind is preserved, never canonicalized away** (SPEC §3): a `task:`
    /// reference and a `block:` reference to one triple are different subjects,
    /// so personal attributes filed under one do not alias the other. The one
    /// question that crosses the two has to be asked by name.
    #[test]
    fn a_task_reference_is_not_the_block_it_sits_on() {
        let (ws, doc, node) = (WorkspaceId::new(), DocumentId::new(), NodeId::new());
        let task = EntityRef::task(ws, doc, node);
        let block = EntityRef::block(ws, doc, node);

        assert_ne!(task, block);
        assert_ne!(task.to_storage(), block.to_storage());
        assert_eq!(task.to_storage(), format!("task:{ws}:{doc}:{node}"));
        assert_eq!((task.kind(), block.kind()), ("task", "block"));
        assert!(task.same_target(&block), "one node, two subjects");
        assert!(
            !task.same_target(&EntityRef::block(ws, doc, NodeId::new())),
            "a different node is a different target"
        );
    }

    /// The wire convention fixed with the store (docs/features/trackable/DECISIONS.md Q2): four
    /// colon-separated segments, and a document-level reference fills the node
    /// segment with the document's root node.
    #[test]
    fn the_storage_form_is_four_segments() {
        let (ws, doc, root) = (WorkspaceId::new(), DocumentId::new(), NodeId::new());
        assert_eq!(
            EntityRef::document(ws, doc, root).to_storage(),
            format!("document:{ws}:{doc}:{root}")
        );
        assert_eq!(
            EntityRef::block(ws, doc, root).to_storage(),
            format!("block:{ws}:{doc}:{root}")
        );
    }

    /// A block is workspace + document + node together — a node id alone does
    /// not parse, because a `NodeId` means nothing outside its document.
    #[test]
    fn a_block_ref_without_its_document_does_not_parse() {
        let node = NodeId::new();
        assert_eq!(EntityRef::parse(&format!("block:{node}")), None);
        assert_eq!(EntityRef::parse(&node.to_storage()), None);
        // Three segments are not four.
        assert_eq!(
            EntityRef::parse(&format!("block:{}:{node}", DocumentId::new())),
            None
        );
    }

    /// An entity kind from a newer build reads as `None` — skipped, never an
    /// error, and never mistaken for some kind this build does know.
    #[test]
    fn an_unknown_entity_kind_is_none() {
        let (ws, doc, node) = (WorkspaceId::new(), DocumentId::new(), NodeId::new());
        assert_eq!(EntityRef::parse(&format!("group:{ws}:{doc}:{node}")), None);
        assert_eq!(EntityRef::parse("rubbish"), None);
    }

    #[test]
    fn debug_is_short_enough_to_read_in_a_failure() {
        let s = format!("{:?}", NodeId::new());
        assert!(s.starts_with("node:"));
        assert_eq!(s.len(), "node:".len() + 8);
    }
}
