// SPDX-License-Identifier: MPL-2.0
//! The removal list is consumed before content enumeration, including when
//! ordinary uploads are paused. Never acknowledge by document identity.
use super::*;
use koru_replication::cloud::transfer::{Control, removal};

pub(super) async fn reconcile<'a, R, W, J>(
    sync: &mut Reconciler<'a, R, W>,
    scope: Scope,
    worker: &J,
    authorize: &Authorize,
    progress: &mut impl FnMut(Progress),
) -> Result<(), Error>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
    J: StoreJobs,
{
    let started = std::time::Instant::now();
    let koru_device::cloud::CloudReplicaPrincipalV1::Workspace { incarnation_id, .. } =
        scope.principal
    else {
        return Ok(());
    };
    let incarnation = uuid::Uuid::from_bytes(*incarnation_id.as_bytes());
    let workspace = scope.eligibility_workspace;
    let anchor = Unit::Document {
        workspace_id: cloud(workspace.as_uuid()),
        document_id: cloud(scope.manifest.as_uuid()),
    };
    let mut cursor =
        store_result(worker.submit_store(move |store| {
            client_store::removal::cursor(store, workspace, incarnation)
        }))
        .await?
        .map_err(map_failure)?;
    // Read first: two offline deletions must each retain their own event even
    // when this device first learns that Cloud already deleted the document.
    loop {
        authorize(anchor).map_err(Error::Authority)?;
        let answer = sync.removal_exchange(removal::read(cursor)).await.map_err(|error| {
            tracing::debug!(target: "cloud.document.removal", stage = "refused", code = error.code(), age_ms = started.elapsed().as_millis() as u64, age_scope = "pass");
            error
        })?;
        let Control::CloudRemovals {
            events,
            next_cursor,
            ..
        } = &answer.control
        else {
            return Err(Error::Invalid);
        };
        if *next_cursor < cursor || (!events.is_empty() && *next_cursor == cursor) {
            return Err(Error::Invalid);
        }
        let done = events.is_empty();
        if done && *next_cursor != cursor {
            return Err(Error::Invalid);
        }
        let page = events.clone();
        let next = *next_cursor;
        let gate = authorize.clone();
        store_result(worker.submit_store(move |store| {
            gate(anchor).map_err(|r| client_store::Failure::Transfer(Error::Authority(r)))?;
            client_store::removal::consume(store, scope, &page, next, Timestamp::now())
        }))
        .await?
        .map_err(map_failure)?;
        if !events.is_empty() {
            tracing::debug!(target: "cloud.document.removal", stage = "event", code = "received_durable", count = events.len(), age_ms = started.elapsed().as_millis() as u64, age_scope = "pass");
        }
        cursor = next;
        if done {
            break;
        }
    }
    let pending =
        store_result(worker.submit_store(move |store| {
            client_store::removal::pending(store, workspace, incarnation)
        }))
        .await?
        .map_err(map_failure)?;
    let mut unanswered = pending.len().min(u32::MAX as usize) as u32;
    progress(Progress::Removing {
        pending: unanswered,
    });
    for event in pending {
        let event_id = event.event_id;
        let document_id = event.document_id;
        tracing::debug!(target:"cloud.document.removal", %event_id, %document_id, stage="requested", code="pending", age_ms=started.elapsed().as_millis() as u64, age_scope="pass");
        authorize(anchor).map_err(Error::Authority)?;
        let answer = sync.removal_exchange(removal::request(event)).await.map_err(|error| {
            tracing::debug!(target: "cloud.document.removal", stage = "refused", code = error.code(), age_ms = started.elapsed().as_millis() as u64, age_scope = "pass");
            error
        })?;
        if !matches!(answer.control,Control::CloudDocumentRemoved {event_id:e,document_id:d,..} if e==event_id && d==document_id)
        {
            return Err(Error::Invalid);
        }
        unanswered = unanswered.saturating_sub(1);
        progress(Progress::Removing {
            pending: unanswered,
        });
        tracing::info!(target:"cloud.document.removal", %event_id, %document_id, stage="removed", code="acknowledged", age_ms=started.elapsed().as_millis() as u64, age_scope="pass");
        // The direct answer is useful feedback, but only a durable list read
        // consumes the event. A crash here simply resends this same payload.
    }
    Ok(())
}
