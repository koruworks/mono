// SPDX-License-Identifier: MPL-2.0

//! Native proof delivery feeds the existing device challenge and exchange.
use super::*;
use crate::delivery::NativeSignInRequest;
use koru_account_wire::{
    GoogleIdToken, GoogleNativeTokenCompleteRequestV1, GoogleNativeTokenCompleteResponseV1,
    GoogleNativeTokenStartRequestV1, GoogleNativeTokenStartResponseV1,
};
use zeroize::Zeroize;

pub(super) struct PendingNative {
    pub(super) locale: String,
    pub(super) ab: Option<koru_agent_wire::OnboardingAb>,
    pub(super) request: GoogleNativeTokenCompleteRequestV1,
    prompt: NativeSignInRequest,
    submitted: bool,
    first_proof: Option<GoogleNativeTokenCompleteRequestV1>,
}

impl PendingNative {
    pub(super) fn may_have_committed(&self) -> bool {
        self.submitted
    }
}

impl Drop for PendingNative {
    fn drop(&mut self) {
        clear_request(&mut self.request);
        if let Some(first) = &mut self.first_proof {
            clear_request(first);
        }
    }
}

fn clear_request(request: &mut GoogleNativeTokenCompleteRequestV1) {
    request.flow_state.0.zeroize();
    request.client_state.0.zeroize();
    request.pkce_verifier.0.zeroize();
    request.id_token.0.zeroize();
}

impl ProductionAccountService {
    pub(super) fn start_native_token(
        &mut self,
        generation: u64,
        locale: String,
        ab: Option<koru_agent_wire::OnboardingAb>,
        cancel: &Cancel,
        progress: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        if let Some(pending) = &self.pending_native {
            if pending.locale != locale {
                return Err(ServiceError::terminal("account_retry_pending"));
            }
        } else {
            if cancel.cancelled() {
                return Err(ServiceError::terminal("cancelled"));
            }
            let client_state = zeroize::Zeroizing::new(random_secret(self.entropy.as_ref()));
            let verifier = zeroize::Zeroizing::new(random_secret(self.entropy.as_ref()));
            let mut start = self
                .client
                .google_native_token_start(&GoogleNativeTokenStartRequestV1 {
                    version: Version1,
                    descriptor: self.descriptor.clone(),
                    platform: self.platform,
                    app_version: self.app_version.clone(),
                    enrollment_protocol_version: Some(koru_pairing::ENROLLMENT_VERSION),
                    peer_protocol_version: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
                    client_state: OAuthState(client_state.to_string()),
                    pkce_challenge: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(Sha256::digest(verifier.as_bytes())),
                    locale: locale.clone(),
                    code: ab.as_ref().and_then(|v| v.code.clone()),
                    header: ab.as_ref().and_then(|v| v.header.clone()),
                    billing: ab.as_ref().and_then(|v| v.billing.clone()),
                })
                .map_err(map_client_error)?;
            let prompt = self.native_prompt(generation, &start, false)?;
            start.nonce.0.zeroize();
            self.pending_native = Some(PendingNative {
                locale,
                ab,
                request: GoogleNativeTokenCompleteRequestV1 {
                    version: Version1,
                    flow_id: start.flow_id,
                    flow_state: start.flow_state,
                    client_state: OAuthState(client_state.to_string()),
                    pkce_verifier: PkceVerifier(verifier.to_string()),
                    id_token: GoogleIdToken(String::new()),
                },
                prompt,
                submitted: false,
                first_proof: None,
            });
        }
        loop {
            if !self.pending_native.as_ref().expect("native flow").submitted {
                if cancel.cancelled() {
                    self.pending_native = None;
                    return Err(ServiceError::terminal("cancelled"));
                }
                progress(ServiceProgress::State(
                    self.state(generation, AccountStage::WaitingForBrowser),
                ));
                let mut session = self.delivery.bind_native()?;
                let pending = self.pending_native.as_mut().expect("native flow");
                // A user Retry owns a new worker generation, but the retained
                // prompt still names the same nonce and delivery request.
                pending.prompt.generation = generation;
                if self
                    .clock
                    .now_millis()
                    .is_none_or(|now| now >= pending.prompt.expires_at_millis)
                {
                    self.pending_native = None;
                    return Err(ServiceError::terminal("authentication_failed"));
                }
                let mut token = match session.wait(&pending.prompt, cancel) {
                    Ok(token) => token,
                    Err(error) => {
                        if !error.retryable {
                            self.pending_native = None;
                        }
                        return Err(error);
                    }
                };
                if cancel.cancelled() {
                    token.0.zeroize();
                    self.pending_native = None;
                    return Err(ServiceError::terminal("cancelled"));
                }
                if token.0.is_empty() || token.0.len() > 32 * 1024 {
                    token.0.zeroize();
                    self.pending_native = None;
                    return Err(ServiceError::terminal("authentication_failed"));
                }
                pending.request.id_token = token;
                // From this point the exact proof may have identity effects.
                // Retain it before the first request, including on lost replies.
                pending.submitted = true;
            }
            progress(ServiceProgress::State(
                self.state(generation, AccountStage::Exchanging),
            ));
            match self.complete_native()? {
                GoogleNativeTokenCompleteResponseV1::ConfirmationRequired {
                    confirmation, ..
                } => {
                    self.pending_creation = Some(creation::PendingCreation::native(
                        confirmation.authority.0,
                        self.pending_native.as_ref().expect("native flow"),
                    ));
                    // The same sign-in intent covers first account creation.
                    // The retained receipt still owns uncertain commit recovery.
                    return self.finish_creation(generation, Some(true), cancel, progress);
                }
                GoogleNativeTokenCompleteResponseV1::Cancelled { .. } => {
                    self.pending_native = None;
                    return Ok(ServiceOutcome {
                        state: self.state(generation, AccountStage::Cancelled),
                        credential: CredentialChange::Keep,
                        result: AccountOperationResult::Cancelled,
                        follow_up: None,
                    });
                }
                GoogleNativeTokenCompleteResponseV1::ExchangeReady { flow_id, grant, .. } => {
                    self.prepare_native_exchange(flow_id, grant);
                    return self.finish_exchange(generation);
                }
                GoogleNativeTokenCompleteResponseV1::CollisionRequired {
                    mut reauthentication,
                    ..
                } => {
                    self.continue_native_collision(generation, &mut reauthentication)?;
                }
            }
        }
    }

    fn native_prompt(
        &self,
        generation: u64,
        start: &GoogleNativeTokenStartResponseV1,
        reauthentication: bool,
    ) -> Result<NativeSignInRequest, ServiceError> {
        let now = self
            .clock
            .now_millis()
            .ok_or_else(|| ServiceError::retryable("clock_unavailable"))?;
        if start.nonce.0.is_empty()
            || start.nonce.0.len() > 512
            || start.flow_state.0.is_empty()
            || start.flow_state.0.len() > 512
        {
            return Err(ServiceError::terminal("native_challenge_invalid"));
        }
        if start.expires_at_millis <= now {
            return Err(ServiceError::terminal("native_challenge_expired"));
        }
        // The server grants a full ten minutes on its own clock. Even a small
        // client clock lag can make that look longer. Bound our local wait
        // instead of rejecting a valid challenge; completion still enforces
        // the server's expiry, including exact replay of an uncertain request.
        let expires_at_millis = start
            .expires_at_millis
            .min(now.saturating_add(10 * 60 * 1000));
        let mut bytes = [0; 8];
        self.entropy.fill_bytes(&mut bytes);
        Ok(NativeSignInRequest {
            generation,
            request_id: u64::from_le_bytes(bytes).max(1),
            nonce: start.nonce.0.clone(),
            expires_at_millis,
            reauthentication,
        })
    }

    fn complete_native(&mut self) -> Result<GoogleNativeTokenCompleteResponseV1, ServiceError> {
        let pending = self.pending_native.as_ref().expect("native flow");
        // The hosted endpoint owns expiry validation and exact replay. A local
        // deadline must never discard a request whose response was lost.
        match self.client.google_native_token_complete(&pending.request) {
            Ok(response) => Ok(response),
            Err(error) => {
                let error = map_client_error(error);
                if !error.retryable {
                    self.pending_native = None;
                }
                Err(error)
            }
        }
    }

    pub(super) fn prepare_native_exchange(&mut self, flow_id: AuthFlowId, grant: OneUseGrant) {
        let pending = self.pending_native.as_ref().expect("native flow");
        self.pending_exchange = Some(Pending {
            command: AccountCommand::StartGoogle {
                locale: pending.locale.clone(),
                ab: pending.ab.clone(),
            },
            request: NativeExchangeRequestV1 {
                version: Version1,
                flow_id,
                grant,
                client_state: pending.request.client_state.clone(),
                pkce_verifier: pending.request.pkce_verifier.clone(),
                device_ownership_proof: None,
            },
        });
    }

    pub(super) fn resolve_native_for_logout(&mut self) -> Result<(), ServiceError> {
        if self.pending_exchange.is_some() {
            if self.pending_native.is_some() {
                let _ = self.finish_exchange(0)?;
            }
            return Ok(());
        }
        let Some(pending) = &self.pending_native else {
            return Ok(());
        };
        if pending.submitted {
            match self.complete_native()? {
                GoogleNativeTokenCompleteResponseV1::ExchangeReady { flow_id, grant, .. } => {
                    self.prepare_native_exchange(flow_id, grant);
                    // Complete the normal challenge/signature before remote
                    // logout can replay the same exchange and revoke its result.
                    let _ = self.finish_exchange(0)?;
                }
                GoogleNativeTokenCompleteResponseV1::CollisionRequired { .. }
                | GoogleNativeTokenCompleteResponseV1::Cancelled { .. }
                | GoogleNativeTokenCompleteResponseV1::ConfirmationRequired { .. } => {
                    // No native session exists. Cancellation never prompts for
                    // the extra explicit proof needed to link this collision.
                }
            }
        }
        self.pending_native = None;
        Ok(())
    }

    pub(super) fn continue_native_collision(
        &mut self,
        generation: u64,
        reauthentication: &mut GoogleNativeTokenStartResponseV1,
    ) -> Result<(), ServiceError> {
        let pending = self
            .pending_native
            .as_ref()
            .ok_or_else(|| ServiceError::terminal("account_confirmation_missing"))?;
        if pending.prompt.reauthentication {
            return Err(ServiceError::terminal("authentication_failed"));
        }
        let prompt = self.native_prompt(generation, reauthentication, true)?;
        let pending = self.pending_native.as_mut().expect("native flow");
        if pending.first_proof.is_none() {
            pending.first_proof = Some(pending.request.clone());
        }
        pending.request.flow_state.0.zeroize();
        pending.request.id_token.0.zeroize();
        pending.request.flow_id = reauthentication.flow_id;
        pending.request.flow_state = reauthentication.flow_state.clone();
        reauthentication.nonce.0.zeroize();
        pending.prompt = prompt;
        pending.submitted = false;
        Ok(())
    }
}
