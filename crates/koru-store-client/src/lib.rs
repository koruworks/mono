// SPDX-License-Identifier: MPL-2.0

//! **A client of the Koru Store, on exactly the terms a third party has.**
//!
//! Nothing here reaches into a database, signals a process, or reads a file the
//! store owns — apart from `store.json` and `store.secret`, which exist to be
//! read by clients. Everything else is the HTTP and WebSocket API. That is the
//! point: if this crate can do it, so can anyone, and the first-party editor
//! gets no privileged path when it is cut over.
//!
//! ## Blocking, deliberately
//!
//! See the manifest. The short version: `koru-workspace-host`'s event loop is
//! synchronous, so the client must be too, and a background thread with a
//! channel is the shape that already works in this tree.
//!
//! ## What a client is responsible for
//!
//! The store owns durability; the client owns **its own replica and its own
//! retry**. Concretely:
//!
//! - it keeps a `LoroDoc` and applies edits locally, immediately, without
//!   waiting for anything;
//! - it sends updates and remembers which are unacknowledged;
//! - on reconnect it sends its version vector and lets ordinary Loro
//!   synchronisation resend whatever the store is missing.
//!
//! There is **no durable client-side journal**, and that is a decision rather
//! than an omission: if the store dies before acknowledging, the surviving
//! client still holds the work in memory and resends it on reconnect. A journal
//! would only add value if the *client* also died in that window, and that is a
//! measurement to make before a mechanism to build.

pub mod catalog;
pub mod debug;
pub mod http;
pub mod sync;

pub use catalog::CatalogWatch;
pub use http::{
    Client, Discovery, PublicError, PublicFailure, PublicResult, Renewal, RetryPolicy,
    StreamingBody, VersionSnapshot,
};
/// The bodies these calls carry, re-exported so a caller names one crate.
///
/// They are declared in `koru-store-wire`, which the store names too — that is
/// what makes a renamed field a build failure on both sides rather than a
/// request that silently means something else.
pub use koru_store_wire::attributes::{
    AttributeDefinition, AttributeGroup, AttributeQuery, AttributeQueryResponse, AttributeRow,
    AttributeRowValue, AttributeTag, DateWindow, DueFilter, InstantWindow,
};
pub use koru_store_wire::reminders::*;
pub use koru_store_wire::search::{
    Navigation, RecentQuery, SearchFacets, SearchHit, SearchPage, SearchQuery, SearchRequest,
    SearchResponse,
};
pub use koru_store_wire::tasks::{
    LifecycleFilter, TaskQuery, TaskQueryResponse, TaskRow, TaskSort,
};
pub use koru_store_wire::{
    CreateProfileWithWorkspace, Created, CreatedProfile, DocumentOutline, EnsureLiveWorkspace,
    EnsuredWorkspace, Failure, FirstWorkspace, ImportQuery, LocalProfileRemoval,
    LocalProfileRestoration, ManifestRef, NewDocument, NewProfile, NewWorkspace, OutlineHeading,
    PersonalAttributeValue, PersonalAttributesResponse, ProfileAccountBindingOutcome,
    ProfileAccountClearOutcome, ProfileAppearance, ProfileLifecycleInventory,
    ProfileLifecycleInventoryItem, ProvisionAccountProfile, RenameProfile, RenameWorkspace, Status,
    Stopping, WelcomeProgress, WorkspaceAppearance, WorkspaceAuthority, WorkspaceAuthorityState,
    WorkspaceAvailabilityIssue,
};
pub use sync::{Ack, Connection, Event};
