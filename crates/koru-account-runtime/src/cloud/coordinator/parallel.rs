// SPDX-License-Identifier: MPL-2.0

//! Independent dataset sessions with bounded active threads and persistent
//! subscriptions. A completed first pass hands the stream to the host's
//! retained subscription; it does not hold the lane forever. Only this owner
//! admits, renews and fences permits. Session threads are joined; the host
//! owns live stream cleanup.
//!
//! Since root-catalog Plan 3.2 a lane is one scope's session — a Workspace's
//! content, or a Profile's Identity/Private units — never a pair: a
//! Workspace's content does not wait for, and is not followed by, a Profile
//! dataset.
use super::*;

struct Lane {
    pending: VecDeque<Session>,
    current: Option<Session>,
    attempt: u8,
    running: Option<std::thread::JoinHandle<()>>,
}

impl<A: Api, H: Host> Coordinator<A, H> {
    pub(super) fn run_sessions(
        &mut self,
        sessions: Vec<Session>,
        clock: &dyn Clock,
        failure: &mut Option<Failure>,
    ) -> bool {
        let mut order = VecDeque::new();
        let mut lanes = BTreeMap::<Owner, Lane>::new();
        for session in sessions {
            order.push_back(session.owner);
            lanes
                .entry(session.owner)
                .or_insert_with(|| Lane {
                    pending: VecDeque::new(),
                    current: None,
                    attempt: 0,
                    running: None,
                })
                .pending
                .push_back(session);
        }
        let (sender, receiver) = mpsc::sync_channel(lanes.len().max(1));
        let mut hinted = false;
        loop {
            let mut running = lanes.values().filter(|lane| lane.running.is_some()).count();
            for _ in 0..order.len() {
                if running >= COHORT_WORKSPACES {
                    break;
                }
                let key = order.pop_front().expect("session order");
                order.push_back(key);
                let lane = lanes.get_mut(&key).expect("session lane");
                if lane.running.is_some() {
                    continue;
                }
                loop {
                    if lane.current.is_none() {
                        lane.current = lane.pending.pop_front();
                        lane.attempt = 0;
                    }
                    let Some(session) = lane.current.as_mut() else {
                        break;
                    };
                    let prepared = self.prepare_session(session, lane.attempt, clock);
                    let result = prepared
                        .and_then(|()| self.spawn_session(session, lane.attempt, sender.clone()));
                    match result {
                        Ok(handle) => {
                            lane.running = Some(handle);
                            running += 1;
                            break;
                        }
                        Err(error) => self.finish_session(lane, Err(error), failure, &mut hinted),
                    }
                }
            }
            if lanes.values().all(|lane| lane.running.is_none()) {
                if lanes
                    .values()
                    .all(|lane| lane.current.is_none() && lane.pending.is_empty())
                {
                    break;
                }
                continue;
            }
            // Every running session is bounded by its host and observes the
            // shared authorization. One blocked Workspace cannot hold
            // another's session behind a cohort-wide join.
            let Ok((key, result)) = receiver.recv() else {
                break;
            };
            let lane = lanes.get_mut(&key).expect("owned session lane");
            if let Some(handle) = lane.running.take() {
                let _ = handle.join();
            }
            let expired = matches!(&result, Err(Failure::CredentialExpired))
                || result
                    .as_ref()
                    .is_ok_and(|report| report.error_code == Some(CREDENTIAL_EXPIRED));
            if expired && lane.attempt == 0 {
                lane.attempt = 1;
            } else {
                self.finish_session(lane, result, failure, &mut hinted);
            }
        }
        hinted
    }

    fn spawn_session(
        &self,
        session: &Session,
        attempt: u8,
        done: mpsc::SyncSender<(Owner, Result<SessionReport, Failure>)>,
    ) -> Result<std::thread::JoinHandle<()>, Failure> {
        let authority = self.current_authority()?;
        let access = self.session_access(&authority);
        let keys = self.jwks.as_ref().ok_or(Failure::Unavailable)?.1.clone();
        let host = self.host.clone();
        let scoped = session.scoped.clone();
        let owner = session.owner;
        let dataset = scoped.permit.dataset().ok_or(Failure::DatasetMismatch)?;
        tracing::info!(
            ?owner,
            dataset = ?dataset,
            attempt,
            "cloud session: starting lane"
        );
        std::thread::Builder::new()
            .name("koru-cloud-session".into())
            .spawn(move || {
                // Always report completion, including a port panic. The owner
                // joins this handle before admitting the lane's next session.
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        match &scoped.route {
                            CloudRouteResultV2::Unavailable { .. } => {
                                Err(Failure::WorkerUnavailable)
                            }
                            CloudRouteResultV2::Available { route } => host.open_dataset(
                                &scoped.permit,
                                &scoped.credential,
                                route,
                                dataset,
                                &keys,
                                &access,
                            ),
                        }
                    }))
                    .unwrap_or(Err(Failure::Unavailable));
                let _ = done.send((owner, result));
            })
            .map_err(|_| Failure::Unavailable)
    }

    fn finish_session(
        &mut self,
        lane: &mut Lane,
        result: Result<SessionReport, Failure>,
        failure: &mut Option<Failure>,
        hinted: &mut bool,
    ) {
        let session = lane.current.take().expect("active session");
        let permit = &session.scoped.permit;
        match result {
            Ok(report) => {
                tracing::info!(owner = ?session.owner, dataset = ?permit.dataset(),
                    stage = ?report.stage, code = report.error_code, "cloud session: lane answered");
                *hinted |= report.remote_hint;
                if let Some(record) = self.records.get_mut(&permit.permit_id) {
                    record.stage = report.stage;
                    record.error_code = report.error_code;
                    record.bytes_held = report.bytes_held;
                    record.queued_jobs = report.queued_jobs;
                    record.pending_consent_ids = report.pending_consent_ids;
                    record.pending_consent_ids.truncate(2);
                }
                if report.error_code == Some(CREDENTIAL_EXPIRED) {
                    *failure = Some(merge(failure.take(), Failure::CredentialExpired));
                }
            }
            Err(error) => {
                if let Some(record) = self.records.get_mut(&permit.permit_id) {
                    record.stage = stage_for(&error);
                    record.error_code = Some(error.code());
                }
                *failure = Some(merge(failure.take(), error));
            }
        }
        lane.attempt = 0;
        self.publish_progress();
    }
}
