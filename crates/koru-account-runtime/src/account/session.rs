// SPDX-License-Identifier: MPL-2.0

//! One resumable account execution context. Scheduling and visible selection
//! belong to the coordinator; all hosted and vault effects remain here.
use super::*;

pub(super) struct SessionRuntime {
    pub scope: u64,
    pub service: Box<dyn AccountService>,
    pub persistence: directory::CredentialPersistence,
    pub credential: Option<SessionCredential>,
    pub retry: Option<(Envelope, Instant, u32)>,
    pub state: Arc<Mutex<AccountState>>,
    pub initialized: bool,
    pub visible: Arc<AtomicBool>,
    discovery_pending: bool,
    hosted_recovery_started: Option<Instant>,
    pending_durability: Option<PendingDurability>,
    next_browser_attempt: u64,
    cleanup_failure: Option<&'static str>,
}
impl SessionRuntime {
    pub fn idle_refresh_at(&self, clock: &dyn Clock) -> Option<Instant> {
        if !self.initialized
            || self.credential.is_none()
            // A hosted call waiting out its backoff does not hold the
            // credential; the bearer it will need must stay fresh meanwhile.
            || self
                .retry
                .as_ref()
                .is_some_and(|(envelope, _, _)| !hosted_command(&envelope.command))
            || self.pending_durability.is_some()
            || self.discovery_pending
            || !self.service.cancellation_safe()
        {
            return None;
        }
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.stage != AccountStage::SignedIn || state.failure_code.is_some() {
            // Terminal failures and exhausted retries require an explicit
            // retry. An expired deadline must not spin on the same refusal.
            return None;
        }
        let due = self.service.access_refresh_at_millis()?;
        let delay = due.saturating_sub(clock.now_millis()?).max(0) as u64;
        clock.monotonic().checked_add(Duration::from_millis(delay))
    }

    pub fn next_wake(&self, clock: &dyn Clock, allow_refresh: bool) -> Option<Instant> {
        let retry = self.retry.as_ref().map(|(_, at, _)| *at);
        let refresh = allow_refresh.then(|| self.idle_refresh_at(clock)).flatten();
        match (retry, refresh) {
            (Some(retry), Some(refresh)) => Some(retry.min(refresh)),
            (retry, refresh) => retry.or(refresh),
        }
    }

    pub fn retirement_complete(&self) -> bool {
        self.initialized
            && self.retry.is_none()
            && self.credential.is_none()
            && self.persistence.record().is_none()
            && self.pending_durability.is_none()
            && self.cleanup_failure.is_none()
            && self.service.cancellation_safe()
    }

    pub fn new(
        scope: u64,
        mut service: Box<dyn AccountService>,
        persistence: directory::CredentialPersistence,
        state: Arc<Mutex<AccountState>>,
    ) -> Self {
        let visible = Arc::new(AtomicBool::new(false));
        service.set_visibility(visible.clone());
        Self {
            scope,
            service,
            persistence,
            credential: None,
            retry: None,
            state,
            initialized: false,
            visible,
            discovery_pending: false,
            hosted_recovery_started: None,
            pending_durability: None,
            next_browser_attempt: 1,
            cleanup_failure: None,
        }
    }
    pub fn initialize(
        &mut self,
        envelope: Envelope,
        clock: &dyn Clock,
        active: &Active,
        notify: &dyn Fn(WorkerEvent),
    ) {
        self.initialized = true;
        let mut retryable = false;
        self.credential = match self.persistence.restore(self.service.as_mut()) {
            Ok(None) => {
                publish_state(signed_out(envelope.generation), &self.state, notify);
                None
            }
            Ok(Some(value)) => {
                // Armed from the slot these bytes came out of, before anything
                // can refresh them: a rotation must not go out under a key
                // that is not already durable beside the token it rotates.
                self.service
                    .arm_refresh_retry_key(self.persistence.refresh_retry_key());
                self.service
                    .credential_restored(&value, self.persistence.record());
                Some(value)
            }
            Err(error) => {
                self.discovery_pending = true;
                retryable = error.retryable;
                failure_at(
                    envelope.generation,
                    error.code,
                    error.retryable.then_some(HOSTED_RETRY_MIN),
                    &self.state,
                    notify,
                    clock,
                );
                None
            }
        };
        if self.credential.is_some() || (self.discovery_pending && retryable) {
            self.retry = Some((
                envelope,
                clock.monotonic()
                    + if self.discovery_pending {
                        HOSTED_RETRY_MIN
                    } else {
                        Duration::ZERO
                    },
                0,
            ));
        } else {
            finish(active, envelope.generation);
        }
    }
    pub fn step(
        self,
        incoming: Option<Envelope>,
        clock: Arc<dyn Clock>,
        worker_active: Active,
        worker_queued: Arc<AtomicUsize>,
        notify: impl Fn(WorkerEvent),
    ) -> Self {
        let Self {
            scope,
            mut service,
            mut persistence,
            mut credential,
            mut retry,
            state: worker_state,
            initialized,
            visible,
            mut discovery_pending,
            mut hosted_recovery_started,
            mut pending_durability,
            mut next_browser_attempt,
            mut cleanup_failure,
        } = self;
        let failure = |generation,
                       code: &str,
                       retry,
                       state: &Mutex<AccountState>,
                       notify: &dyn Fn(WorkerEvent)| {
            failure_at(generation, code, retry, state, notify, clock.as_ref());
        };
        let mut parked_hosted = None;
        // A breakable block, not a loop: every path ends in `break`, and the
        // early ones skip the rest of the step.
        #[allow(clippy::never_loop)]
        loop {
            let (mut envelope, failures) = match incoming {
                Some(mut value) => {
                    if matches!(value.command, AccountCommand::Cancel { .. }) {
                        // An explicit cancellation owns its terminal wording;
                        // it still performs the exact already-issued cleanup.
                        cleanup_failure = None;
                    }
                    if value.counted {
                        worker_queued.fetch_sub(1, Ordering::AcqRel);
                    }
                    if let Some((mut pending, pending_at, pending_failures)) = retry.take() {
                        match &value.command {
                            // The coordinator's own idle refresh came due
                            // while a hosted call waits out its backoff. Run
                            // the refresh alone and leave the call parked at
                            // its own time and recovery window; running the
                            // call early would spend the backoff it is
                            // honouring (a 429's, typically).
                            AccountCommand::Retry
                                if value.requester == 0
                                    && !value.counted
                                    && hosted_command(&pending.command)
                                    && pending_at > clock.monotonic() =>
                            {
                                parked_hosted = Some((
                                    (pending, pending_at, pending_failures),
                                    hosted_recovery_started.take(),
                                ));
                                (value, 0)
                            }
                            AccountCommand::StartGoogle { .. }
                            | AccountCommand::StartApple { .. }
                            | AccountCommand::ContinueWithEmail { .. }
                            | AccountCommand::SubmitEmailPassword { .. }
                                if discovery_pending
                                    && pending_durability.is_none()
                                    && service.cancellation_safe()
                                    && persistence.legacy_ready() =>
                            {
                                finish(&worker_active, pending.generation);
                                (value, 0)
                            }
                            AccountCommand::Retry => {
                                finish(&worker_active, pending.generation);
                                pending.requester = value.requester;
                                pending.generation = value.generation;
                                pending.cancel = value.cancel;
                                (pending, pending_failures)
                            }
                            AccountCommand::Cancel { .. }
                                if !service.cancellation_safe() || pending_durability.is_some() =>
                            {
                                if pending.generation != value.generation {
                                    finish(&worker_active, pending.generation);
                                }
                                value.command = AccountCommand::Logout;
                                value.cancel_attempt = true;
                                (value, 0)
                            }
                            AccountCommand::Cancel { .. } => {
                                retry = Some((pending, clock.monotonic(), pending_failures));
                                (value, 0)
                            }
                            AccountCommand::Logout => {
                                if pending.generation != value.generation {
                                    finish(&worker_active, pending.generation);
                                }
                                (value, 0)
                            }
                            _ => {
                                retry = Some((pending, clock.monotonic(), pending_failures));
                                if value.requester != 0 {
                                    notify(WorkerEvent::Result {
                                        owner_scope: scope,
                                        requester: value.requester,
                                        generation: value.generation,
                                        result: command_failure(
                                            &value.command,
                                            "account_retry_pending",
                                        ),
                                    });
                                }
                                finish(&worker_active, value.generation);
                                break;
                            }
                        }
                    } else {
                        (value, 0)
                    }
                }
                None => match retry.take() {
                    Some((value, _, failures)) => (value, failures),
                    None => break,
                },
            };
            if envelope.cancel.load(Ordering::Acquire) {
                if service.cancellation_safe() && pending_durability.is_none() {
                    service.cancelled(envelope.generation);
                    publish_cancelled(&envelope, &worker_state, &notify);
                    finish(&worker_active, envelope.generation);
                    break;
                }
                envelope.cancel.store(false, Ordering::Release);
            }
            if matches!(envelope.command, AccountCommand::Cancel { .. }) {
                if !is_active(&worker_active, envelope.requester, envelope.generation) {
                    break;
                }
                if service.cancellation_safe() && pending_durability.is_none() {
                    service.cancelled(envelope.generation);
                    publish_cancelled(&envelope, &worker_state, &notify);
                    finish(&worker_active, envelope.generation);
                }
                break;
            }
            if !matches!(envelope.command, AccountCommand::Logout)
                && let Some(mut pending) = pending_durability.take()
            {
                let stored =
                    persistence.replace(&pending.credential, &mut pending.write, service.as_mut());
                if let Err(error) = stored {
                    if error.code == "account_repair_identity_mismatch" {
                        pending_durability = Some(pending);
                        cleanup_failure = Some(error.code);
                        envelope.command = AccountCommand::Logout;
                        envelope.cancel_attempt = true;
                        retry = Some((envelope, clock.monotonic(), 0));
                        break;
                    }
                    let backoff = credential_retry_delay(
                        &error,
                        failures,
                        &mut hosted_recovery_started,
                        clock.as_ref(),
                    );
                    pending_durability = Some(pending);
                    failure(
                        envelope.generation,
                        error.code,
                        backoff,
                        &worker_state,
                        &notify,
                    );
                    if let Some(backoff) = backoff {
                        retry = clock
                            .monotonic()
                            .checked_add(backoff)
                            .map(|at| (envelope, at, failures.saturating_add(1)));
                    } else {
                        finish(&worker_active, envelope.generation);
                    }
                    break;
                }
                credential = Some(pending.credential);
                // The replacement's own retry key went into the same vault
                // write, so the next rotation of these bytes already has a
                // durable key to present.
                service.arm_refresh_retry_key(persistence.refresh_retry_key());
                service.credential_owner_acknowledged(persistence.record());
                log_persisted(scope, true);
                pending.state.generation = envelope.generation;
                if pending.state.failure_code.is_none() {
                    pending.state.failure_code = persistence.repair_code().map(str::to_owned);
                }
                publish_state(pending.state, &worker_state, &notify);
                if let Some(command) = pending.follow_up {
                    envelope.command = command;
                    retry = Some((envelope, clock.monotonic(), 0));
                    break;
                }
                if envelope.requester != 0 {
                    notify(WorkerEvent::Result {
                        owner_scope: scope,
                        requester: envelope.requester,
                        generation: envelope.generation,
                        result: pending.result,
                    });
                }
                finish(&worker_active, envelope.generation);
                break;
            }
            let generation = envelope.generation;
            let requester = envelope.requester;
            let is_logout = matches!(envelope.command, AccountCommand::Logout);
            let preserve_failure_surface = matches!(envelope.command, AccountCommand::Retry)
                && matches!(
                    worker_state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .stage,
                    AccountStage::Unavailable | AccountStage::Failed
                );
            let cancel = Cancel(Arc::clone(&envelope.cancel));
            // A hosted command whose access had expired refreshes first and
            // says so. Its failure is then the credential's, not the call's.
            let refreshed_first = std::cell::Cell::new(false);
            let mut progress = |progress| match progress {
                ServiceProgress::State(mut next)
                    if !cancel.cancelled() && !preserve_failure_surface =>
                {
                    refreshed_first
                        .set(refreshed_first.get() || next.stage == AccountStage::Refreshing);
                    next.generation = generation;
                    publish_state(next, &worker_state, &notify);
                }
                ServiceProgress::State(next) => {
                    refreshed_first
                        .set(refreshed_first.get() || next.stage == AccountStage::Refreshing);
                }
                ServiceProgress::Browser(url) if !cancel.cancelled() && requester != 0 => {
                    let attempt = next_browser_attempt;
                    next_browser_attempt = next_browser_attempt.saturating_add(1);
                    notify(WorkerEvent::Browser {
                        owner_scope: scope,
                        requester,
                        generation,
                        attempt,
                        url,
                    })
                }
                _ => {}
            };
            let changes_credential = matches!(
                envelope.command,
                AccountCommand::StartGoogle { .. }
                    | AccountCommand::StartApple { .. }
                    // An email sign-in ends in the same stored credential a
                    // Google one does, so it owes the same legacy discovery
                    // before it starts. Leaving it out skipped the migration
                    // check the Google path refuses to start without.
                    | AccountCommand::ContinueWithEmail { .. }
                    | AccountCommand::SubmitEmailPassword { .. }
                    | AccountCommand::ConfirmAccountCreation
                    | AccountCommand::CancelAccountCreation
                    | AccountCommand::Logout
            );
            let must_discover = if changes_credential {
                !persistence.legacy_ready()
            } else {
                discovery_pending
            };
            if must_discover && !matches!(envelope.command, AccountCommand::Status) {
                match persistence.restore(service.as_mut()) {
                    Ok(value) if !changes_credential || persistence.legacy_ready() => {
                        credential = value;
                        service.arm_refresh_retry_key(persistence.refresh_retry_key());
                        discovery_pending = false;
                    }
                    Ok(_) => {
                        failure(
                            generation,
                            "credential_migration_pending",
                            None,
                            &worker_state,
                            &notify,
                        );
                        finish(&worker_active, generation);
                        break;
                    }
                    Err(error) => {
                        let now = clock.monotonic();
                        let started = *hosted_recovery_started.get_or_insert(now);
                        let delay = hosted_retry_delay(
                            failures.saturating_add(1),
                            now.saturating_duration_since(started),
                        );
                        failure(generation, error.code, delay, &worker_state, &notify);
                        if let Some(delay) = delay {
                            retry = Some((envelope, now + delay, failures.saturating_add(1)));
                        } else {
                            finish(&worker_active, generation);
                        }
                        break;
                    }
                }
            }
            if changes_credential && persistence.legacy_ready() {
                discovery_pending = false;
            }
            if is_logout {
                if persistence
                    .clear_retired(
                        pending_durability
                            .as_ref()
                            .map(|p| (&p.credential, &p.write)),
                        !envelope.cancel_attempt,
                    )
                    .is_err()
                {
                    let backoff = DURABILITY_RETRY;
                    failure(
                        generation,
                        "credential_unavailable",
                        Some(backoff),
                        &worker_state,
                        &notify,
                    );
                    retry = clock
                        .monotonic()
                        .checked_add(backoff)
                        .map(|at| (envelope, at, failures.saturating_add(1)));
                    break;
                }
                service.credential_removed(persistence.removed_owner());
                let restored = credential.take();
                let previous = pending_durability.take().map(|p| p.credential).or(restored);
                pending_durability = None;
                // Local erasure is authoritative. Remote logout is a
                // bounded best effort after the vault can no longer
                // restore this session on restart.
                let remote = service.execute(
                    generation,
                    envelope.command.clone(),
                    previous.as_ref(),
                    &cancel,
                    &mut progress,
                );
                if remote.as_ref().is_err_and(|error| error.retryable)
                    && !service.cancellation_safe()
                {
                    let now = clock.monotonic();
                    let started = *hosted_recovery_started.get_or_insert(now);
                    let delay = hosted_retry_delay(
                        failures.saturating_add(1),
                        now.saturating_duration_since(started),
                    );
                    failure(
                        generation,
                        "account_unavailable",
                        delay,
                        &worker_state,
                        &notify,
                    );
                    if let Some(delay) = delay {
                        retry = now
                            .checked_add(delay)
                            .map(|at| (envelope, at, failures.saturating_add(1)));
                    } else {
                        finish(&worker_active, generation);
                    }
                    break;
                }
                let mut next = signed_out(generation);
                let mut result = match remote {
                    Ok(outcome)
                        if outcome.state.failure_code.as_deref()
                            == Some("account_confirmation_expired") =>
                    {
                        next.failure_code = outcome.state.failure_code;
                        outcome.result
                    }
                    Err(error) if error.code == crate::enrollment::PERSISTENCE_FAILED => {
                        next.failure_code = Some(error.code.into());
                        AccountOperationResult::Failed {
                            code: error.code.into(),
                        }
                    }
                    Err(_) => {
                        next.failure_code = Some("remote_revocation_pending".into());
                        AccountOperationResult::SignedOut
                    }
                    Ok(_) if envelope.cancel_attempt => AccountOperationResult::Cancelled,
                    Ok(_) => AccountOperationResult::SignedOut,
                };
                if let Some(code) = cleanup_failure.take() {
                    if envelope.cancel_attempt
                        && matches!(result, AccountOperationResult::Cancelled)
                    {
                        next.stage = AccountStage::Failed;
                        next.failure_code = Some(code.into());
                        result = AccountOperationResult::Failed { code: code.into() };
                    }
                }
                publish_state(next, &worker_state, &notify);
                if requester != 0 {
                    notify(WorkerEvent::Result {
                        owner_scope: scope,
                        requester,
                        generation,
                        result,
                    });
                }
                finish(&worker_active, generation);
                break;
            }
            // Nothing is armed when the slot held no retry key and the vault
            // would not take one at restore. Topping it up here is what keeps
            // a momentary vault failure from leaving a session unable to
            // rotate for the rest of the process; an armed one costs nothing.
            if let Some(restored) = &credential
                && persistence.refresh_retry_key().is_none()
            {
                service.arm_refresh_retry_key(persistence.ensure_refresh_retry_key(restored));
            }
            let outcome = service.execute(
                generation,
                envelope.command.clone(),
                credential.as_ref(),
                &cancel,
                &mut progress,
            );
            // A returned success is already the terminal fact. In
            // particular, link/rebind/Profile mutations may have
            // committed remotely before a racing cancel becomes
            // visible. Cancellation may stop an operation that did
            // not complete, but it must not relabel committed success.
            if cancel.cancelled() && outcome.is_err() {
                if service.cancellation_safe() && pending_durability.is_none() {
                    service.cancelled(generation);
                    publish_cancelled(&envelope, &worker_state, &notify);
                    finish(&worker_active, generation);
                    break;
                }
                if persistence
                    .clear(
                        pending_durability
                            .as_ref()
                            .map(|p| (&p.credential, &p.write)),
                        false,
                    )
                    .is_err()
                {
                    let backoff = DURABILITY_RETRY;
                    envelope.command = AccountCommand::Logout;
                    envelope.cancel.store(false, Ordering::Release);
                    failure(
                        generation,
                        "credential_unavailable",
                        Some(backoff),
                        &worker_state,
                        &notify,
                    );
                    retry = clock
                        .monotonic()
                        .checked_add(backoff)
                        .map(|at| (envelope, at, failures.saturating_add(1)));
                    break;
                }
                service.credential_removed(persistence.removed_owner());
                let restored = credential.take();
                let previous = pending_durability.take().map(|p| p.credential).or(restored);
                pending_durability = None;
                let remote = service.execute(
                    generation,
                    AccountCommand::Logout,
                    previous.as_ref(),
                    &cancel,
                    &mut progress,
                );
                if remote.as_ref().is_err_and(|error| error.retryable)
                    && !service.cancellation_safe()
                {
                    envelope.command = AccountCommand::Logout;
                    envelope.cancel.store(false, Ordering::Release);
                    let now = clock.monotonic();
                    let started = *hosted_recovery_started.get_or_insert(now);
                    let delay = hosted_retry_delay(
                        failures.saturating_add(1),
                        now.saturating_duration_since(started),
                    );
                    failure(
                        generation,
                        "account_unavailable",
                        delay,
                        &worker_state,
                        &notify,
                    );
                    if let Some(delay) = delay {
                        retry = now
                            .checked_add(delay)
                            .map(|at| (envelope, at, failures.saturating_add(1)));
                    } else {
                        finish(&worker_active, generation);
                    }
                    break;
                }
                let mut next = signed_out(generation);
                let result = match remote {
                    Ok(outcome)
                        if outcome.state.failure_code.as_deref()
                            == Some("account_confirmation_expired") =>
                    {
                        next.failure_code = outcome.state.failure_code;
                        outcome.result
                    }
                    Err(error) if error.code == crate::enrollment::PERSISTENCE_FAILED => {
                        next.failure_code = Some(error.code.into());
                        AccountOperationResult::Failed {
                            code: error.code.into(),
                        }
                    }
                    Err(_) => {
                        next.failure_code = Some("remote_revocation_pending".into());
                        AccountOperationResult::Cancelled
                    }
                    Ok(_) => AccountOperationResult::Cancelled,
                };
                publish_state(next, &worker_state, &notify);
                if requester != 0 {
                    notify(WorkerEvent::Result {
                        owner_scope: scope,
                        requester,
                        generation,
                        result,
                    });
                }
                finish(&worker_active, generation);
                break;
            }
            match outcome {
                Ok(mut outcome) => {
                    hosted_recovery_started = None;
                    match outcome.credential {
                        CredentialChange::Keep => {}
                        CredentialChange::Replace(next) => {
                            let mut write = directory::PendingCredentialWrite::default();
                            if let Err(error) =
                                persistence.replace(&next, &mut write, service.as_mut())
                            {
                                let backoff = credential_retry_delay(
                                    &error,
                                    failures,
                                    &mut hosted_recovery_started,
                                    clock.as_ref(),
                                );
                                pending_durability = Some(PendingDurability {
                                    credential: next,
                                    state: outcome.state,
                                    result: outcome.result,
                                    follow_up: outcome.follow_up,
                                    write,
                                });
                                if error.code == "account_repair_identity_mismatch" {
                                    cleanup_failure = Some(error.code);
                                    envelope.command = AccountCommand::Logout;
                                    envelope.cancel_attempt = true;
                                    retry = Some((envelope, clock.monotonic(), 0));
                                    break;
                                }
                                failure(generation, error.code, backoff, &worker_state, &notify);
                                if let Some(backoff) = backoff {
                                    retry = clock
                                        .monotonic()
                                        .checked_add(backoff)
                                        .map(|at| (envelope, at, failures.saturating_add(1)));
                                } else {
                                    finish(&worker_active, envelope.generation);
                                }
                                break;
                            }
                            credential = Some(next);
                            service.arm_refresh_retry_key(persistence.refresh_retry_key());
                            service.credential_owner_acknowledged(persistence.record());
                            log_persisted(scope, false);
                        }
                        CredentialChange::Remove => {
                            credential = None;
                            if persistence.clear(None, true).is_err() {
                                failure(
                                    generation,
                                    "credential_unavailable",
                                    None,
                                    &worker_state,
                                    &notify,
                                );
                                finish(&worker_active, generation);
                                break;
                            }
                        }
                    }
                    outcome.state.generation = generation;
                    if outcome.state.failure_code.is_none() {
                        outcome.state.failure_code = persistence.repair_code().map(str::to_owned);
                    }
                    publish_state(outcome.state, &worker_state, &notify);
                    if let Some(command) = outcome.follow_up {
                        envelope.command = command;
                        retry = Some((envelope, clock.monotonic(), 0));
                        break;
                    }
                    if requester != 0 {
                        notify(WorkerEvent::Result {
                            owner_scope: scope,
                            requester,
                            generation,
                            result: outcome.result,
                        });
                    }
                    finish(&worker_active, generation);
                }
                Err(error) if error.clear_credential => {
                    hosted_recovery_started = None;
                    if persistence.clear(None, true).is_err() {
                        let backoff = DURABILITY_RETRY;
                        envelope.command = AccountCommand::Logout;
                        failure(
                            generation,
                            "credential_unavailable",
                            Some(backoff),
                            &worker_state,
                            &notify,
                        );
                        retry = clock
                            .monotonic()
                            .checked_add(backoff)
                            .map(|at| (envelope, at, failures.saturating_add(1)));
                        break;
                    }
                    credential = None;
                    publish_state(
                        AccountState {
                            generation,
                            stage: AccountStage::SignedOut,
                            capability: true,
                            failure_code: Some(error.code.into()),
                            ..Default::default()
                        },
                        &worker_state,
                        &notify,
                    );
                    if requester != 0 {
                        notify(WorkerEvent::Result {
                            owner_scope: scope,
                            requester,
                            generation,
                            result: command_failure(&envelope.command, error.code),
                        });
                    }
                    finish(&worker_active, generation);
                }
                Err(error) if error.retryable => {
                    let failures = failures.saturating_add(1);
                    let now = clock.monotonic();
                    let started = *hosted_recovery_started.get_or_insert(now);
                    let backoff =
                        hosted_retry_delay(failures, now.saturating_duration_since(started));
                    if hosted_command(&envelope.command) && !refreshed_first.get() {
                        // A rate-limited or unavailable hosted call says
                        // nothing about the session: the account stays as it
                        // is published, the requester hears its own command
                        // failed, and the call waits out its backoff in the
                        // background. Only the credential's own paths below
                        // may take the account to Failed.
                        let code = if backoff.is_some() {
                            error.code
                        } else {
                            hosted_recovery_started = None;
                            "account_retry_exhausted"
                        };
                        if requester != 0 {
                            notify(WorkerEvent::Result {
                                owner_scope: scope,
                                requester,
                                generation,
                                result: command_failure(&envelope.command, code),
                            });
                        }
                        finish(&worker_active, generation);
                        let mut next = worker_state
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .clone();
                        next.retry_at_millis = backoff.and_then(|delay| {
                            clock
                                .now_millis()?
                                .checked_add(i64::try_from(delay.as_millis()).ok()?)
                        });
                        publish_state(next, &worker_state, &notify);
                        if let Some(backoff) = backoff {
                            // Answered already: the retry has no one to tell.
                            envelope.requester = 0;
                            retry = now.checked_add(backoff).map(|at| (envelope, at, failures));
                        }
                    } else if let Some(backoff) = backoff {
                        failure(
                            generation,
                            error.code,
                            Some(backoff),
                            &worker_state,
                            &notify,
                        );
                        retry = now.checked_add(backoff).map(|at| (envelope, at, failures));
                    } else {
                        hosted_recovery_started = None;
                        failure(
                            generation,
                            "account_retry_exhausted",
                            None,
                            &worker_state,
                            &notify,
                        );
                        if requester != 0 {
                            notify(WorkerEvent::Result {
                                owner_scope: scope,
                                requester,
                                generation,
                                result: command_failure(
                                    &envelope.command,
                                    "account_retry_exhausted",
                                ),
                            });
                        }
                        finish(&worker_active, generation);
                    }
                }
                // **A hosted call refused for its own reasons answers its
                // requester, and the account stays as it is.** The retryable
                // arm above already held this for outages; a *terminal*
                // refusal fell through to the arm below and published the
                // whole account as Failed. Selecting a Profile that had been
                // removed from this computer was refused this way, and every
                // surface then read "signed out" while the stored session was
                // fine — and a Failed account stops its idle refresh, so the
                // bearer then went stale for real.
                Err(error)
                    if hosted_command(&envelope.command)
                        && !refreshed_first.get()
                        && !speaks_for_the_session(error.code) =>
                {
                    hosted_recovery_started = None;
                    if requester != 0 {
                        notify(WorkerEvent::Result {
                            owner_scope: scope,
                            requester,
                            generation,
                            result: command_failure(&envelope.command, error.code),
                        });
                    }
                    finish(&worker_active, generation);
                }
                Err(error) => {
                    hosted_recovery_started = None;
                    failure(generation, error.code, None, &worker_state, &notify);
                    if requester != 0 {
                        notify(WorkerEvent::Result {
                            owner_scope: scope,
                            requester,
                            generation,
                            result: command_failure(&envelope.command, error.code),
                        });
                    }
                    finish(&worker_active, generation);
                }
            }
            break;
        }
        if let Some(((pending, at, failures), started)) = parked_hosted {
            if retry.is_none() && credential.is_some() && pending_durability.is_none() {
                retry = Some((pending, at, failures));
                hosted_recovery_started = started;
            } else {
                // The refresh left work of its own in the one retry slot (its
                // follow-up, its own retry) or left no session to call with.
                // The parked call was answered when it failed; drop it rather
                // than displace the credential's work.
                tracing::info!(
                    target: "koru_account",
                    owner_scope = scope,
                    "dropped a parked hosted retry after the idle refresh"
                );
                if pending.requester != 0 {
                    notify(WorkerEvent::Result {
                        owner_scope: scope,
                        requester: pending.requester,
                        generation: pending.generation,
                        result: command_failure(&pending.command, "account_retry_pending"),
                    });
                }
                finish(&worker_active, pending.generation);
            }
        }
        Self {
            scope,
            service,
            persistence,
            credential,
            retry,
            state: worker_state,
            initialized,
            visible,
            discovery_pending,
            hosted_recovery_started,
            pending_durability,
            next_browser_attempt,
            cleanup_failure,
        }
    }
}

/// The durable acknowledgement of a replaced credential: the vault and the
/// directory both hold it. Its absence after a "credential refresh answered"
/// line is a rotation the next start will present stale.
fn log_persisted(owner_scope: u64, after_retry: bool) {
    tracing::info!(
        target: "koru_account",
        owner_scope,
        after_retry,
        "credential persisted"
    );
}

/// A call made with the session's bearer that answers for itself alone. A
/// retryable refusal of one (a 429, an outage) is about that call, not about
/// whether this device is signed in, so it neither fails the account nor
/// stops the idle refresh that keeps its bearer fresh.
///
/// Deliberately absent: everything that restores, refreshes, exchanges or
/// removes the credential, and the Google link and rebind flows, whose
/// continuation the service holds and `Retry` resumes. A command added later
/// is on the credential's side until someone puts it here.
/// The terminal refusals of a hosted call that are about the session rather
/// than the call: the service no longer accepts the credential, or the
/// credential this device holds is not the one the call was made for. These
/// still take the account to Failed, because the person has to sign in again.
/// Every other terminal code — a Profile not authorized, removed from this
/// computer, bound to another account, a request the service did not like —
/// is the call's own answer. A revoked session clears the credential in an
/// arm of its own before this is asked.
fn speaks_for_the_session(code: &str) -> bool {
    matches!(
        code,
        "authentication_required"
            | "credential_invalid"
            | "credential_rejected"
            | "credential_owner_mismatch"
            | "credential_owner_unavailable"
            | "account_repair_identity_mismatch"
            | "session_revoked"
    )
}

fn hosted_command(command: &AccountCommand) -> bool {
    matches!(
        command,
        AccountCommand::InspectAccountSession
            | AccountCommand::InspectCloudSubscription { .. }
            | AccountCommand::SetProfileCloudSync { .. }
            | AccountCommand::InspectCloudSync { .. }
            | AccountCommand::ListProfiles
            | AccountCommand::ListProfileChoices
            | AccountCommand::SelectProfiles { .. }
            | AccountCommand::RetryEnrollment { .. }
            | AccountCommand::CancelEnrollment
            | AccountCommand::CreateProfile { .. }
            | AccountCommand::AttachProfile { .. }
            | AccountCommand::RemoveProfileAuthorization { .. }
            | AccountCommand::DeleteProfile { .. }
            | AccountCommand::RestoreProfile { .. }
            | AccountCommand::ReconcileProfileLifecycles
            | AccountCommand::InspectProfileLifecycle { .. }
            | AccountCommand::AcknowledgeProfileCleanup { .. }
    )
}
