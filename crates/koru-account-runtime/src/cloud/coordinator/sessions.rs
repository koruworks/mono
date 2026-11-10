// SPDX-License-Identifier: MPL-2.0

//! Refresh credentials through their issuer, keeping the admission's shape.
//! No credential is extended or patched in place: a Workspace is re-admitted
//! through the same `admit`, a Profile's units through the same
//! `admit_profile_units`, and the old permit is fenced before the new one is
//! installed.
use super::*;

impl<A: Api, H: Host> Coordinator<A, H> {
    pub(super) fn authorized<T>(
        &mut self,
        call: impl Fn(&mut A, &EnrollmentAuthorization) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let authority = self.current_authority()?;
        let result = call(&mut self.api, &authority);
        if result.as_ref().err() == Some(&Failure::SessionExpired) {
            // An account refresh racing an HTTP request gets one retry with
            // the new bearer. A 401 with the same bearer waits for renewal;
            // it is never retried as though revocation were a network error.
            let current = self.current_authority()?;
            if current.access.0 != authority.access.0 {
                return call(&mut self.api, &current);
            }
        }
        if result.as_ref().err() == Some(&Failure::CatalogBindingRequired) {
            return self.rebound(call);
        }
        result
    }

    /// QUESTIONS.md Q12 (c): the route named a precondition this client can
    /// satisfy by itself, so the binding step runs again and the call is
    /// made once more. No message — a precondition the client meets is not
    /// news for a person.
    ///
    /// **The ceiling is here, and it is the whole allowance.** A
    /// binding-required answer arriving immediately after a binding that
    /// just succeeded can only mean the binding row vanished between the two
    /// calls, which is not a precondition this client can meet: it takes the
    /// ordinary unavailable path and the pass backs off.
    ///
    /// It is two things, and each stops a different loop:
    ///
    /// - **The retried call is made here, not through
    ///   [`Self::authorized`]**, so one retry is structurally all there is.
    ///   The answer to it cannot start a second rebind, and the `match`
    ///   below turns a second binding-required into unavailable.
    /// - **`binding_retried` is set *before* the binding step**, which is
    ///   what stops the loop between the binding step and the route: the
    ///   binding request goes through `authorized` itself, so a service that
    ///   answers *binding required* to the binding route would otherwise
    ///   bind, be told to bind, and bind again without end. It also holds
    ///   the allowance to one for the whole pass, so a second route told the
    ///   same thing is answered rather than bound for again.
    ///
    /// It is cleared at the top of each pass, so the backoff — not a counter
    /// — is what paces the next attempt.
    fn rebound<T>(
        &mut self,
        call: impl Fn(&mut A, &EnrollmentAuthorization) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        if self.binding_retried {
            return Err(Failure::Unavailable);
        }
        self.binding_retried = true;
        tracing::info!(
            target: "koru_cloud",
            "cloud pass: the service asked for the catalog binding; binding and retrying once"
        );
        let outcome = match self.bind() {
            Ok(()) => {
                let authority = self.current_authority()?;
                call(&mut self.api, &authority)
            }
            Err(error) => Err(error),
        };
        match outcome {
            // The second answer is not a precondition any more.
            Err(Failure::CatalogBindingRequired) => Err(Failure::Unavailable),
            other => other,
        }
    }

    /// Re-admit one session's scope and replace its permit on the host.
    fn renew_session(&mut self, session: &mut Session) -> Result<(), Failure> {
        let authority = self.current_authority()?;
        tracing::info!(owner = ?session.owner, "cloud session: renewing worker admission");
        let replacement = match session.owner {
            Owner::Workspace(workspace) => {
                let candidate = self
                    .admissions
                    .get(&workspace)
                    .map(|admission| admission.candidate.clone())
                    .ok_or(Failure::SessionExpired)?;
                let admission = self
                    .admit(&authority, candidate)?
                    .ok_or(Failure::SessionExpired)?;
                let content = admission.content.clone();
                self.admissions.insert(workspace, admission);
                content
            }
            Owner::Profile(profile) => {
                let scoped = match self.contract {
                    Some(CloudContract::V2) => self.admit_profile_units(&authority, profile)?,
                    // The v1 pair's Profile dataset is renewed with the
                    // Workspace that lent it; re-admit that Workspace and
                    // take its Profile half.
                    _ => {
                        let lender = self
                            .admissions
                            .values()
                            .find(|admission| admission.candidate.legacy_profile == Some(profile))
                            .map(|admission| admission.candidate.clone())
                            .ok_or(Failure::SessionExpired)?;
                        let admission = self
                            .admit(&authority, lender)?
                            .ok_or(Failure::SessionExpired)?;
                        let scoped = admission
                            .legacy_profile_units
                            .clone()
                            .ok_or(Failure::SessionExpired)?;
                        self.admissions
                            .insert(admission.candidate.workspace_id, admission);
                        scoped
                    }
                };
                self.profile_units.insert(profile, scoped.clone());
                scoped
            }
        };
        self.current_authority()?;
        let old = session.scoped.permit.permit_id;
        self.host.fence(&[old])?;
        self.live.retain(|permit| permit.permit_id != old);
        self.sequence = self.sequence.saturating_add(1);
        let permit = replacement.permit.clone();
        // Retain ownership even if the host applied the context but its reply
        // was lost. The next pass must fence this id before admitting more.
        self.live.push(permit.clone());
        if let Some(mut record) = self.records.remove(&old) {
            record.permit_id = permit.permit_id;
            record.profile_id = permit
                .placement_profile()
                .map(|profile| ProfileId::from_uuid(profile.as_uuid()));
            if let Some(dataset) = permit.dataset() {
                record.dataset = dataset;
            }
            self.records.insert(permit.permit_id, record);
        }
        session.scoped = replacement;
        // Retain the replacement in the lane before a fallible acknowledgement.
        // Retrying with its old id would leak a permit per uncertain renewal.
        self.host.replace_context(
            self.sequence,
            authority.owner.account,
            self.account_generation,
            std::slice::from_ref(&permit),
        )?;
        Ok(())
    }

    pub(super) fn prepare_session(
        &mut self,
        session: &mut Session,
        attempt: u8,
        clock: &dyn Clock,
    ) -> Result<(), Failure> {
        self.current_authority()?;
        let now = clock.now_millis().ok_or(Failure::Unavailable)? / 1000;
        if attempt > 0 || !session.scoped.fresh(now) {
            self.renew_session(session)?;
        }
        let now = clock.now_millis().ok_or(Failure::Unavailable)? / 1000;
        if !session.scoped.fresh(now) {
            return Err(Failure::CredentialExpired);
        }
        if let Some(record) = self.records.get_mut(&session.scoped.permit.permit_id) {
            record.stage = Stage::Reconciling;
            record.error_code = None;
        }
        self.stage = Stage::Reconciling;
        let authority = self.current_authority()?;
        if let Err(error) = self.host.replace_blob_access(
            authority.owner.account,
            self.account_generation,
            &authority.access,
        ) {
            tracing::warn!(
                code = error.code(),
                "cloud session: file access renewal failed"
            );
        }
        self.publish_progress();
        Ok(())
    }
}
