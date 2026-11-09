// SPDX-License-Identifier: MPL-2.0

//! The Store end of the Agent-owned replication control session.

use std::sync::{Arc, Mutex};

use koru_sync_control::{
    CONTROL_FRAME_VERSION, CONTROL_PROTOCOL_VERSION, CONTROL_SERVICE, ControlGreeting,
    ControlHello, ControlHost, RequestFrame, ResponseFrame,
};

#[derive(Clone)]
pub(crate) enum ReplicationRuntime {
    Iroh(koru_network_iroh::RuntimeHandle),
    Session(koru_network_iroh::SessionRuntimeHandle),
}

impl From<koru_network_iroh::RuntimeHandle> for ReplicationRuntime {
    fn from(runtime: koru_network_iroh::RuntimeHandle) -> Self {
        Self::Iroh(runtime)
    }
}

impl From<koru_network_iroh::SessionRuntimeHandle> for ReplicationRuntime {
    fn from(runtime: koru_network_iroh::SessionRuntimeHandle) -> Self {
        Self::Session(runtime)
    }
}

impl ReplicationRuntime {
    /// §8's peer-first read, forwarded to whichever runtime this process has.
    /// `None` is the fall-through to Cloud, not a failure.
    pub(crate) async fn read_blob_range(
        &self,
        workspace: koru_store_core::WorkspaceId,
        hash: koru_store_core::model::BlobHash,
        offset: u64,
        max: u64,
    ) -> Option<koru_network_iroh::PeerBlobRange> {
        match self {
            Self::Iroh(runtime) => runtime.read_blob_range(workspace, hash, offset, max).await,
            Self::Session(runtime) => runtime.read_blob_range(workspace, hash, offset, max).await,
        }
    }

    fn retain(&self) {
        if let Self::Iroh(runtime) = self {
            runtime.retain();
        }
    }

    fn release(&self, grace: std::time::Duration) {
        if let Self::Iroh(runtime) = self {
            runtime.release(grace);
        }
    }

    pub(crate) fn clear(&self) {
        if let Self::Iroh(runtime) = self {
            runtime.clear();
        }
    }

    fn replace_authorization(&self, snapshot: koru_sync_control::AuthorizationSnapshot) {
        match self {
            Self::Iroh(runtime) => runtime.replace_authorization(snapshot),
            Self::Session(runtime) => runtime.replace_authorization(snapshot),
        }
    }

    fn complete_trust_record(
        &self,
        delivery_id: koru_sync_control::TrustDeliveryId,
        accepted: bool,
    ) {
        match self {
            Self::Iroh(runtime) => runtime.complete_trust_record(delivery_id, accepted),
            Self::Session(runtime) => runtime.complete_trust_record(delivery_id, accepted),
        }
    }

    fn take_pairing_events(&self) -> Vec<koru_sync_control::PairingEvent> {
        match self {
            Self::Iroh(runtime) => runtime.take_pairing_events(),
            Self::Session(runtime) => runtime.take_pairing_events(),
        }
    }

    fn restore_pairing_events(&self, events: Vec<koru_sync_control::PairingEvent>) {
        match self {
            Self::Iroh(runtime) => runtime.restore_pairing_events(events),
            Self::Session(runtime) => runtime.restore_pairing_events(events),
        }
    }

    fn state_change_page(
        &self,
        peer: koru_device::DeviceId,
        before: Option<i64>,
    ) -> Result<koru_network_iroh::StateChangePage, koru_replication::protocol::StoreFailure> {
        match self {
            Self::Iroh(runtime) => runtime.state_change_page(peer, before),
            Self::Session(runtime) => runtime.state_change_page(peer, before),
        }
    }

    fn status(&self) -> koru_network_iroh::NetworkStatus {
        match self {
            Self::Iroh(runtime) => runtime.status(),
            Self::Session(runtime) => runtime.status(),
        }
    }
}

pub fn start(
    root: &std::path::Path,
    channel: &str,
    host: Arc<Mutex<ControlHost>>,
    replication: impl Into<ReplicationRuntime>,
    grace: std::time::Duration,
    store: crate::StoreWorkerClient,
) -> Result<(), String> {
    let replication = replication.into();
    let endpoint = koru_ipc::Endpoint::for_service_root(CONTROL_SERVICE, channel, root)
        .map_err(|error| error.to_string())?;
    let listener = koru_ipc::Listener::bind(endpoint).map_err(|error| error.to_string())?;
    std::thread::Builder::new()
        .name("koru-store-server sync control".into())
        .spawn(move || serve(listener, host, replication, grace, store))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn serve(
    listener: koru_ipc::Listener,
    host: Arc<Mutex<ControlHost>>,
    replication: ReplicationRuntime,
    grace: std::time::Duration,
    store: crate::StoreWorkerClient,
) {
    loop {
        let Ok(mut stream) = listener.accept() else {
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        };
        if !koru_ipc::same_user(&stream) {
            continue;
        }
        let Ok((frame_version, body)) = koru_ipc::frame::read_raw(&mut stream) else {
            continue;
        };
        if frame_version != CONTROL_FRAME_VERSION {
            continue;
        }
        let Ok(hello) = koru_ipc::frame::decode::<ControlHello>(&body) else {
            continue;
        };
        let greeting = if hello.protocol_version == CONTROL_PROTOCOL_VERSION {
            ControlGreeting::Welcome {
                protocol_version: CONTROL_PROTOCOL_VERSION,
            }
        } else {
            ControlGreeting::Refused {
                ours: CONTROL_PROTOCOL_VERSION,
                theirs: hello.protocol_version,
                code: "protocol".into(),
            }
        };
        if koru_ipc::frame::write_frame(&mut stream, CONTROL_FRAME_VERSION, &greeting).is_err() {
            continue;
        }
        if !matches!(greeting, ControlGreeting::Welcome { .. }) {
            continue;
        }
        if host
            .lock()
            .map_err(|_| ())
            .and_then(|mut state| state.begin_session(&hello).map_err(|_| ()))
            .is_err()
        {
            continue;
        }
        // **Reclaim, do not rebuild.** This used to clear — closing every peer
        // and dropping the iroh endpoint — so the reconnecting agent's first
        // authorization had to bind an endpoint and redial the whole mesh, for
        // a grant that had not moved. `retain` cancels the release the last
        // session scheduled; if that release already fired, provisioning and
        // authorizing rebuild exactly what a clear would have forced.
        replication.retain();

        loop {
            let Ok((version, body)) = koru_ipc::frame::read_raw(&mut stream) else {
                break;
            };
            if version != CONTROL_FRAME_VERSION {
                break;
            }
            let Ok(request) = koru_ipc::frame::decode::<RequestFrame>(&body) else {
                break;
            };
            let runtime_action = RuntimeAction::from_request(&request.request);
            let request_kind = request_kind_name(&request.request);
            // **The seven Cloud variants carry a second, tighter cap.** The
            // existing frame cap bounds the socket; this one bounds a Cloud
            // body, so an over-large permit page is refused as a message
            // rather than answered. The already-decoded body is measured, not
            // a re-encoding of it.
            let mut response = if koru_sync_control::is_cloud_request(&request.request)
                && body.len() > koru_sync_control::cloud::MAX_CLOUD_CONTROL_FRAME_BYTES
            {
                tracing::warn!(
                    request = %request_kind,
                    bytes = body.len(),
                    "sync control: a Cloud frame is over its 64 KiB cap"
                );
                koru_sync_control::ControlResponse::Rejected {
                    code: "cloud_frame_too_large".into(),
                }
            } else {
                match host.lock() {
                    Ok(mut state) => state.handle(hello.session_id, request.request),
                    Err(_) => break,
                }
            };
            if !matches!(
                response,
                koru_sync_control::ControlResponse::Rejected { .. }
            ) {
                response = runtime_action.apply(&replication, &store, response);
                // **Nothing is promoted here any more** (D258). B8 raised this
                // device's seeded `local_only` rows from the account, pairing
                // and selection evidence these very requests carry; on
                // 2026-09-20 the maintainers withdrew that and made a person's
                // explicit choice the only promotion path, so migration 12's
                // rows stay exactly as they are until somebody answers the
                // Profile prompt. The group commit B8 built survives — its
                // trigger is now `Store::choose_transport_mode`, reached from
                // the Manage tab through the local Store route.
            }
            let response = ResponseFrame {
                id: request.id,
                response,
            };
            if let Err(error) =
                koru_ipc::frame::write_frame(&mut stream, CONTROL_FRAME_VERSION, &response)
            {
                // Said out loud: a session that ends here is an agent that
                // will reconnect and replace the whole authorization. That
                // used to close every peer; it no longer does — an unchanged
                // grant is a no-op and the runtime is only released, not
                // cleared — but the *reply* is still gone, which is the part
                // below. A reply too large for a frame was this exact silence
                // for a day.
                tracing::warn!(
                    request = %request_kind,
                    error = %error,
                    "sync control: could not answer; the control session ends"
                );
                // **A reply that never left took its payload with it.**
                // `take_pairing_events` drains the queue when the response is
                // built, so an unsent reply is not a delayed delivery but a
                // lost one — which is what made the oversized batch a silent
                // failure rather than a retry. Recovery happened to work only
                // because the reconnect closed every peer and made them gossip
                // again; putting them back is the part that does not depend on
                // that.
                if let koru_sync_control::ControlResponse::PairingEvents { events } =
                    response.response
                {
                    replication.restore_pairing_events(events);
                }
                break;
            }
        }
        if let Ok(mut state) = host.lock() {
            state.end_session(hello.session_id);
        }
        // **The host clears; the data plane is only released.** Host
        // bookkeeping is what a status surface reads, and with no session
        // there is honestly none — but the endpoint, the connections and the
        // grant survive `grace`, because an agent that stops for two seconds
        // and comes back is the ordinary case and tearing the mesh down for it
        // was the churn this fixes. Nothing reclaims it inside the window and
        // the full clear runs after all.
        replication.release(grace);
    }
}

enum RuntimeAction {
    Provision([u8; 32]),
    Authorize(koru_sync_control::AuthorizationSnapshot),
    Connect(koru_device::DeviceId),
    Disconnect(koru_device::DeviceId),
    BeginPairing(Box<koru_pairing::PairingInvitationV1>),
    CancelPairing,
    JoinPairing(Box<koru_pairing::PairingJoinV1>),
    CompletePairing {
        attempt_id: koru_sync_control::PairingAttemptId,
        prepared: Box<koru_pairing::PreparedPairingV1>,
    },
    SendTrustRecord {
        peer: koru_device::DeviceId,
        record: Box<koru_device::TrustRecord>,
    },
    SendTrustRecords {
        peer: koru_device::DeviceId,
        records: Vec<koru_device::TrustRecord>,
    },
    CompleteTrustRecord {
        delivery_id: koru_sync_control::TrustDeliveryId,
        accepted: bool,
    },
    PairingEvents,
    StateHistory {
        peer: koru_device::DeviceId,
        before: Option<i64>,
    },
    Status,
    /// **The Cloud operations the `ControlHost` answers on its own.** It
    /// validates the session, the sequence space, the permits and the
    /// principal, and this daemon passes that answer through unchanged
    /// rather than inventing a second one. The content driver reads the
    /// installed context from the same host at attachment
    /// (`daemon::cloud_driver`); the consent and network-policy operations
    /// keep the host's safe placeholder answers until the consent surface
    /// lands.
    CloudPlaceholder,
    /// HOST-CONTROL §3.1 `read_cloud_bootstrap`: the host validated the
    /// session and the shape; the daemon reads the durable manifest identity
    /// through the Store worker, the only valid local source.
    ReadCloudBootstrap {
        workspace: koru_store_core::WorkspaceId,
    },
}

/// The request's kind for a log line, and never its contents: a gossip batch
/// is a page of signed records and a snapshot names every workspace.
///
/// **Exhaustive on purpose.** A wildcard arm here logged nine of the thirteen
/// kinds as `other` — including `SendTrustRecords`, which carries a batch and
/// is the likeliest thing on this socket to be too large to answer. The whole
/// value of this line is naming what could not be written, so a new request
/// variant should not be able to arrive as `other`: without the arm it is a
/// compile error, and with one it is a log nobody can act on.
fn request_kind_name(request: &koru_sync_control::ControlRequest) -> &'static str {
    use koru_sync_control::ControlRequest as R;
    match request {
        R::ProvisionEndpoint { .. } => "provision_endpoint",
        R::ReplaceAuthorization { .. } => "replace_authorization",
        R::ConnectPeer { .. } => "connect_peer",
        R::DisconnectPeer { .. } => "disconnect_peer",
        R::BeginPairing { .. } => "begin_pairing",
        R::CancelPairing => "cancel_pairing",
        R::JoinPairing { .. } => "join_pairing",
        R::CompletePairing { .. } => "complete_pairing",
        R::SendTrustRecord { .. } => "send_trust_record",
        R::SendTrustRecords { .. } => "send_trust_records",
        R::CompleteTrustRecord { .. } => "complete_trust_record",
        R::PairingEvents => "pairing_events",
        R::StateHistory { .. } => "state_history",
        R::Status => "status",
        R::ReplaceCloudContext { .. } => "replace_cloud_context",
        R::InstallCloudSession { .. } => "install_cloud_session",
        R::FenceCloud { .. } => "fence_cloud",
        R::CloudStatus { .. } => "cloud_status",
        R::DecideCloudTransfer { .. } => "decide_cloud_transfer",
        R::ReplaceCloudNetworkPolicy { .. } => "replace_cloud_network_policy",
        R::ReplaceCloudBlobAccess { .. } => "replace_cloud_blob_access",
        R::ReadCloudBootstrap { .. } => "read_cloud_bootstrap",
    }
}

impl RuntimeAction {
    fn from_request(request: &koru_sync_control::ControlRequest) -> Self {
        use koru_sync_control::ControlRequest;
        match request {
            ControlRequest::ProvisionEndpoint { secret } => Self::Provision(*secret.expose()),
            ControlRequest::ReplaceAuthorization { snapshot } => Self::Authorize(snapshot.clone()),
            ControlRequest::ConnectPeer { peer } => Self::Connect(*peer),
            ControlRequest::DisconnectPeer { peer } => Self::Disconnect(*peer),
            ControlRequest::BeginPairing { invitation } => Self::BeginPairing(invitation.clone()),
            ControlRequest::CancelPairing => Self::CancelPairing,
            ControlRequest::JoinPairing { join } => Self::JoinPairing(join.clone()),
            ControlRequest::CompletePairing {
                attempt_id,
                prepared,
            } => Self::CompletePairing {
                attempt_id: *attempt_id,
                prepared: prepared.clone(),
            },
            ControlRequest::SendTrustRecord { peer, record } => Self::SendTrustRecord {
                peer: *peer,
                record: record.clone(),
            },
            ControlRequest::SendTrustRecords { peer, records } => Self::SendTrustRecords {
                peer: *peer,
                records: records.clone(),
            },
            ControlRequest::CompleteTrustRecord {
                delivery_id,
                accepted,
            } => Self::CompleteTrustRecord {
                delivery_id: *delivery_id,
                accepted: *accepted,
            },
            ControlRequest::PairingEvents => Self::PairingEvents,
            ControlRequest::StateHistory { peer, before } => Self::StateHistory {
                peer: *peer,
                before: *before,
            },
            ControlRequest::Status => Self::Status,
            ControlRequest::ReadCloudBootstrap {
                dataset: koru_device::cloud::CloudReplicaPrincipalV1::Workspace { workspace_id, .. },
                ..
            } => Self::ReadCloudBootstrap {
                workspace: koru_store_core::WorkspaceId::from_uuid(workspace_id.as_uuid()),
            },
            ControlRequest::ReplaceCloudContext { .. }
            | ControlRequest::InstallCloudSession { .. }
            | ControlRequest::FenceCloud { .. }
            | ControlRequest::CloudStatus { .. }
            | ControlRequest::DecideCloudTransfer { .. }
            | ControlRequest::ReplaceCloudNetworkPolicy { .. }
            | ControlRequest::ReplaceCloudBlobAccess { .. }
            | ControlRequest::ReadCloudBootstrap { .. } => Self::CloudPlaceholder,
        }
    }

    fn apply(
        self,
        runtime: &ReplicationRuntime,
        store: &crate::StoreWorkerClient,
        response: koru_sync_control::ControlResponse,
    ) -> koru_sync_control::ControlResponse {
        match self {
            Self::ReadCloudBootstrap { workspace } => {
                let koru_sync_control::ControlResponse::CloudBootstrap {
                    control_session_id, ..
                } = response
                else {
                    return response;
                };
                return match read_cloud_bootstrap(store, workspace) {
                    Ok(manifest_document_id) => {
                        koru_sync_control::ControlResponse::CloudBootstrap {
                            control_session_id,
                            manifest_document_id,
                        }
                    }
                    Err(code) => koru_sync_control::ControlResponse::Rejected { code: code.into() },
                };
            }
            Self::Provision(secret) => {
                if let ReplicationRuntime::Iroh(runtime) = runtime {
                    runtime.provision_endpoint(secret);
                }
            }
            Self::Authorize(snapshot) => runtime.replace_authorization(snapshot),
            Self::Connect(peer) => {
                if let ReplicationRuntime::Iroh(runtime) = runtime {
                    runtime.connect_peer(peer);
                }
            }
            Self::Disconnect(peer) => {
                if let ReplicationRuntime::Iroh(runtime) = runtime {
                    runtime.disconnect_peer(peer);
                }
            }
            Self::BeginPairing(invitation) => {
                if let ReplicationRuntime::Iroh(runtime) = runtime {
                    runtime.begin_pairing(*invitation);
                }
            }
            Self::CancelPairing => {
                if let ReplicationRuntime::Iroh(runtime) = runtime {
                    runtime.cancel_pairing();
                }
            }
            Self::JoinPairing(join) => {
                if let ReplicationRuntime::Iroh(runtime) = runtime {
                    runtime.join_pairing(*join);
                }
            }
            Self::CompletePairing {
                attempt_id,
                prepared,
            } => {
                if let ReplicationRuntime::Iroh(runtime) = runtime {
                    runtime.complete_pairing(attempt_id, *prepared);
                }
            }
            Self::SendTrustRecord { peer, record } => {
                if let ReplicationRuntime::Iroh(runtime) = runtime {
                    runtime.send_trust_record(peer, *record);
                }
            }
            Self::SendTrustRecords { peer, records } => {
                // The Agent marks a batch as gossiped only when it was really
                // queued: a snapshot replacement can close this peer's
                // connection in the same instant, and an unmarked batch is
                // retried on the next tick rather than lost for minutes.
                if let ReplicationRuntime::Iroh(runtime) = runtime {
                    let delivered = runtime.send_trust_records(peer, records);
                    if delivered.recv_timeout(std::time::Duration::from_secs(2)) != Ok(true) {
                        return koru_sync_control::ControlResponse::Rejected {
                            code: "peer_not_connected".into(),
                        };
                    }
                }
            }
            Self::CompleteTrustRecord {
                delivery_id,
                accepted,
            } => runtime.complete_trust_record(delivery_id, accepted),
            Self::PairingEvents => {
                return koru_sync_control::ControlResponse::PairingEvents {
                    events: runtime.take_pairing_events(),
                };
            }
            Self::StateHistory { peer, before } => {
                return match runtime.state_change_page(peer, before) {
                    Ok(page) => koru_sync_control::ControlResponse::StateHistory {
                        peer,
                        events: page.events.into_iter().map(control_state_event).collect(),
                        next_before: page.next_before,
                    },
                    Err(error) => koru_sync_control::ControlResponse::Rejected {
                        code: error.code.into(),
                    },
                };
            }
            Self::Status => {
                let koru_sync_control::ControlResponse::Status(mut status) = response else {
                    return response;
                };
                let network = runtime.status();
                status.peers = network
                    .peers
                    .into_iter()
                    .map(|(device_id, state)| koru_sync_control::PeerStatus {
                        device_id,
                        state: peer_state(state),
                        last_seen_millis: network.last_seen_millis.get(&device_id).copied(),
                        last_state_change_millis: network
                            .last_state_change_millis
                            .get(&device_id)
                            .copied(),
                    })
                    .collect();
                status.sync_event =
                    network
                        .last_sync_event
                        .map(|event| koru_sync_control::SyncEvent {
                            sequence: event.sequence,
                            at_millis: event.at_millis,
                            peer: event.peer,
                            stage: event.stage.into(),
                            workspace: event.workspace,
                            items: event.items,
                            error_code: event.error_code,
                        });
                status.root_sync = network.root_sync.into_values().flatten().collect();
                return koru_sync_control::ControlResponse::Status(status);
            }
            // Deliberately no runtime call: the host already answered.
            Self::CloudPlaceholder => {}
        }
        response
    }
}

/// The originating device's own manifest identity (HOST-CONTROL §3.1). A
/// Workspace this Store does not hold is `None`, which the Agent reads as a
/// fresh device; a Workspace it holds answers the manifest the catalog
/// records, including a soft-deleted one, because the identity survives
/// deletion. A Store that cannot answer is an error, never a null: "errors
/// and timeouts are not null success".
fn read_cloud_bootstrap(
    store: &crate::StoreWorkerClient,
    workspace: koru_store_core::WorkspaceId,
) -> Result<Option<koru_device::cloud::CloudId>, &'static str> {
    store
        .with_store(
            move |store| -> Result<Option<koru_store_core::DocumentId>, &'static str> {
                match store.workspace_including_deleted(workspace) {
                    Ok(_) => store
                        .replica_manifest_id_including_deleted(workspace)
                        .map(Some)
                        .map_err(|_| "manifest_unavailable"),
                    Err(error)
                        if error.code == koru_store_core::errors::Code::WorkspaceNotFound =>
                    {
                        Ok(None)
                    }
                    Err(_) => Err("store_unavailable"),
                }
            },
        )
        .map_err(|_| "store_worker_unavailable")?
        .map(|manifest| {
            manifest.map(|id| koru_device::cloud::CloudId::from_bytes(*id.as_uuid().as_bytes()))
        })
}

fn control_state_event(
    event: koru_replication::StateChangeEvent,
) -> koru_sync_control::StateChangeEvent {
    koru_sync_control::StateChangeEvent {
        event_id: event.event_id,
        sequence: event.sequence,
        at_millis: event.at_millis,
        peer: event.peer,
        kind: event.kind,
        profile: event.profile,
        workspace: event.workspace,
        document: event.document,
        document_name: event.document_name,
        blob_hash: event.blob_hash,
        detail: event.detail,
        items: event.items,
    }
}

fn peer_state(state: koru_network_iroh::PeerConnectionState) -> koru_sync_control::PeerState {
    use koru_network_iroh::{PathKind, PeerConnectionState};
    use koru_sync_control::PeerState;
    match state {
        PeerConnectionState::Disconnected => PeerState::Disconnected,
        PeerConnectionState::Resolving | PeerConnectionState::Connecting => PeerState::Connecting,
        PeerConnectionState::Connected {
            path: PathKind::Direct,
        } => PeerState::ConnectedDirect,
        PeerConnectionState::Connected {
            path: PathKind::Relay,
        } => PeerState::ConnectedRelay,
        PeerConnectionState::Connected {
            path: PathKind::Unknown,
        } => PeerState::ConnectedUnknown,
        PeerConnectionState::BackingOff => PeerState::BackingOff,
        PeerConnectionState::Unauthorized => PeerState::Unauthorized,
        PeerConnectionState::Failed { .. } => PeerState::Failed,
    }
}

#[cfg(test)]
mod tests {
    /// A queue nobody serves: the control tests never read the Store, and a
    /// Cloud bootstrap read against it answers `store_worker_unavailable`.
    fn test_store_client() -> crate::StoreWorkerClient {
        crate::daemon::test_store_client()
    }

    use super::*;
    use koru_sync_control::{
        ControlRequest, ControlResponse, ControlSessionId, EndpointSecretBytes, RequestFrame,
        ResponseFrame,
    };

    /// How long these tests wait for an iroh endpoint to bind or clear.
    ///
    /// A liveness deadline, not a budget: nothing here asserts how fast a bind
    /// is, only that it happens. It is generous on purpose because the thing
    /// it waits for is slow (TECHDEBT 88) and because a suite running eight
    /// binaries at once must not turn that slowness into a false failure.
    const BIND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

    #[tokio::test(flavor = "multi_thread")]
    async fn ipc_session_provisions_and_clears_volatile_authority() {
        let root = tempfile::tempdir().unwrap();
        let channel = koru_channel::segment().to_owned();
        let host = Arc::new(Mutex::new(ControlHost::default()));
        let replication = koru_network_iroh::RuntimeHandle::spawn(
            koru_network_iroh::IrohNetworkConfig::isolated(),
        );
        // Short, because this test is about the *host*, which clears
        // immediately either way — and a long grace would leave a live iroh
        // endpoint behind after the test returns.
        start(
            root.path(),
            &channel,
            Arc::clone(&host),
            replication.clone(),
            std::time::Duration::from_millis(200),
            test_store_client(),
        )
        .unwrap();
        let endpoint =
            koru_ipc::Endpoint::for_service_root(CONTROL_SERVICE, &channel, root.path()).unwrap();
        let mut stream = koru_ipc::connect(&endpoint).unwrap();
        let hello = ControlHello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            session_id: ControlSessionId::new(),
            local_device: koru_device::DeviceId::new(),
        };
        koru_ipc::frame::write_frame(&mut stream, CONTROL_FRAME_VERSION, &hello).unwrap();
        let (_, greeting) = koru_ipc::frame::read_frame::<_, ControlGreeting>(&mut stream).unwrap();
        assert!(matches!(greeting, ControlGreeting::Welcome { .. }));
        koru_ipc::frame::write_frame(
            &mut stream,
            CONTROL_FRAME_VERSION,
            &RequestFrame {
                id: 1,
                request: ControlRequest::ProvisionEndpoint {
                    secret: EndpointSecretBytes::new([7; 32]),
                },
            },
        )
        .unwrap();
        let (_, response) = koru_ipc::frame::read_frame::<_, ResponseFrame>(&mut stream).unwrap();
        assert_eq!(response.response, ControlResponse::Ok);
        assert!(host.lock().unwrap().status().endpoint_id.is_some());
        // Binding the iroh endpoint is real work with no fixed cost, so this
        // waits on a deadline rather than a count. The assert below is exact.
        //
        // **`BIND_DEADLINE` is sixty seconds because a bind here measures
        // fourteen**, on an idle Mac, repeatably (five runs, 17.2-17.4 s for
        // the three tests; 13.75 / 13.95 / 17.43 s individually). The comment
        // this replaces said 2.5 s on a *loaded* machine, so either the cost
        // moved or the platform did — and twenty seconds against a
        // fourteen-second job is a 30% margin that any parallelism spends.
        // These three failed together in three of five whole-workspace runs
        // and passed five of five alone. See TECHDEBT 88: the fourteen seconds
        // is the finding, and this deadline is only what stops it being
        // reported as a flake.
        let deadline = std::time::Instant::now() + BIND_DEADLINE;
        while replication.status().endpoint_id.is_none() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(
            replication.status().endpoint_id,
            host.lock().unwrap().status().endpoint_id
        );
        drop(stream);
        let deadline = std::time::Instant::now() + BIND_DEADLINE;
        while std::time::Instant::now() < deadline {
            if host.lock().unwrap().status().endpoint_id.is_none() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("control disconnect did not clear volatile authority");
    }

    /// Open a control session and provision the endpoint, answering the
    /// endpoint id the runtime settled on.
    async fn provisioned_session(
        endpoint: &koru_ipc::Endpoint,
        replication: &koru_network_iroh::RuntimeHandle,
        secret: [u8; 32],
    ) -> (koru_ipc::Stream, koru_device::ReplicationEndpointId) {
        let mut stream = koru_ipc::connect(endpoint).unwrap();
        let hello = ControlHello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            session_id: ControlSessionId::new(),
            local_device: koru_device::DeviceId::new(),
        };
        koru_ipc::frame::write_frame(&mut stream, CONTROL_FRAME_VERSION, &hello).unwrap();
        let (_, greeting) = koru_ipc::frame::read_frame::<_, ControlGreeting>(&mut stream).unwrap();
        assert!(matches!(greeting, ControlGreeting::Welcome { .. }));
        koru_ipc::frame::write_frame(
            &mut stream,
            CONTROL_FRAME_VERSION,
            &RequestFrame {
                id: 1,
                request: ControlRequest::ProvisionEndpoint {
                    secret: EndpointSecretBytes::new(secret),
                },
            },
        )
        .unwrap();
        let (_, response) = koru_ipc::frame::read_frame::<_, ResponseFrame>(&mut stream).unwrap();
        assert_eq!(response.response, ControlResponse::Ok);
        // Binding is real work with no fixed cost; wait on a deadline. See
        // `BIND_DEADLINE`.
        let deadline = std::time::Instant::now() + BIND_DEADLINE;
        while replication.status().endpoint_id.is_none() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let id = replication
            .status()
            .endpoint_id
            .expect("the endpoint should have bound");
        (stream, id)
    }

    /// **The churn this exists to stop.** An agent that drops and comes back
    /// inside the grace keeps the endpoint it had — no rebind, and therefore
    /// none of the peer closes and redials a rebind forces. Asserted past the
    /// original release deadline, so a `retain` that failed to cancel would
    /// show up as the endpoint disappearing while the session is live.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_that_reconnects_inside_the_grace_keeps_its_endpoint() {
        let root = tempfile::tempdir().unwrap();
        let channel = koru_channel::segment().to_owned();
        let host = Arc::new(Mutex::new(ControlHost::default()));
        let replication = koru_network_iroh::RuntimeHandle::spawn(
            koru_network_iroh::IrohNetworkConfig::isolated(),
        );
        let grace = std::time::Duration::from_secs(3);
        start(
            root.path(),
            &channel,
            Arc::clone(&host),
            replication.clone(),
            grace,
            test_store_client(),
        )
        .unwrap();
        let endpoint =
            koru_ipc::Endpoint::for_service_root(CONTROL_SERVICE, &channel, root.path()).unwrap();

        let (stream, first) = provisioned_session(&endpoint, &replication, [7; 32]).await;
        drop(stream);
        // Back before the grace expires, with the same secret — which is what
        // an agent restart looks like.
        let (stream, second) = provisioned_session(&endpoint, &replication, [7; 32]).await;
        assert_eq!(first, second, "the endpoint was rebound across a reconnect");

        // Past the deadline the first release set: if `retain` had not
        // cancelled it, the clear would land here, under a live session.
        tokio::time::sleep(grace + std::time::Duration::from_millis(500)).await;
        assert_eq!(
            replication.status().endpoint_id,
            Some(first),
            "a released runtime was cleared despite being reclaimed"
        );
        drop(stream);
        replication.clear();
    }

    /// The other half: nothing reclaims it, so the grant really does die —
    /// the volatile authority stays volatile, just not instantly.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_that_does_not_come_back_clears_after_the_grace() {
        let root = tempfile::tempdir().unwrap();
        let channel = koru_channel::segment().to_owned();
        let host = Arc::new(Mutex::new(ControlHost::default()));
        let replication = koru_network_iroh::RuntimeHandle::spawn(
            koru_network_iroh::IrohNetworkConfig::isolated(),
        );
        start(
            root.path(),
            &channel,
            Arc::clone(&host),
            replication.clone(),
            std::time::Duration::from_millis(300),
            test_store_client(),
        )
        .unwrap();
        let endpoint =
            koru_ipc::Endpoint::for_service_root(CONTROL_SERVICE, &channel, root.path()).unwrap();

        let (stream, _) = provisioned_session(&endpoint, &replication, [9; 32]).await;
        drop(stream);
        let deadline = std::time::Instant::now() + BIND_DEADLINE;
        while std::time::Instant::now() < deadline {
            if replication.status().endpoint_id.is_none() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the runtime never cleared after its grace expired");
    }
}
