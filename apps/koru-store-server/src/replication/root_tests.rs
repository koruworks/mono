// SPDX-License-Identifier: MPL-2.0

//! The root-catalog exchange between two (and three) real Stores, driven
//! through the adapter exactly as a sync pass drives it, in memory
//! (`docs/features/root-catalog/TECHNICAL.final.md` §4, §7.2, §9;
//! ROADMAP Plan 2.1 verification). No disk, no network, no runtime: every
//! Store is `SqliteCatalog::in_memory()` over `SqliteBackend::in_memory()`,
//! and every frame is a function call between two adapters sharing one
//! `AuthenticatedPeer`, the way the two directions of one relationship do.
//!
//! The helpers at the top are the sync pass's shape: bind the scope, pull
//! root units, pull Profile units, provision content under the placement the
//! root now knows, arm this device's own modes (a second device initializes
//! its own choices, A1 §2), then content.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Mutex;

use koru_device::{DeviceId, RelationshipId, ReplicationEndpointSecret};
use koru_replication::protocol::{
    AuthorizationFence, MODE_REFUSES_TRANSPORT, POLICY_CHANGED, ROOT_SCOPE_NOT_BOUND,
    ROOT_UNIT_NOT_FOUND,
};
use koru_store_core::EntityRef;
use koru_store_core::model::{Profile, Timestamp};
use koru_store_core::personal_attributes::PersonalAttributeValue;
use koru_store_core::root_catalog::CatalogUnitRef;
use koru_store_core::service::{JoinPhase, JoinUnit, Store};
use koru_store_core::text::Locale;
use koru_store_core::transport::{SetTransportMode, TransportMode, TransportSubject};
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use loro::VersionVector;

use super::*;

const AT: i64 = 1_770_000_000_000;

fn at() -> Timestamp {
    Timestamp::from_millis(AT)
}

fn later(offset_ms: i64) -> Timestamp {
    Timestamp::from_millis(AT + offset_ms)
}

/// One device: a memory-only Store behind the daemon's adapter.
struct Device {
    state: Arc<AppState>,
    adapter: LiveReplicationStore,
}

impl Device {
    fn new(name: &str) -> Self {
        let store = Store::open(
            SqliteCatalog::in_memory().unwrap(),
            SqliteBackend::in_memory(),
        )
        .unwrap();
        let state = Arc::new(AppState::new(store, "<memory>".into(), name.into(), AT));
        let adapter = LiveReplicationStore::new(Arc::clone(&state));
        Self { state, adapter }
    }

    fn store(&self) -> std::sync::MutexGuard<'_, KoruStore> {
        self.state.store.lock().unwrap()
    }

    /// A Profile whose own peer path is armed as `mode`.
    fn profile(&self, name: &str, mode: TransportMode) -> Profile {
        let profile = self
            .store()
            .create_profile(name, None, None, Locale::parse("en").unwrap(), at())
            .unwrap();
        self.arm(TransportSubject::Profile(profile.id), mode);
        profile
    }

    fn workspace(&self, owner: ProfileId, name: &str, mode: TransportMode) -> WorkspaceId {
        let workspace = self.store().create_workspace(owner, name, at()).unwrap().id;
        self.arm(TransportSubject::Workspace(workspace), mode);
        workspace
    }

    /// Set one subject's mode at whatever revision it is at. A Workspace
    /// cannot sit above its Profile, so callers arm the Profile first.
    fn arm(&self, subject: TransportSubject, mode: TransportMode) {
        let mut store = self.store();
        let revision = store.transport_mode(subject).unwrap().revision;
        store
            .set_transport_mode(SetTransportMode {
                subject,
                expected_revision: revision,
                mode,
            })
            .unwrap();
    }

    /// Arm every Profile and every Workspace beneath it, as a person does
    /// by answering the prompt on this device (D258). Lowering carries the
    /// children in one group, as `choose_transport_mode` does.
    fn arm_all(&self, mode: TransportMode) {
        let profiles = self.store().profiles().unwrap();
        for profile in profiles {
            let mut store = self.store();
            let group = store.transport_mode_group(profile.id).unwrap();
            let mut requests = vec![SetTransportMode {
                subject: group.profile.subject,
                expected_revision: group.profile.revision,
                mode,
            }];
            for child in &group.children {
                requests.push(SetTransportMode {
                    subject: child.subject,
                    expected_revision: child.revision,
                    mode,
                });
            }
            store.set_transport_modes(&requests).unwrap();
        }
    }

    fn placement(&self, workspace: WorkspaceId) -> Option<ProfileId> {
        self.store()
            .root_placement(workspace)
            .unwrap()
            .and_then(|placement| placement.profile)
    }

    fn owner_row(&self, workspace: WorkspaceId) -> ProfileId {
        self.store()
            .workspace_including_deleted(workspace)
            .unwrap()
            .owner
    }

    fn deleted_at(&self, workspace: WorkspaceId) -> Option<Timestamp> {
        self.store()
            .workspace_including_deleted(workspace)
            .unwrap()
            .deleted_at
    }

    fn workspace_count(&self) -> usize {
        self.store().workspaces_including_deleted().unwrap().len()
    }

    /// Type into a document and flush it, as a window's room does.
    ///
    /// **A created document is empty.** `create_document` writes the title
    /// and the kind into the *manifest* and leaves the document itself with
    /// no operation at all — the store deliberately does not know what is
    /// inside one (ADR-80), so the first node is seeded by whoever opens it.
    /// Its snapshot therefore carries nothing, and a test that wants to show
    /// content travelling between two devices has to write some first.
    fn type_into(&self, workspace: WorkspaceId, document: DocumentId, peer_id: u64, text: &str) {
        let mut store = self.store();
        let base = store.version(workspace, document).unwrap();
        let live = loro::LoroDoc::new();
        live.import(&store.snapshot(workspace, document).unwrap())
            .unwrap();
        live.set_peer_id(peer_id).unwrap();
        live.get_text("t").insert(0, text).unwrap();
        live.commit();
        let update = live.export(loro::ExportMode::updates(&base)).unwrap();
        store
            .apply_update(
                workspace,
                document,
                update,
                at(),
                koru_store_core::ports::UpdateOrigin::Store,
            )
            .unwrap();
        store.flush(workspace, document, at()).unwrap();
    }
}

fn relationship() -> RelationshipId {
    RelationshipId::new()
}

fn peer_with(
    relationship: RelationshipId,
    all_data: bool,
    profiles: &[ProfileId],
    workspaces: &[WorkspaceId],
) -> AuthenticatedPeer {
    AuthenticatedPeer {
        relationship_id: relationship,
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data,
        authorized_profiles: profiles.iter().copied().collect(),
        authorized_workspaces: workspaces.iter().copied().collect(),
        authorization_fence: AuthorizationFence::active(),
        display_name: None,
        incompatible: false,
        peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
    }
}

fn all_data_peer(relationship: RelationshipId) -> AuthenticatedPeer {
    peer_with(relationship, true, &[], &[])
}

fn decode(version: &[u8]) -> VersionVector {
    VersionVector::decode(version).unwrap_or_default()
}

/// The root part of a sync pass from `source` to `target`: bind, compare,
/// request every unit the receipt is behind on, apply. Answers each unit's
/// outcome so a test can look at one.
fn exchange_root(
    source: &Device,
    target: &Device,
    peer: &AuthenticatedPeer,
) -> Vec<(CatalogUnitRef, Result<RootJoin, StoreFailure>)> {
    assert!(target.adapter.bind_root_scope(peer).unwrap());
    let inventory = source.adapter.root_catalog(peer).unwrap();
    let mut outcomes = Vec::new();
    for remote in inventory.units {
        let unit = remote.unit;
        let remote_version = decode(&remote.version);
        let local = match target.adapter.root_unit_version(peer, unit) {
            Ok(local) => {
                let local = decode(&local);
                if local.includes_vv(&remote_version) {
                    continue;
                }
                local
            }
            Err(error) if error.code == ROOT_UNIT_NOT_FOUND => VersionVector::new(),
            Err(error) => {
                outcomes.push((unit, Err(error)));
                continue;
            }
        };
        let outcome = source
            .adapter
            .root_unit_updates_since(peer, unit, &local)
            .and_then(|bytes| {
                target.adapter.apply_remote_root_updates(
                    peer,
                    unit,
                    vec![bytes],
                    Some(&remote_version),
                )
            });
        outcomes.push((unit, outcome));
    }
    outcomes
}

/// The personal v2 part, after content bootstrap.
fn exchange_personal(source: &Device, target: &Device, peer: &AuthenticatedPeer) {
    let inventory = source.adapter.root_catalog(peer).unwrap();
    for remote in inventory.personal {
        let remote_version = decode(&remote.version);
        let local = match target.adapter.personal_version(peer, remote.workspace) {
            Ok(local) => decode(&local),
            Err(error) if error.code == ROOT_UNIT_NOT_FOUND => VersionVector::new(),
            Err(error) => panic!("personal version: {}", error.code),
        };
        if !local.is_empty() && local.includes_vv(&remote_version) {
            continue;
        }
        let bytes = source
            .adapter
            .personal_updates_since(peer, remote.workspace, &local)
            .unwrap();
        target
            .adapter
            .apply_remote_personal_updates(
                peer,
                remote.workspace,
                vec![bytes],
                Some(&remote_version),
            )
            .unwrap();
    }
}

/// The Profile-unit part: identity rows first, then every unit's bytes.
fn exchange_profiles(source: &Device, target: &Device, peer: &AuthenticatedPeer) {
    let units = source.adapter.profiles(peer).unwrap();
    for state in &units {
        if matches!(
            state.unit,
            koru_store_core::profile_units::ProfileUnitRef::Identity { .. }
        ) {
            target.adapter.provision_profile(peer, state).unwrap();
        }
    }
    // The receiving device's Profiles arrive Local-only, as every row does
    // (migration 12's seed); a person arms them. Done here so the identity
    // and private bytes can be admitted in this same exchange.
    target.arm_all(TransportMode::P2p);
    for state in units {
        let local = decode(&target.adapter.profile_version(peer, state.unit).unwrap());
        if !local.is_empty() && local.includes_vv(&decode(&state.version)) {
            continue;
        }
        let bytes = source
            .adapter
            .profile_updates_since(peer, state.unit, &local)
            .unwrap();
        target
            .adapter
            .apply_remote_profile_updates(peer, state.unit, vec![bytes])
            .unwrap();
    }
}

fn provision(
    source: &Device,
    target: &Device,
    peer: &AuthenticatedPeer,
    workspace: WorkspaceId,
) -> Result<(), StoreFailure> {
    let inventory = source.adapter.inventory(peer, workspace)?;
    target.adapter.provision_replica(peer, &inventory.replica)
}

/// A whole pass in the order the runtime runs it, for the same person's
/// device: root, Profiles, content provisioning, this device's arming.
fn sync(source: &Device, target: &Device, peer: &AuthenticatedPeer) {
    for (unit, outcome) in exchange_root(source, target, peer) {
        outcome.unwrap_or_else(|error| panic!("{unit:?}: {}", error.code));
    }
    exchange_profiles(source, target, peer);
    for row in source.adapter.catalog(peer).unwrap() {
        provision(source, target, peer, row.workspace_id).unwrap();
    }
    target.arm_all(TransportMode::P2p);
    exchange_personal(source, target, peer);
}

fn unit_of(workspace: WorkspaceId) -> CatalogUnitRef {
    CatalogUnitRef::Workspace { workspace }
}

// ------------------------------------------------------------ the scope

/// §4.1: an all-data relationship is bound to this device's scope once, the
/// binding is a `store_state` value under the documented key, a second
/// call changes nothing, and a Profile- or Workspace-granted relationship
/// is never bound.
#[test]
fn an_all_data_peer_is_scope_bound_once_and_the_binding_persists_in_store_state() {
    let device = Device::new("B");
    let bound = relationship();
    let peer = all_data_peer(bound);
    assert!(device.adapter.bind_root_scope(&peer).unwrap());
    let scope = device.store().peer_scope(bound.as_uuid()).unwrap();
    assert!(scope.is_some());
    assert!(device.adapter.bind_root_scope(&peer).unwrap());
    assert_eq!(
        device.store().peer_scope(bound.as_uuid()).unwrap(),
        scope,
        "decided once per relationship"
    );
    assert_eq!(
        koru_store_core::service::peer_scope_key(bound.as_uuid()),
        format!("root_catalog.peer_scope.{}", bound.as_uuid())
    );

    let profile = device.profile("P", TransportMode::P2p);
    let workspace = device.workspace(profile.id, "W", TransportMode::P2p);
    let by_profile = peer_with(relationship(), false, &[profile.id], &[]);
    let by_uuid = peer_with(relationship(), false, &[], &[workspace]);
    for scoped in [&by_profile, &by_uuid] {
        assert!(!device.adapter.bind_root_scope(scoped).unwrap());
        assert!(
            device
                .store()
                .peer_scope(scoped.relationship_id.as_uuid())
                .unwrap()
                .is_none()
        );
    }
}

// -------------------------------------------------------- the §9 scenario

/// W synced A→B, then moved to another Profile on A: the root unit flows to
/// B, B's `workspaces.owner` follows, and B's content sync continues with no
/// `WorkspaceExists` from a descriptor that still says the old Profile.
#[test]
fn a_workspace_moved_after_sync_follows_on_the_peer_without_workspace_exists() {
    let a = Device::new("A");
    let b = Device::new("B");
    let p1 = a.profile("P1", TransportMode::P2p);
    let p2 = a.profile("P2", TransportMode::P2p);
    let w = a.workspace(p1.id, "W", TransportMode::P2p);
    let (document, _) = a
        .store()
        .create_document(w, "Plan", "page", None, None, at())
        .unwrap();
    a.type_into(w, document, 1, "the plan");
    let peer = all_data_peer(relationship());

    sync(&a, &b, &peer);
    assert_eq!(b.placement(w), Some(p1.id));
    assert_eq!(b.owner_row(w), p1.id);
    assert_eq!(b.store().profiles().unwrap().len(), 2);
    // A receipt per unit, reconciled: nothing more to pull.
    let receipt = b
        .store()
        .join_receipt(
            peer.relationship_id.as_uuid(),
            JoinUnit::WorkspaceCatalog { workspace: w },
        )
        .unwrap()
        .expect("a receipt for W's unit");
    assert_eq!(receipt.phase, JoinPhase::Reconciled);

    // Content: the document provisions and its bytes travel.
    b.adapter.provision_document(&peer, w, document).unwrap();
    let before = b.adapter.version(&peer, w, document).unwrap();
    let bytes = a.adapter.snapshot(&peer, w, document).unwrap();
    b.adapter
        .apply_remote_updates(&peer, w, document, vec![bytes])
        .unwrap();
    assert_ne!(b.adapter.version(&peer, w, document).unwrap(), before);

    // A moves W to P2. The move is a root operation; the descriptor A will
    // send next names P2, and B's own row still says P1 until the unit comes.
    a.store().move_workspace(w, p2.id).unwrap();
    assert_eq!(a.placement(w), Some(p2.id));
    let outcomes = exchange_root(&a, &b, &peer);
    let (_, outcome) = outcomes
        .iter()
        .find(|(unit, _)| *unit == unit_of(w))
        .expect("W's unit was pulled");
    assert!(matches!(
        outcome,
        Ok(RootJoin::Applied {
            reconciled: true,
            ..
        })
    ));
    assert_eq!(b.placement(w), Some(p2.id));
    assert_eq!(b.owner_row(w), p2.id, "the projection follows the root");

    // Content continues: no owner-only `WorkspaceExists`, in either order of
    // descriptor and unit.
    provision(&a, &b, &peer, w).expect("a moved Workspace is the same replica");
    let inventory = a.adapter.inventory(&peer, w).unwrap();
    assert_eq!(
        inventory.replica.profile_id, p2.id,
        "the hint is the current placement"
    );
    assert!(b.adapter.inventory(&peer, w).is_ok());
    assert_eq!(b.adapter.catalog(&peer).unwrap()[0].profile_id, p2.id);
    // And the other direction still provisions nothing new on A.
    let a_count = a.workspace_count();
    provision(&b, &a, &peer, w).unwrap();
    assert_eq!(a.workspace_count(), a_count);
}

/// Remote updates never create a Workspace or a default: root units alone
/// leave no `workspaces` row, and provisioning adds exactly the advertised
/// Workspaces and nothing beside them.
#[test]
fn remote_root_updates_never_create_a_workspace_or_a_default() {
    let a = Device::new("A");
    let b = Device::new("B");
    let p = a.profile("P", TransportMode::P2p);
    let w = a.workspace(p.id, "W", TransportMode::P2p);
    let peer = all_data_peer(relationship());
    for (_, outcome) in exchange_root(&a, &b, &peer) {
        outcome.unwrap();
    }
    assert_eq!(b.workspace_count(), 0, "a unit is placement, not content");
    assert!(b.store().root_placement(w).unwrap().is_some());
    exchange_profiles(&a, &b, &peer);
    assert_eq!(b.workspace_count(), 0);
    provision(&a, &b, &peer, w).unwrap();
    let rows = b.store().workspaces_including_deleted().unwrap();
    assert_eq!(
        rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![w],
        "exactly the advertised Workspace; no default beside it"
    );
}

// ------------------------------------------------------------ provisioning

/// Missing root state is pending, not owner spoofing: a descriptor alone
/// provisions nothing on the same person's device, and the same descriptor
/// provisions once the unit has arrived.
#[test]
fn missing_root_state_holds_provisioning_pending_until_the_unit_arrives() {
    let a = Device::new("A");
    let b = Device::new("B");
    let p = a.profile("P", TransportMode::P2p);
    let w = a.workspace(p.id, "W", TransportMode::P2p);
    let peer = all_data_peer(relationship());
    assert!(b.adapter.bind_root_scope(&peer).unwrap());
    exchange_profiles(&a, &b, &peer);

    let inventory = a.adapter.inventory(&peer, w).unwrap();
    assert_eq!(
        b.adapter
            .provision_replica(&peer, &inventory.replica)
            .unwrap_err()
            .code,
        PLACEMENT_PENDING
    );
    assert_eq!(b.workspace_count(), 0, "nothing was written");
    assert!(b.store().root_placement(w).unwrap().is_none());

    for (_, outcome) in exchange_root(&a, &b, &peer) {
        outcome.unwrap();
    }
    b.adapter
        .provision_replica(&peer, &inventory.replica)
        .expect("the same descriptor provisions once placement is known");
    assert_eq!(b.owner_row(w), p.id);
    // Idempotent: the same inventory again changes nothing and raises nothing.
    b.adapter
        .provision_replica(&peer, &inventory.replica)
        .unwrap();
    assert_eq!(b.workspace_count(), 1);
}

/// A Workspace-granted peer's root frames are refused and install no
/// placement; its content for an already-known Workspace continues by UUID.
#[test]
fn a_workspace_grant_peers_root_frames_are_refused_and_its_content_continues() {
    let a = Device::new("A");
    let b = Device::new("B");
    let p = a.profile("P", TransportMode::P2p);
    let w = a.workspace(p.id, "W", TransportMode::P2p);
    let same_person = all_data_peer(relationship());
    sync(&a, &b, &same_person);

    // A second relationship, Workspace-scoped: another person's device. It
    // may know no Profile, so a Workspace unit whose current placement names
    // one is held by the export boundary (§4.3) and is not even listed; it
    // gets no Profile unit and no personal unit.
    let scoped = peer_with(relationship(), false, &[], &[w]);
    assert!(!b.adapter.bind_root_scope(&scoped).unwrap());
    let inventory = a.adapter.root_catalog(&scoped).unwrap();
    assert!(
        inventory.units.is_empty(),
        "a placed Workspace unit is held from a peer that may know no Profile"
    );
    assert!(inventory.personal.is_empty());
    assert_eq!(
        a.adapter
            .root_unit_updates_since(&scoped, unit_of(w), &VersionVector::new())
            .unwrap_err()
            .code,
        koru_replication::protocol::ROOT_UNIT_HELD
    );
    // Even bytes it somehow obtained install nothing: the relationship is
    // not bound to this device's scope.
    let bytes = a
        .adapter
        .root_unit_updates_since(&same_person, unit_of(w), &VersionVector::new())
        .unwrap();
    let placement_before = b.placement(w);
    assert_eq!(
        b.adapter
            .apply_remote_root_updates(&scoped, unit_of(w), vec![bytes], None)
            .unwrap_err()
            .code,
        ROOT_SCOPE_NOT_BOUND
    );
    assert_eq!(b.placement(w), placement_before, "nothing installed");
    assert!(
        b.store()
            .join_receipt(
                scoped.relationship_id.as_uuid(),
                JoinUnit::WorkspaceCatalog { workspace: w }
            )
            .unwrap()
            .is_none(),
        "no receipt for a relationship that may not join"
    );
    assert_eq!(
        b.adapter.personal_version(&scoped, w).unwrap_err().code,
        ROOT_SCOPE_NOT_BOUND
    );
    // Content by UUID continues on both sides.
    assert!(b.adapter.inventory(&scoped, w).is_ok());
    provision(&a, &b, &scoped, w).unwrap();
    assert_eq!(b.owner_row(w), p.id);

    // A Workspace-granted peer provisioning a Workspace this device does not
    // know waits for this device's own root: never the sender's Profile.
    let c = Device::new("C");
    c.profile("Own", TransportMode::P2p);
    assert_eq!(
        provision(&a, &c, &scoped, w).unwrap_err().code,
        PLACEMENT_PENDING
    );
    assert_eq!(c.workspace_count(), 0);
}

// ---------------------------------------------------------------- grants

/// A Profile grant follows current placement: a move out closes that peer's
/// access on the next check, the destination's grant opens it, and a
/// UUID grant keeps it throughout — none of them panics or breaks the
/// session's other grants.
#[test]
fn profile_grant_sessions_lose_a_moved_out_workspace_and_uuid_grants_keep_it() {
    let a = Device::new("A");
    let from = a.profile("From", TransportMode::P2p);
    let to = a.profile("To", TransportMode::P2p);
    let w = a.workspace(from.id, "W", TransportMode::P2p);
    let other = a.workspace(from.id, "Other", TransportMode::P2p);
    let from_peer = peer_with(relationship(), false, &[from.id], &[]);
    let to_peer = peer_with(relationship(), false, &[to.id], &[]);
    let uuid_peer = peer_with(relationship(), false, &[], &[w]);

    assert!(a.adapter.inventory(&from_peer, w).is_ok());
    assert!(a.adapter.inventory(&uuid_peer, w).is_ok());
    assert_eq!(
        a.adapter.inventory(&to_peer, w).unwrap_err().code,
        "unauthorized_workspace"
    );
    assert_eq!(
        a.adapter
            .root_catalog(&from_peer)
            .unwrap()
            .units
            .iter()
            .filter(|unit| unit.unit.workspace().is_some())
            .count(),
        2
    );

    a.store().move_workspace(w, to.id).unwrap();

    assert_eq!(
        a.adapter.inventory(&from_peer, w).unwrap_err().code,
        "unauthorized_workspace",
        "the Profile grant is evaluated against the current placement"
    );
    assert!(
        a.adapter.inventory(&from_peer, other).is_ok(),
        "the session keeps its other grants"
    );
    assert!(a.adapter.inventory(&to_peer, w).is_ok());
    assert!(
        a.adapter.inventory(&uuid_peer, w).is_ok(),
        "a UUID grant follows its Workspace"
    );
    let from_units = a.adapter.root_catalog(&from_peer).unwrap().units;
    assert!(
        !from_units.iter().any(|unit| unit.unit == unit_of(w)),
        "the moved-out Workspace's unit leaves the Profile grant's inventory"
    );
    assert!(
        a.adapter
            .root_catalog(&to_peer)
            .unwrap()
            .units
            .iter()
            .any(|unit| unit.unit == unit_of(w))
    );
    assert!(
        !a.adapter
            .catalog(&from_peer)
            .unwrap()
            .iter()
            .any(|row| row.workspace_id == w)
    );
    assert_eq!(
        a.adapter
            .root_unit_updates_since(&from_peer, unit_of(w), &VersionVector::new())
            .unwrap_err()
            .code,
        koru_replication::protocol::ROOT_UNIT_HELD,
        "a unit outside the grant is held, indistinguishably from one that may not leave"
    );
    // The UUID-granted session is still subject to fences.
    uuid_peer.authorization_fence.revoke();
    assert_eq!(
        a.adapter.inventory(&uuid_peer, w).unwrap_err().code,
        "relationship_revoked"
    );
}

// ---------------------------------------------------------------- privacy

/// Everything an event carried, as text: field names and values.
struct Capture(Mutex<String>);

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Visitor<'a>(&'a mut String);
        impl tracing::field::Visit for Visitor<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                let _ = write!(self.0, "{}={:?} ", field.name(), value);
            }
        }
        let mut text = self.0.lock().unwrap();
        event.record(&mut Visitor(&mut text));
        text.push('\n');
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Every byte in a frame's history as JSON text, so a hidden id or name can
/// be searched for in keys, values and redacted spans alike.
fn history_text(bytes: &[u8]) -> String {
    let doc = loro::LoroDoc::new();
    let status = doc.import(bytes).unwrap();
    assert!(status.pending.is_none(), "derived bytes apply completely");
    serde_json::to_string(
        &doc.export_json_updates_without_peer_compression(
            &VersionVector::default(),
            &doc.oplog_vv(),
        ),
    )
    .unwrap()
}

/// A Local-only Profile with sentinel names, a hidden child and a historical
/// placement of a visible Workspace under it: no framed byte — unit lists,
/// digests, exported histories, descriptors, content inventories or the log
/// lines of the whole exchange — carries its UUID or its names.
#[test]
fn a_local_only_profile_leaks_nothing_through_frames_digests_or_logs() {
    const HIDDEN_PROFILE: &str = "SENTINEL-HIDDEN-PROFILE";
    const HIDDEN_WORKSPACE: &str = "SENTINEL-HIDDEN-WORKSPACE";
    let a = Device::new("A");
    let b = Device::new("B");
    // The visible side.
    let visible = a.profile("Visible", TransportMode::P2p);
    // The hidden side, armed as P2P first so W can be created and moved
    // under it, then closed for good.
    let hidden = a.profile(HIDDEN_PROFILE, TransportMode::P2p);
    let hidden_workspace = a.workspace(hidden.id, HIDDEN_WORKSPACE, TransportMode::P2p);
    // A visible Workspace whose history names the hidden Profile.
    let w = a.workspace(hidden.id, "W", TransportMode::P2p);
    a.store().move_workspace(w, visible.id).unwrap();
    a.arm_all_profile(hidden.id, TransportMode::LocalOnly);
    let hidden_ids = [hidden.id.to_storage(), hidden_workspace.to_storage()];
    let peer = all_data_peer(relationship());

    let capture = Arc::new(Capture(Mutex::new(String::new())));
    let mut framed = String::new();
    tracing::subscriber::with_default(Arc::clone(&capture), || {
        let inventory = a.adapter.root_catalog(&peer).unwrap();
        for unit in &inventory.units {
            framed.push_str(&unit.unit.subject_id());
            framed.push_str(&format!("{:?}", unit.version));
        }
        let digest = koru_replication::protocol::root_catalog_digest(
            &peer,
            &inventory.units,
            &inventory.personal,
        )
        .unwrap();
        framed.push_str(&format!("{digest:?}"));
        assert_eq!(
            inventory
                .units
                .iter()
                .filter(|unit| unit.unit.profile().is_some())
                .count(),
            1,
            "one Profile unit: the visible one"
        );
        assert_eq!(
            inventory
                .units
                .iter()
                .filter(|unit| unit.unit.workspace().is_some())
                .count(),
            1,
            "one Workspace unit: W; the hidden child is not even a count"
        );
        for row in a.adapter.catalog(&peer).unwrap() {
            framed.push_str(&row.workspace_id.to_storage());
            framed.push_str(&row.profile_id.to_storage());
        }
        for state in a.adapter.profiles(&peer).unwrap() {
            framed.push_str(&format!("{state:?}"));
        }
        let descriptor = a.adapter.inventory(&peer, w).unwrap().replica;
        framed.push_str(&format!("{descriptor:?}"));
        assert_eq!(
            a.adapter
                .inventory(&peer, hidden_workspace)
                .unwrap_err()
                .code,
            MODE_REFUSES_TRANSPORT
        );
        // The exported history of W, redacted where it named the hidden
        // Profile, and applied on B.
        for (unit, outcome) in exchange_root(&a, &b, &peer) {
            outcome.unwrap_or_else(|error| panic!("{unit:?}: {}", error.code));
        }
        let bytes = a
            .adapter
            .root_unit_updates_since(&peer, unit_of(w), &VersionVector::new())
            .unwrap();
        framed.push_str(&history_text(&bytes));
        framed.push_str(&String::from_utf8_lossy(&bytes));
    });
    let logs = capture.0.lock().unwrap().clone();
    for needle in hidden_ids
        .iter()
        .map(String::as_str)
        .chain([HIDDEN_PROFILE, HIDDEN_WORKSPACE])
    {
        assert!(
            !framed.contains(needle),
            "framed bytes name a hidden subject: {needle}"
        );
        assert!(
            !logs.contains(needle),
            "a log line names a hidden subject: {needle}"
        );
    }
    assert!(
        logs.contains("root_sync"),
        "the exchange did log its phases"
    );
    assert_eq!(b.placement(w), Some(visible.id));
    assert!(
        b.store()
            .root_placement(hidden_workspace)
            .unwrap()
            .is_none()
    );
}

mod acceptance;

impl Device {
    /// Arm one Profile's group, children included.
    fn arm_all_profile(&self, profile: ProfileId, mode: TransportMode) {
        let mut store = self.store();
        let group = store.transport_mode_group(profile).unwrap();
        let mut requests = vec![SetTransportMode {
            subject: group.profile.subject,
            expected_revision: group.profile.revision,
            mode,
        }];
        for child in &group.children {
            requests.push(SetTransportMode {
                subject: child.subject,
                expected_revision: child.revision,
                mode,
            });
        }
        store.set_transport_modes(&requests).unwrap();
    }
}

/// A move into a Local-only Profile waits in the overlay and is framed
/// nowhere; once the destination admits export the overlay is folded into
/// the durable unit **exactly once** and the next inventory carries it.
///
/// **Which call folds it.** §4.3 publishes an overlay "when sync becomes
/// eligible", and for a Workspace the transport-mode commit is where
/// eligibility changes: committing the group that raises the destination
/// Profile's ceiling publishes the overlay in that same call
/// (`Store::publish_newly_exportable_overlays`). The publication job is
/// the backstop for the folds a mode commit cannot do — a Profile unit's
/// overlay, a publication that failed and was only logged, a policy
/// generation that moved under it — so here it must find nothing left.
/// That is the assertion this test exists for: the fold happens once, not
/// once per pass, and the job never folds a second time.
#[test]
fn a_held_overlay_is_folded_exactly_once_when_its_subject_may_export() {
    let a = Device::new("A");
    let open = a.profile("Open", TransportMode::P2p);
    let closed = a.profile("Closed", TransportMode::LocalOnly);
    let w = a.workspace(open.id, "W", TransportMode::P2p);
    let peer = all_data_peer(relationship());
    let before = a
        .adapter
        .root_catalog(&peer)
        .unwrap()
        .units
        .into_iter()
        .find(|unit| unit.unit == unit_of(w))
        .unwrap()
        .version;

    a.store().move_workspace(w, closed.id).unwrap();
    assert_eq!(a.placement(w), Some(closed.id), "the local move succeeded");
    assert_eq!(
        a.adapter.publish_pending_root_overlays().unwrap(),
        0,
        "nothing is published while the destination may not export"
    );
    let inventory = a.adapter.root_catalog(&peer).unwrap();
    assert!(
        !inventory.units.iter().any(|unit| unit.unit == unit_of(w)),
        "a Workspace whose current placement is hidden is not listed"
    );
    assert_eq!(
        a.adapter
            .root_unit_updates_since(&peer, unit_of(w), &VersionVector::new())
            .unwrap_err()
            .code,
        koru_replication::protocol::ROOT_UNIT_HELD
    );

    assert_eq!(
        a.store().pending_overlay_units().unwrap(),
        vec![unit_of(w)],
        "the move waits in W's private overlay"
    );

    a.arm_all_profile(closed.id, TransportMode::P2p);
    assert!(
        a.store().pending_overlay_units().unwrap().is_empty(),
        "the commit that made W exportable folded its overlay (§4.3)"
    );
    assert_eq!(
        a.adapter.publish_pending_root_overlays().unwrap(),
        0,
        "folded once: the job has nothing left to publish"
    );
    assert_eq!(
        a.adapter.publish_pending_root_overlays().unwrap(),
        0,
        "idempotent"
    );
    let after = a
        .adapter
        .root_catalog(&peer)
        .unwrap()
        .units
        .into_iter()
        .find(|unit| unit.unit == unit_of(w))
        .expect("listed again")
        .version;
    assert_ne!(before, after);
    let bytes = a
        .adapter
        .root_unit_updates_since(&peer, unit_of(w), &VersionVector::new())
        .unwrap();
    let b = Device::new("B");
    b.profile("Any", TransportMode::P2p);
    assert!(b.adapter.bind_root_scope(&peer).unwrap());
    b.adapter
        .apply_remote_root_updates(&peer, unit_of(w), vec![bytes], None)
        .unwrap();
    assert_eq!(b.placement(w), Some(closed.id));
}

// --------------------------------------------------- staging and commit

/// §9 "Privacy/authority changes during staged work": a candidate staged
/// under one policy generation is discarded by its commit when the
/// generation moved — here by a local move, and by a mode change to
/// Local-only — and the local work is kept.
#[test]
fn a_change_between_staging_and_commit_discards_the_candidate_and_keeps_local_edits() {
    let a = Device::new("A");
    let b = Device::new("B");
    let p1 = a.profile("P1", TransportMode::P2p);
    let p2 = a.profile("P2", TransportMode::P2p);
    let w = a.workspace(p1.id, "W", TransportMode::P2p);
    let peer = all_data_peer(relationship());
    sync(&a, &b, &peer);
    let p3 = b
        .store()
        .create_profile("P3", None, None, Locale::parse("en").unwrap(), at())
        .unwrap();
    b.arm_all_profile(p3.id, TransportMode::P2p);

    // A moves W to P2; B stages the frame, then B's own user moves W to P3
    // before the commit. The staged candidate is stale and is discarded;
    // B's move stands until the next pass stages again.
    a.store().move_workspace(w, p2.id).unwrap();
    let bytes = a
        .adapter
        .root_unit_updates_since(
            &peer,
            unit_of(w),
            &decode(&b.adapter.root_unit_version(&peer, unit_of(w)).unwrap()),
        )
        .unwrap();
    let staged = b
        .store()
        .stage_root_unit(
            peer.relationship_id.as_uuid(),
            unit_of(w),
            std::slice::from_ref(&bytes),
        )
        .unwrap();
    b.store().move_workspace(w, p3.id).unwrap();
    let error = b
        .store()
        .commit_staged_root_unit(peer.relationship_id.as_uuid(), staged, None)
        .unwrap_err();
    assert_eq!(error.code(), koru_store_core::Code::IdempotencyConflict);
    assert_eq!(b.placement(w), Some(p3.id), "local work is kept");

    // Staged again from the new frontier; this time the mode closes between
    // staging and commit: refused, nothing applied, the edit kept.
    let bytes = a
        .adapter
        .root_unit_updates_since(
            &peer,
            unit_of(w),
            &decode(&b.adapter.root_unit_version(&peer, unit_of(w)).unwrap()),
        )
        .unwrap();
    let staged = b
        .store()
        .stage_root_unit(
            peer.relationship_id.as_uuid(),
            unit_of(w),
            std::slice::from_ref(&bytes),
        )
        .unwrap();
    b.arm_all_profile(p3.id, TransportMode::LocalOnly);
    let error = b
        .store()
        .commit_staged_root_unit(peer.relationship_id.as_uuid(), staged, None)
        .unwrap_err();
    assert!(matches!(
        error.code(),
        koru_store_core::Code::Forbidden | koru_store_core::Code::IdempotencyConflict
    ));
    assert_eq!(b.placement(w), Some(p3.id));
    // And through the adapter the same shows as the wire's policy code.
    b.arm_all_profile(p3.id, TransportMode::P2p);
    let frontier = decode(&b.adapter.root_unit_version(&peer, unit_of(w)).unwrap());
    let bytes = a
        .adapter
        .root_unit_updates_since(&peer, unit_of(w), &frontier)
        .unwrap();
    let staged = b
        .store()
        .stage_root_unit(
            peer.relationship_id.as_uuid(),
            unit_of(w),
            std::slice::from_ref(&bytes),
        )
        .unwrap();
    b.store().move_workspace(w, p1.id).unwrap();
    let error = root_exchange::commit_root_unit(&mut b.store(), &peer, staged, None).unwrap_err();
    assert_eq!(error.code, POLICY_CHANGED);
}

/// §9 "Missing ancestors": a delta whose ancestors this device lacks is
/// `Pending`, nothing is applied, the receipt stays at `staged`, and the
/// re-request from the current (empty) frontier applies.
#[test]
fn a_frame_with_missing_ancestors_is_pending_and_the_re_request_applies() {
    let a = Device::new("A");
    let b = Device::new("B");
    let p1 = a.profile("P1", TransportMode::P2p);
    let p2 = a.profile("P2", TransportMode::P2p);
    let w = a.workspace(p1.id, "W", TransportMode::P2p);
    let peer = all_data_peer(relationship());
    assert!(b.adapter.bind_root_scope(&peer).unwrap());
    let first = decode(
        &a.adapter
            .root_catalog(&peer)
            .unwrap()
            .units
            .into_iter()
            .find(|unit| unit.unit == unit_of(w))
            .unwrap()
            .version,
    );
    a.store().move_workspace(w, p2.id).unwrap();
    // Only the move's operations: their ancestors (the unit's header and
    // first placement) are not on B.
    let delta = a
        .adapter
        .root_unit_updates_since(&peer, unit_of(w), &first)
        .unwrap();
    let outcome = b
        .adapter
        .apply_remote_root_updates(&peer, unit_of(w), vec![delta], None)
        .unwrap();
    assert!(matches!(outcome, RootJoin::Pending { .. }));
    assert!(
        b.store().root_placement(w).unwrap().is_none(),
        "nothing applied"
    );
    let receipt = b
        .store()
        .join_receipt(
            peer.relationship_id.as_uuid(),
            JoinUnit::WorkspaceCatalog { workspace: w },
        )
        .unwrap()
        .expect("a staged receipt");
    assert_eq!(receipt.phase, JoinPhase::Staged);
    assert!(receipt.frontier_vv().is_empty());
    assert_eq!(
        b.adapter
            .root_unit_version(&peer, unit_of(w))
            .unwrap_err()
            .code,
        ROOT_UNIT_NOT_FOUND,
        "the next request asks from the empty frontier"
    );

    let snapshot = a
        .adapter
        .root_unit_updates_since(&peer, unit_of(w), &VersionVector::new())
        .unwrap();
    let outcome = b
        .adapter
        .apply_remote_root_updates(&peer, unit_of(w), vec![snapshot], None)
        .unwrap();
    assert!(matches!(outcome, RootJoin::Applied { .. }));
    assert_eq!(b.placement(w), Some(p2.id));
    assert_eq!(
        b.store()
            .join_receipt(
                peer.relationship_id.as_uuid(),
                JoinUnit::WorkspaceCatalog { workspace: w }
            )
            .unwrap()
            .unwrap()
            .phase,
        JoinPhase::Reconciled
    );
}

/// Duplicate and crossing joins: the same frontier twice rewrites nothing
/// (the unit's revision is unchanged), and an older frontier arriving after
/// a newer one never regresses the receipt or the unit.
#[test]
fn duplicate_and_crossing_joins_are_idempotent_and_never_regress_the_receipt() {
    let a = Device::new("A");
    let b = Device::new("B");
    let p1 = a.profile("P1", TransportMode::P2p);
    let p2 = a.profile("P2", TransportMode::P2p);
    let w = a.workspace(p1.id, "W", TransportMode::P2p);
    let peer = all_data_peer(relationship());
    assert!(b.adapter.bind_root_scope(&peer).unwrap());
    let relationship = peer.relationship_id.as_uuid();
    let receipt_unit = JoinUnit::WorkspaceCatalog { workspace: w };

    let older = a
        .adapter
        .root_unit_updates_since(&peer, unit_of(w), &VersionVector::new())
        .unwrap();
    a.store().move_workspace(w, p2.id).unwrap();
    let newer = a
        .adapter
        .root_unit_updates_since(&peer, unit_of(w), &VersionVector::new())
        .unwrap();
    let newer_version = decode(
        &a.adapter
            .root_catalog(&peer)
            .unwrap()
            .units
            .into_iter()
            .find(|unit| unit.unit == unit_of(w))
            .unwrap()
            .version,
    );

    // Crossing: the newer snapshot lands first.
    b.adapter
        .apply_remote_root_updates(&peer, unit_of(w), vec![newer.clone()], Some(&newer_version))
        .unwrap();
    assert_eq!(b.placement(w), Some(p2.id));
    let receipt = |device: &Device| {
        device
            .store()
            .join_receipt(relationship, receipt_unit)
            .unwrap()
            .unwrap()
    };
    let after_newer = receipt(&b);
    assert_eq!(after_newer.phase, JoinPhase::Reconciled);

    // The older snapshot after it: nothing regresses.
    let outcome = b
        .adapter
        .apply_remote_root_updates(&peer, unit_of(w), vec![older], None)
        .unwrap();
    assert!(matches!(outcome, RootJoin::Applied { .. }));
    assert_eq!(b.placement(w), Some(p2.id));
    let after_older = receipt(&b);
    assert_eq!(
        after_older.frontier, after_newer.frontier,
        "the frontier never regresses"
    );
    assert_eq!(after_older.phase, JoinPhase::Reconciled);

    // Duplicate: the same frontier again is a no-op on the unit.
    let version_before = b.adapter.root_unit_version(&peer, unit_of(w)).unwrap();
    b.adapter
        .apply_remote_root_updates(&peer, unit_of(w), vec![newer], Some(&newer_version))
        .unwrap();
    assert_eq!(
        b.adapter.root_unit_version(&peer, unit_of(w)).unwrap(),
        version_before
    );
    assert_eq!(receipt(&b).frontier, after_newer.frontier);
}

// ----------------------------------------------------- three devices

/// Three devices, every delivery order: A moves W while C trashes it; B
/// receives the two in both orders, and all three agree on one placement
/// and one lifecycle (Loro's own ordering through the units, no clock).
#[test]
fn three_devices_converge_on_one_placement_and_one_lifecycle_in_every_delivery_order() {
    let a = Device::new("A");
    let c = Device::new("C");
    let p1 = a.profile("P1", TransportMode::P2p);
    let p2 = a.profile("P2", TransportMode::P2p);
    let w = a.workspace(p1.id, "W", TransportMode::P2p);
    let ac = all_data_peer(relationship());
    sync(&a, &c, &ac);
    assert_eq!(c.placement(w), Some(p1.id));

    // Concurrent: A moves W to P2, C trashes W.
    a.store().move_workspace(w, p2.id).unwrap();
    c.store().delete_workspace(w, later(10)).unwrap();

    let orders: [[char; 2]; 2] = [['a', 'c'], ['c', 'a']];
    let mut placements = BTreeSet::new();
    let mut lifecycles = BTreeSet::new();
    for order in orders {
        let b = Device::new("B");
        let ab = all_data_peer(relationship());
        let cb = all_data_peer(relationship());
        for device in order {
            match device {
                'a' => sync(&a, &b, &ab),
                _ => sync(&c, &b, &cb),
            }
        }
        placements.insert(b.placement(w));
        lifecycles.insert(b.deleted_at(w).is_some());
        assert_eq!(
            b.owner_row(w),
            b.placement(w).unwrap(),
            "the row follows the root"
        );
    }
    assert_eq!(placements.len(), 1, "one placement whatever the order");
    assert_eq!(
        lifecycles,
        [true].into_iter().collect(),
        "an active deletion wins visibility"
    );
    assert_eq!(placements.into_iter().next().unwrap(), Some(p2.id));

    // A and C converge on each other too.
    sync(&c, &a, &ac);
    sync(&a, &c, &ac);
    assert_eq!(a.placement(w), Some(p2.id));
    assert_eq!(c.placement(w), Some(p2.id));
    assert!(a.deleted_at(w).is_some());
    assert!(c.deleted_at(w).is_some());
}

// ------------------------------------------------------------ personal

/// Personal v2 units flow only to the same person's device: a scoped peer
/// is framed none and may not ask; the all-data peer gets the content.
#[test]
fn personal_v2_units_flow_only_to_the_same_persons_device() {
    let a = Device::new("A");
    let b = Device::new("B");
    let p = a.profile("P", TransportMode::P2p);
    let w = a.workspace(p.id, "W", TransportMode::P2p);
    let entity = EntityRef::new("task", w, DocumentId::new(), uuid::Uuid::new_v4()).unwrap();
    a.store()
        .set_personal_attribute(
            p.id,
            &entity,
            "mood",
            Some(&PersonalAttributeValue::Text {
                text: "SENTINEL-PERSONAL".into(),
            }),
        )
        .unwrap();
    let same_person = all_data_peer(relationship());
    let by_uuid = peer_with(relationship(), false, &[], &[w]);
    let by_profile = peer_with(relationship(), false, &[p.id], &[]);

    assert_eq!(
        a.adapter.root_catalog(&same_person).unwrap().personal.len(),
        1
    );
    for scoped in [&by_uuid, &by_profile] {
        assert!(a.adapter.root_catalog(scoped).unwrap().personal.is_empty());
        assert_eq!(
            a.adapter
                .personal_updates_since(scoped, w, &VersionVector::new())
                .unwrap_err()
                .code,
            ROOT_SCOPE_NOT_BOUND
        );
    }

    sync(&a, &b, &same_person);
    let attributes = b.store().personal_attributes(p.id, &entity).unwrap();
    assert_eq!(
        attributes.get("mood"),
        Some(&PersonalAttributeValue::Text {
            text: "SENTINEL-PERSONAL".into()
        })
    );
    let receipt = b
        .store()
        .join_receipt(
            same_person.relationship_id.as_uuid(),
            JoinUnit::PersonalV2 { workspace: w },
        )
        .unwrap()
        .expect("a personal receipt");
    assert_eq!(receipt.phase, JoinPhase::Reconciled);
}

/// Redacted (all-data, with a hidden historical placement nulled) and full
/// replicas keep converging on current state in both directions after new
/// placements, with no raw-byte fallback and no second lineage.
#[test]
fn redacted_and_full_replicas_keep_converging_after_new_placements() {
    let a = Device::new("A");
    let b = Device::new("B");
    let hidden = a.profile("Hidden", TransportMode::P2p);
    let v1 = a.profile("V1", TransportMode::P2p);
    let v2 = a.profile("V2", TransportMode::P2p);
    let w = a.workspace(hidden.id, "W", TransportMode::P2p);
    a.store().move_workspace(w, v1.id).unwrap();
    a.arm_all_profile(hidden.id, TransportMode::LocalOnly);
    let peer = all_data_peer(relationship());

    sync(&a, &b, &peer);
    assert_eq!(b.placement(w), Some(v1.id));
    assert_eq!(
        b.store().profiles().unwrap().len(),
        2,
        "the hidden Profile never arrived"
    );

    // A full replica moves again; the redacted one follows.
    a.store().move_workspace(w, v2.id).unwrap();
    for (_, outcome) in exchange_root(&a, &b, &peer) {
        outcome.unwrap();
    }
    assert_eq!(b.placement(w), Some(v2.id));

    // The redacted replica moves; the full one follows its new operation
    // without re-learning or losing its own hidden history.
    assert!(a.adapter.bind_root_scope(&peer).unwrap());
    b.store().move_workspace(w, v1.id).unwrap();
    for (_, outcome) in exchange_root(&b, &a, &peer) {
        outcome.unwrap();
    }
    assert_eq!(a.placement(w), Some(v1.id));
    assert_eq!(a.owner_row(w), v1.id);
    let a_version = decode(&a.adapter.root_unit_version(&peer, unit_of(w)).unwrap());
    let b_version = decode(&b.adapter.root_unit_version(&peer, unit_of(w)).unwrap());
    assert_eq!(a_version, b_version, "one lineage, both frontiers equal");
}
