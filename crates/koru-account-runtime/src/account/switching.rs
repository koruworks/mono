// SPDX-License-Identifier: MPL-2.0

//! One coordinator owns visible selection; one execution lane owns blocking
//! effects. A selected view never lends its authority to another context.
use super::session::SessionRuntime;
use super::*;
use std::collections::{BTreeMap, VecDeque};

#[cfg(test)]
mod pruning_tests;
#[cfg(test)]
mod tests;

pub(super) enum Message {
    Command(Envelope),
    Progress(u64, WorkerEvent),
    Finished(SessionRuntime, Vec<u64>),
    Wake,
}
pub(super) struct Routing {
    pub selected: u64,
    pub anonymous: Option<u64>,
    pub signin_scopes: std::collections::HashSet<u64>,
    pub next_scope: u64,
    pub accounts: BTreeMap<u64, Option<KoruIdentityId>>,
    pub generations: HashMap<u64, u64>,
    pub queued_generations: std::collections::HashSet<u64>,
    pub retiring_scopes: std::collections::HashSet<u64>,
    pub retired_accounts: HashMap<KoruIdentityId, u64>,
}
#[derive(Default)]
pub(super) struct Attachments {
    pub enrollment: Option<Arc<dyn crate::enrollment::Driver>>,
    /// The Cloud coordinator is activated, refreshed and deactivated by the
    /// same routing decisions as enrollment, with the same authorization.
    pub cloud: Option<Arc<dyn crate::cloud::Driver>>,
    pub responder: Option<Arc<crate::enrollment::ResponderSessionGate>>,
}
struct Held {
    runtime: Option<SessionRuntime>,
    state: Arc<Mutex<AccountState>>,
    record: Option<crate::identity::DirectoryRecord>,
    queue: VecDeque<Envelope>,
    authorization: Option<crate::enrollment::EnrollmentAuthorization>,
    removing: bool,
    removal_cutoff: Option<u64>,
    visible: Arc<AtomicBool>,
    legacy_adoption: Arc<AtomicBool>,
    pending_result: Option<WorkerEvent>,
}
impl Held {
    fn new(runtime: SessionRuntime, record: Option<crate::identity::DirectoryRecord>) -> Self {
        Self {
            state: runtime.state.clone(),
            visible: runtime.visible.clone(),
            legacy_adoption: runtime.persistence.legacy_adoption_guard(),
            runtime: Some(runtime),
            record,
            queue: VecDeque::new(),
            authorization: None,
            removing: false,
            removal_cutoff: None,
            pending_result: None,
        }
    }
}
struct Work {
    runtime: SessionRuntime,
    incoming: Option<Envelope>,
    initialize: Option<Envelope>,
}

pub(super) fn start(
    service: Box<dyn AccountService>,
    credentials: Arc<dyn CredentialStore>,
    entry: CredentialEntry,
    directory: Option<Arc<dyn directory::AccountDirectory>>,
    clock: Arc<dyn Clock>,
    initial_foreground: bool,
    notify: impl Fn(WorkerEvent) + Send + 'static,
) -> AccountControl {
    let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY * 3 + 8);
    let state = Arc::new(Mutex::new(AccountState {
        capability: true,
        stage: AccountStage::Restoring,
        ..Default::default()
    }));
    let foreground = Arc::new((Mutex::new(Some(initial_foreground)), Condvar::new()));
    let active: Active = Arc::new(Mutex::new(HashMap::new()));
    let queued = Arc::new(AtomicUsize::new(0));
    let next_generation = Arc::new(AtomicU64::new(1));
    let routing = Arc::new(Mutex::new(Routing {
        selected: 1,
        anonymous: None,
        signin_scopes: Default::default(),
        next_scope: 2,
        accounts: BTreeMap::from([(1, None)]),
        generations: HashMap::from([(0, 1)]),
        queued_generations: Default::default(),
        retiring_scopes: Default::default(),
        retired_accounts: Default::default(),
    }));
    let attachments = Arc::new(Mutex::new(Attachments::default()));
    let startup_cancel = Arc::new(AtomicBool::new(false));
    active
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(0, (0, Arc::clone(&startup_cancel)));
    let control = AccountControl {
        sender: sender.clone(),
        state: state.clone(),
        active: active.clone(),
        queued: queued.clone(),
        next_generation: next_generation.clone(),
        foreground: foreground.clone(),
        routing: routing.clone(),
        attachments: attachments.clone(),
        enrollment: None,
        cloud: None,
        responder_session: None,
    };
    std::thread::Builder::new()
        .name("koru-account-coordinator".into())
        .spawn(move || {
            let template = service.fork();
            let initial_state = Arc::new(Mutex::new(
                state.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            ));
            let initial = SessionRuntime::new(
                1,
                service,
                directory::CredentialPersistence::new(
                    credentials.clone(),
                    entry.clone(),
                    directory.clone(),
                )
                .coordinated(),
                initial_state.clone(),
            );
            let mut held = BTreeMap::from([(1, Held::new(initial, None))]);
            let namespace = entry.namespace();
            // Metadata precedes any vault or hosted inspection, so offline startup
            // still exposes known accounts and selection to the local view.
            if let Some(repo) = &directory {
                if let Ok(snapshot) = repo.snapshot(&namespace) {
                    held.get_mut(&1)
                        .unwrap()
                        .runtime
                        .as_mut()
                        .unwrap()
                        .persistence
                        .pin_initial_selection(snapshot.active);
                    let mut routes = routing.lock().unwrap_or_else(|e| e.into_inner());
                    for record in snapshot.records {
                        let scope = if Some(record.owner.account) == snapshot.active {
                            1
                        } else {
                            let scope = routes.next_scope;
                            routes.next_scope += 1;
                            scope
                        };
                        routes.accounts.insert(scope, Some(record.owner.account));
                        if scope == 1 {
                            if routes.anonymous == Some(1) {
                                routes.anonymous = None;
                            }
                            let initial = held.get_mut(&1).unwrap();
                            initial
                                .state
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .cloud_profiles = initial
                                .runtime
                                .as_ref()
                                .unwrap()
                                .service
                                .cloud_profiles(record.owner.account);
                            initial.record = Some(record);
                        } else if let Some(fresh) = template.as_ref().and_then(|t| t.fork()) {
                            let slot = Arc::new(Mutex::new(AccountState {
                                capability: true,
                                stage: AccountStage::Restoring,
                                cloud_profiles: fresh.cloud_profiles(record.owner.account),
                                ..Default::default()
                            }));
                            let persistence = directory::CredentialPersistence::new(
                                credentials.clone(),
                                entry.clone(),
                                directory.clone(),
                            )
                            .for_selection(Some(record.owner.account));
                            let runtime =
                                SessionRuntime::new(scope, fresh, persistence, slot.clone());
                            held.insert(scope, Held::new(runtime, Some(record)));
                        }
                    }
                }
            }
            let (jobs, job_receiver) = mpsc::sync_channel::<Work>(1);
            let completion = sender.clone();
            let lane_clock = clock.clone();
            let lane_active = active.clone();
            let lane_queued = queued.clone();
            std::thread::Builder::new()
                .name("koru-account-effects".into())
                .spawn(move || {
                    while let Ok(mut work) = job_receiver.recv() {
                        let before = lane_active
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        let operation_active = Arc::new(Mutex::new(before.clone()));
                        let scope = work.runtime.scope;
                        let progress = |event| {
                            let _ = completion.send(Message::Progress(scope, event));
                        };
                        if let Some(envelope) = work.initialize {
                            work.runtime.initialize(
                                envelope,
                                lane_clock.as_ref(),
                                &operation_active,
                                &progress,
                            );
                        } else {
                            work.runtime = work.runtime.step(
                                work.incoming,
                                lane_clock.clone(),
                                operation_active.clone(),
                                lane_queued.clone(),
                                progress,
                            );
                        }
                        let finished = before
                            .keys()
                            .filter(|generation| {
                                !operation_active
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .contains_key(generation)
                            })
                            .copied()
                            .collect();
                        if completion
                            .send(Message::Finished(work.runtime, finished))
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .expect("spawn account execution lane");
            let mut busy = false;
            let mut initial = Some(startup_cancel);
            let mut last_scope = 0;
            loop {
                if foreground
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none()
                {
                    break;
                }
                publish(&held, &routing, &active, &state, &notify);
                {
                    let routes = routing.lock().unwrap_or_else(|e| e.into_inner());
                    for (scope, h) in &held {
                        h.visible.store(
                            *scope == routes.selected
                                && !h.removing
                                && !routes.retiring_scopes.contains(scope),
                            Ordering::Release,
                        );
                    }
                }
                let on = *foreground.0.lock().unwrap_or_else(|e| e.into_inner()) == Some(true);
                if !busy && on {
                    // Admitted commands get a turn before automatic retry. Round
                    // robin prevents one inactive recovery from monopolizing work.
                    let candidates = held.keys().copied().collect::<Vec<_>>();
                    let ordered = candidates
                        .iter()
                        .copied()
                        .filter(|s| *s > last_scope)
                        .chain(candidates.iter().copied().filter(|s| *s <= last_scope))
                        .collect::<Vec<_>>();
                    let chosen = ordered
                        .iter()
                        .copied()
                        .find(|s| held[s].runtime.is_some() && !held[s].queue.is_empty())
                        .or_else(|| {
                            ordered.iter().copied().find(|s| {
                                held[s].runtime.as_ref().is_some_and(|r| {
                                    !r.initialized
                                        || r.next_wake(clock.as_ref(), !held[s].removing)
                                            .is_some_and(|at| at <= clock.monotonic())
                                })
                            })
                        });
                    if let Some(scope) = chosen {
                        let h = held.get_mut(&scope).unwrap();
                        let runtime = h.runtime.take().unwrap();
                        let automatic = if !runtime.initialized
                            || (h.queue.is_empty()
                                && !h.removing
                                && runtime
                                    .idle_refresh_at(clock.as_ref())
                                    .is_some_and(|at| at <= clock.monotonic()))
                        {
                            let (generation, cancel) = if scope == 1 && initial.is_some() {
                                (0, initial.take().unwrap())
                            } else {
                                let generation = next_generation.fetch_add(1, Ordering::AcqRel);
                                let cancel = Arc::new(AtomicBool::new(false));
                                active
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .insert(generation, (0, cancel.clone()));
                                routing
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .generations
                                    .insert(generation, scope);
                                (generation, cancel)
                            };
                            Some(Envelope {
                                owner_scope: scope,
                                requester: 0,
                                generation,
                                command: AccountCommand::Retry,
                                cancel,
                                counted: false,
                                cancel_attempt: false,
                            })
                        } else {
                            None
                        };
                        let (initialize, incoming) = if runtime.initialized {
                            (None, h.queue.pop_front().or(automatic))
                        } else {
                            (automatic, None)
                        };
                        if let Some(envelope) = &incoming {
                            routing
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .queued_generations
                                .remove(&envelope.generation);
                        }
                        jobs.send(Work {
                            runtime,
                            incoming,
                            initialize,
                        })
                        .expect("live account execution lane");
                        last_scope = scope;
                        busy = true;
                    }
                }
                let next = if busy || !on {
                    None
                } else {
                    held.values()
                        .filter_map(|h| h.runtime.as_ref()?.next_wake(clock.as_ref(), !h.removing))
                        .min()
                };
                let received = match next {
                    Some(at) => receiver
                        .recv_timeout(at.saturating_duration_since(clock.monotonic()))
                        .ok(),
                    None => receiver.recv().ok(),
                };
                let Some(message) = received else { continue };
                match message {
                    Message::Wake => {
                        let routes = routing.lock().unwrap_or_else(|e| e.into_inner());
                        for (scope, h) in held.iter().filter(|(scope, h)| {
                            !h.removing && !routes.retiring_scopes.contains(scope)
                        }) {
                            if let Some(authority) = &h.authorization {
                                if routes
                                    .retired_accounts
                                    .get(&authority.owner.account)
                                    .is_some_and(|cutoff| scope <= cutoff)
                                {
                                    continue;
                                }
                                if let Some(gate) = &attachments
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .responder
                                {
                                    gate.enable_account(
                                        authority.owner.account,
                                        authority.session_id.clone(),
                                    );
                                }
                            }
                        }
                        drop(routes);
                        activate_drivers(&held, &routing, &attachments);
                    }
                    Message::Progress(scope, event) => {
                        publish(&held, &routing, &active, &state, &notify);
                        match event {
                            WorkerEvent::Changed => {}
                            WorkerEvent::Browser {
                                requester,
                                generation,
                                attempt,
                                url,
                                ..
                            } => notify(WorkerEvent::Browser {
                                owner_scope: scope,
                                requester,
                                generation,
                                attempt,
                                url,
                            }),
                            event @ WorkerEvent::Result { .. } => {
                                if let Some(h) = held.get_mut(&scope) {
                                    h.pending_result = Some(event);
                                }
                            }
                        }
                    }
                    Message::Finished(mut runtime, finished) => {
                        busy = false;
                        let scope = runtime.scope;
                        let record = runtime.persistence.record();
                        // Preserve a surviving acknowledged account after a
                        // canceled or rejected attempt in a separate context.
                        let previous_account = held
                            .get(&scope)
                            .and_then(|h| h.record.clone())
                            .filter(|old| {
                                record
                                    .as_ref()
                                    .is_none_or(|next| next.owner.account != old.owner.account)
                            })
                            .and_then(|old| match directory.as_ref()?.snapshot(&namespace) {
                                Ok(snapshot) => snapshot
                                    .records
                                    .into_iter()
                                    .find(|r| r.owner.account == old.owner.account),
                                // Keep uncertain discovery metadata visible; the
                                // new context must validate it before authority.
                                Err(_) => Some(old),
                            });
                        let mut discovered = runtime.persistence.take_discovered_records();
                        discovered.extend(previous_account);
                        restore_discovered(
                            discovered,
                            scope,
                            record.as_ref(),
                            &mut held,
                            &routing,
                            template.as_deref(),
                            &credentials,
                            &entry,
                            &directory,
                        );
                        if let Some(record) = &record {
                            if let Some(cutoff) = held.get(&scope).and_then(|h| h.removal_cutoff) {
                                routing
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .retired_accounts
                                    .entry(record.owner.account)
                                    .and_modify(|old| *old = (*old).max(cutoff))
                                    .or_insert(cutoff);
                            }
                            let retired = routing
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .retired_accounts
                                .get(&record.owner.account)
                                .is_some_and(|cutoff| scope <= *cutoff);
                            if retired {
                                if let Some(h) = held.get_mut(&scope) {
                                    h.removing = true;
                                }
                            }
                            let obsolete = held
                                .iter()
                                .filter_map(|(other, h)| {
                                    (*other != scope
                                        && h.record.as_ref().is_some_and(|old| {
                                            old.owner.account == record.owner.account
                                                && (retired || old.version < record.version)
                                        }))
                                    .then_some(*other)
                                })
                                .collect::<Vec<_>>();
                            for old_scope in obsolete {
                                let h = held.get_mut(&old_scope).unwrap();
                                if !h.removing {
                                    h.removing = true;
                                    queue_retirement(
                                        h,
                                        old_scope,
                                        &next_generation,
                                        &routing,
                                        &active,
                                    );
                                }
                            }
                            if retired
                                && held.get(&scope).is_some_and(|h| {
                                    h.queue
                                        .iter()
                                        .all(|e| !matches!(e.command, AccountCommand::Logout))
                                })
                            {
                                queue_retirement(
                                    held.get_mut(&scope).unwrap(),
                                    scope,
                                    &next_generation,
                                    &routing,
                                    &active,
                                );
                            }
                        }
                        if let Some(h) = held.get_mut(&scope) {
                            // Admission retires ownership under this same lock. Keep
                            // eligibility publication inside that fence so a late
                            // vault acknowledgement cannot re-enable a signed-out owner.
                            let routes = routing.lock().unwrap_or_else(|e| e.into_inner());
                            h.removing |= routes.retiring_scopes.contains(&scope)
                                || runtime.persistence.record().is_some_and(|r| {
                                    routes
                                        .retired_accounts
                                        .get(&r.owner.account)
                                        .is_some_and(|cutoff| scope <= *cutoff)
                                });
                            h.record = runtime.persistence.record();
                            let previous = h.authorization.take();
                            h.authorization = runtime.service.enrollment_authority();
                            if !h.removing
                                && let Some(next) = &h.authorization
                            {
                                let ports = attachments.lock().unwrap_or_else(|e| e.into_inner());
                                if let Some(gate) = &ports.responder {
                                    gate.enable_account(
                                        next.owner.account,
                                        next.session_id.clone(),
                                    );
                                }
                                if let Some(previous) = previous
                                    && previous.session_id == next.session_id
                                    && previous.record_version < next.record_version
                                {
                                    if let Some(enrollment) = &ports.enrollment {
                                        enrollment
                                            .refresh_authorization(previous.clone(), next.clone());
                                    }
                                    if let Some(cloud) = &ports.cloud {
                                        cloud.refresh_authorization(previous, next.clone());
                                    }
                                }
                            }
                            drop(routes);
                            if let Some(record) = &h.record {
                                let mut routes = routing.lock().unwrap_or_else(|e| e.into_inner());
                                routes
                                    .accounts
                                    .insert(scope, (!h.removing).then_some(record.owner.account));
                                if routes.anonymous == Some(scope) {
                                    routes.anonymous = None;
                                }
                                // Only the still-selected scope may commit selection.
                                let activate = routes.selected == scope
                                    && !h.removing
                                    && !routes.retiring_scopes.contains(&scope);
                                drop(routes);
                                if activate {
                                    if let Some(repo) = &directory {
                                        if repo.activate(&namespace, record.owner.account).is_err()
                                        {
                                            h.state
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .failure_code =
                                                Some("account_selection_unavailable".into());
                                        }
                                    }
                                }
                            } else {
                                routing
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .accounts
                                    .insert(scope, None);
                            }
                            h.runtime = Some(runtime);
                        }
                        let retired_state = {
                            let routes = routing.lock().unwrap_or_else(|e| e.into_inner());
                            held.get(&routes.selected).and_then(|h| {
                                let state = h.state.lock().unwrap_or_else(|e| e.into_inner());
                                (h.removing
                                    && h.record.is_none()
                                    && state.stage == AccountStage::SignedOut
                                    && h.queue.is_empty())
                                .then(|| state.clone())
                            })
                        };
                        if let Some(retired_state) = retired_state
                            && let Some(service) = template.as_ref().and_then(|t| t.fork())
                        {
                            let mut routes = routing.lock().unwrap_or_else(|e| e.into_inner());
                            let fresh = routes.next_scope;
                            routes.next_scope += 1;
                            routes.selected = fresh;
                            routes.anonymous = Some(fresh);
                            routes.accounts.insert(fresh, None);
                            let slot = Arc::new(Mutex::new(retired_state));
                            let persistence = directory::CredentialPersistence::new(
                                credentials.clone(),
                                entry.clone(),
                                directory.clone(),
                            )
                            .for_selection(None);
                            let mut runtime =
                                SessionRuntime::new(fresh, service, persistence, slot.clone());
                            runtime.initialized = true;
                            held.insert(fresh, Held::new(runtime, None));
                        }
                        activate_drivers(&held, &routing, &attachments);
                        publish(&held, &routing, &active, &state, &notify);
                        for generation in finished {
                            finish(&active, generation);
                        }
                        publish(&held, &routing, &active, &state, &notify);
                        if let Some(event) =
                            held.get_mut(&scope).and_then(|h| h.pending_result.take())
                        {
                            notify(event);
                        }
                    }
                    Message::Command(mut envelope) => {
                        let scope = envelope.owner_scope;
                        if matches!(
                            envelope.command,
                            AccountCommand::StartGoogle { .. }
                                | AccountCommand::StartApple { .. }
                                | AccountCommand::StartEmailBrowser { .. }
                                | AccountCommand::ContinueWithEmail { .. }
                        ) {
                            if let Some(h) = held.get(&scope).filter(|h| h.record.is_none()) {
                                h.legacy_adoption.store(false, Ordering::Release);
                            }
                        }
                        match &envelope.command {
                            AccountCommand::SwitchAccount { account_id } => {
                                let account = KoruIdentityId::from_str(account_id).ok();
                                let valid = held.get(&scope).is_some_and(|h| {
                                    !h.removing
                                        && h.record
                                            .as_ref()
                                            .is_some_and(|r| Some(r.owner.account) == account)
                                });
                                let switched = valid
                                    && account.is_some_and(|account| {
                                        directory.as_ref().is_some_and(|r| {
                                            r.activate(&namespace, account).is_ok()
                                        })
                                    });
                                if switched {
                                    routing.lock().unwrap_or_else(|e| e.into_inner()).selected =
                                        scope;
                                    activate_drivers(&held, &routing, &attachments);
                                }
                                queued.fetch_sub(1, Ordering::AcqRel);
                                publish(&held, &routing, &active, &state, &notify);
                                finish(&active, envelope.generation);
                                publish(&held, &routing, &active, &state, &notify);
                                notify(WorkerEvent::Result {
                                    owner_scope: scope,
                                    requester: envelope.requester,
                                    generation: envelope.generation,
                                    result: if switched {
                                        AccountOperationResult::AccountSelected {
                                            scope_id: scope,
                                            account_id: Some(account_id.clone()),
                                        }
                                    } else {
                                        AccountOperationResult::Failed {
                                            code: "account_selection_unavailable".into(),
                                        }
                                    },
                                });
                            }
                            AccountCommand::AddAccount { locale } => {
                                let resume = held.get(&scope).is_some_and(|h| {
                                    !h.queue.is_empty()
                                        || h.runtime.as_ref().is_none_or(|r| r.retry.is_some())
                                        || h.state.lock().unwrap_or_else(|e| e.into_inner()).stage
                                            != AccountStage::SignedOut
                                });
                                if !held.contains_key(&scope) {
                                    if let Some(service) = template.as_ref().and_then(|t| t.fork())
                                    {
                                        let slot =
                                            Arc::new(Mutex::new(signed_out(envelope.generation)));
                                        let persistence = directory::CredentialPersistence::new(
                                            credentials.clone(),
                                            entry.clone(),
                                            directory.clone(),
                                        )
                                        .for_selection(None);
                                        let runtime = SessionRuntime::new(
                                            scope,
                                            service,
                                            persistence,
                                            slot.clone(),
                                        );
                                        held.insert(scope, Held::new(runtime, None));
                                    }
                                }
                                if let Some(h) = held.get_mut(&scope) {
                                    // The shared flag also fences a legacy inspection
                                    // already running on the effect lane.
                                    h.legacy_adoption.store(false, Ordering::Release);
                                    routing.lock().unwrap_or_else(|e| e.into_inner()).selected =
                                        scope;
                                    if !resume {
                                        envelope.command = AccountCommand::StartGoogle {
                                            locale: locale.clone(),
                                        };
                                        h.queue.push_back(envelope);
                                    } else {
                                        queued.fetch_sub(1, Ordering::AcqRel);
                                        publish(&held, &routing, &active, &state, &notify);
                                        finish(&active, envelope.generation);
                                        publish(&held, &routing, &active, &state, &notify);
                                        notify(WorkerEvent::Result {
                                            owner_scope: scope,
                                            requester: envelope.requester,
                                            generation: envelope.generation,
                                            result: AccountOperationResult::AccountSelected {
                                                scope_id: scope,
                                                account_id: None,
                                            },
                                        });
                                    }
                                    activate_drivers(&held, &routing, &attachments);
                                } else {
                                    queued.fetch_sub(1, Ordering::AcqRel);
                                    finish(&active, envelope.generation);
                                    notify(WorkerEvent::Result {
                                        owner_scope: scope,
                                        requester: envelope.requester,
                                        generation: envelope.generation,
                                        result: AccountOperationResult::Failed {
                                            code: "account_selection_unavailable".into(),
                                        },
                                    });
                                }
                            }
                            _ => {
                                if matches!(envelope.command, AccountCommand::Logout) {
                                    let cutoff = routing
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .next_scope
                                        .saturating_sub(1);
                                    let account = held
                                        .get(&scope)
                                        .and_then(|h| h.record.as_ref())
                                        .map(|r| r.owner.account);
                                    if let Some(account) = account {
                                        routing
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .retired_accounts
                                            .entry(account)
                                            .and_modify(|old| *old = (*old).max(cutoff))
                                            .or_insert(cutoff);
                                        for (other, h) in &mut held {
                                            if *other != scope
                                                && !h.removing
                                                && h.record
                                                    .as_ref()
                                                    .is_some_and(|r| r.owner.account == account)
                                            {
                                                h.removing = true;
                                                h.removal_cutoff = Some(cutoff);
                                                queue_retirement(
                                                    h,
                                                    *other,
                                                    &next_generation,
                                                    &routing,
                                                    &active,
                                                );
                                            }
                                        }
                                    }
                                    if let Some(h) = held.get_mut(&scope) {
                                        h.removal_cutoff = Some(cutoff);
                                    }
                                }
                                if let Some(h) = held.get_mut(&scope) {
                                    if matches!(envelope.command, AccountCommand::Logout) {
                                        h.removing = true;
                                        if let Some(record) = &h.record {
                                            if let Some(gate) = &attachments
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .responder
                                            {
                                                gate.disable_account(record.owner.account);
                                            }
                                        }
                                        h.queue.push_front(envelope);
                                    } else {
                                        h.queue.push_back(envelope);
                                    }
                                } else {
                                    if envelope.counted {
                                        queued.fetch_sub(1, Ordering::AcqRel);
                                    }
                                    finish(&active, envelope.generation);
                                }
                            }
                        }
                    }
                }
                prune_retired(&mut held, &routing, &active);
            }
        })
        .expect("spawn account coordinator");
    control
}

/// Discovery is metadata only. Each new context validates its own credential
/// before publishing authority; neither discovery nor import selects it.
#[allow(clippy::too_many_arguments)]
fn restore_discovered(
    records: Vec<crate::identity::DirectoryRecord>,
    completed_scope: u64,
    completed_record: Option<&crate::identity::DirectoryRecord>,
    held: &mut BTreeMap<u64, Held>,
    routing: &Mutex<Routing>,
    template: Option<&dyn AccountService>,
    credentials: &Arc<dyn CredentialStore>,
    entry: &CredentialEntry,
    directory: &Option<Arc<dyn directory::AccountDirectory>>,
) {
    for record in records {
        let account = record.owner.account;
        if completed_record.is_some_and(|r| r.owner.account == account)
            || held.iter().any(|(scope, h)| {
                (*scope != completed_scope || h.removing)
                    && h.record
                        .as_ref()
                        .is_some_and(|r| r.owner.account == account)
            })
        {
            continue;
        }
        let Some(service) = template.and_then(|t| t.fork()) else {
            continue;
        };
        let mut routes = routing.lock().unwrap_or_else(|e| e.into_inner());
        if routes.retired_accounts.contains_key(&account)
            || routes
                .retiring_scopes
                .iter()
                .any(|scope| routes.accounts.get(scope) == Some(&Some(account)))
        {
            continue;
        }
        let scope = routes.next_scope;
        routes.next_scope += 1;
        routes.accounts.insert(scope, Some(account));
        drop(routes);
        let state = Arc::new(Mutex::new(AccountState {
            capability: true,
            stage: AccountStage::Restoring,
            cloud_profiles: service.cloud_profiles(account),
            ..Default::default()
        }));
        let persistence = directory::CredentialPersistence::new(
            credentials.clone(),
            entry.clone(),
            directory.clone(),
        )
        .for_selection(Some(account));
        held.insert(
            scope,
            Held::new(
                SessionRuntime::new(scope, service, persistence, state),
                Some(record),
            ),
        );
    }
}

/// Results leave the coordinator before this runs. Scope IDs never recur, so
/// quiescent retired contexts need no service snapshot or routing tombstone.
fn prune_retired(held: &mut BTreeMap<u64, Held>, routing: &Mutex<Routing>, active: &Active) {
    let mut routes = routing.lock().unwrap_or_else(|e| e.into_inner());
    let active = active.lock().unwrap_or_else(|e| e.into_inner());
    let live_scopes = routes
        .generations
        .iter()
        .filter_map(|(generation, scope)| {
            (active.contains_key(generation) || routes.queued_generations.contains(generation))
                .then_some(*scope)
        })
        .collect::<std::collections::HashSet<_>>();
    let removed = held
        .iter()
        .filter_map(|(scope, h)| {
            (*scope != routes.selected
                && Some(*scope) != routes.anonymous
                && h.record.is_none()
                && h.authorization.is_none()
                && h.queue.is_empty()
                && h.pending_result.is_none()
                && !live_scopes.contains(scope)
                && matches!(
                    h.state.lock().unwrap_or_else(|e| e.into_inner()).stage,
                    AccountStage::SignedOut | AccountStage::Cancelled | AccountStage::Failed
                )
                && h.runtime
                    .as_ref()
                    .is_some_and(SessionRuntime::retirement_complete))
            .then_some(*scope)
        })
        .collect::<std::collections::HashSet<_>>();
    let retired = removed
        .iter()
        .filter_map(|scope| held.remove(scope))
        .collect::<Vec<_>>();
    routes.accounts.retain(|scope, _| !removed.contains(scope));
    routes
        .retiring_scopes
        .retain(|scope| !removed.contains(scope));
    routes
        .signin_scopes
        .retain(|scope| !removed.contains(scope));
    let queued_generations = routes.queued_generations.clone();
    routes.generations.retain(|generation, scope| {
        (active.contains_key(generation) || queued_generations.contains(generation))
            && !removed.contains(scope)
    });
    let remaining_generations = routes
        .generations
        .keys()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    routes
        .queued_generations
        .retain(|generation| remaining_generations.contains(generation));
    routes.retired_accounts.retain(|account, cutoff| {
        held.iter().any(|(scope, h)| {
            *scope <= *cutoff
                && h.record
                    .as_ref()
                    .is_none_or(|record| record.owner.account == *account)
        })
    });
    drop(active);
    drop(routes);
    // Service destruction is outside the mutex shared with command admission.
    drop(retired);
}

fn queue_retirement(
    h: &mut Held,
    scope: u64,
    next_generation: &AtomicU64,
    routing: &Mutex<Routing>,
    active: &Active,
) {
    let generation = next_generation.fetch_add(1, Ordering::AcqRel);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut routes = routing.lock().unwrap_or_else(|e| e.into_inner());
    routes.accounts.insert(scope, None);
    routes.generations.insert(generation, scope);
    active
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(generation, (0, cancel.clone()));
    h.queue.push_front(Envelope {
        owner_scope: scope,
        requester: 0,
        generation,
        command: AccountCommand::Logout,
        cancel,
        counted: false,
        cancel_attempt: false,
    });
}
fn activate_drivers(
    held: &BTreeMap<u64, Held>,
    routing: &Mutex<Routing>,
    attachments: &Mutex<Attachments>,
) {
    let routes = routing.lock().unwrap_or_else(|e| e.into_inner());
    let selected = routes.selected;
    for (scope, h) in held {
        h.visible.store(
            *scope == selected && !h.removing && !routes.retiring_scopes.contains(scope),
            Ordering::Release,
        );
    }
    let authority = held
        .get(&selected)
        .filter(|h| !h.removing && !routes.retiring_scopes.contains(&selected))
        .and_then(|h| h.authorization.clone());
    let authority = authority.filter(|a| {
        !routes
            .retired_accounts
            .get(&a.owner.account)
            .is_some_and(|cutoff| selected <= *cutoff)
    });
    let ports = attachments.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(enrollment) = &ports.enrollment {
        enrollment.activate_owned(authority.clone(), selected);
    }
    if let Some(cloud) = &ports.cloud {
        cloud.activate_owned(authority, selected);
    }
}
fn publish(
    held: &BTreeMap<u64, Held>,
    routing: &Mutex<Routing>,
    active: &Active,
    state: &Mutex<AccountState>,
    notify: &dyn Fn(WorkerEvent),
) {
    let mut routes = routing.lock().unwrap_or_else(|e| e.into_inner());
    let active = active.lock().unwrap_or_else(|e| e.into_inner());
    routes
        .generations
        .retain(|generation, _| active.contains_key(generation));
    routes
        .queued_generations
        .retain(|generation| active.contains_key(generation));
    let mut next = held
        .get(&routes.selected)
        .map(|h| h.state.lock().unwrap_or_else(|e| e.into_inner()).clone())
        .unwrap_or_default();
    next.active_scope = routes.selected;
    next.queue_depth = held.get(&routes.selected).map_or(0, |h| h.queue.len());
    next.active_account_id = routes
        .accounts
        .get(&routes.selected)
        .and_then(|a| a.map(|a| a.to_string()));
    next.held_accounts = held
        .iter()
        .filter_map(|(scope, h)| {
            let record = h.record.as_ref()?;
            if h.removing {
                return None;
            }
            let status = h.state.lock().unwrap_or_else(|e| e.into_inner());
            Some(koru_agent_wire::AccountHeldAccount {
                scope_id: *scope,
                account_id: record.owner.account.to_string(),
                auth_identity_id: Some(record.owner.auth_identity.to_string()),
                stage: status.stage,
                credential_ready: record.state == crate::identity::DirectoryState::Ready,
                failure_code: status.failure_code.clone(),
                active_generations: active
                    .keys()
                    .filter(|g| routes.generations.get(g) == Some(scope))
                    .copied()
                    .collect(),
            })
        })
        .collect();
    drop(active);
    drop(routes);
    let changed = {
        let mut current = state.lock().unwrap_or_else(|e| e.into_inner());
        if *current == next {
            false
        } else {
            *current = next;
            true
        }
    };
    if changed {
        notify(WorkerEvent::Changed);
    }
}
