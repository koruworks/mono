// SPDX-License-Identifier: MPL-2.0

//! The living worker owns creation authority and the exact chosen request.
//! UI commands carry intent only; no provider or confirmation secret is saved.
use super::*;
use crate::delivery::SignInSession;
use koru_account_wire::{
    AccountCreationRequestV1, AccountCreationResponseV1, GoogleCallbackResponseV1,
    GoogleNativeTokenCompleteResponseV1,
};
use zeroize::Zeroize;

pub(super) struct PendingCreation {
    request: AccountCreationRequestV1,
    decision: Option<bool>,
    locale: String,
    /// The command that began this sign-in, so the exchange after creation is
    /// retried under the same provider's name.
    start: AccountCommand,
    flow_id: AuthFlowId,
    listener: Option<Box<dyn SignInSession>>,
    browser_continuation_opened: bool,
    // Only the issued session identifier is duplicated here. The worker owns
    // refresh bytes until vault ACK; the service already owns its access token.
    pub(super) issued_session_id: Option<String>,
}

impl Drop for PendingCreation {
    fn drop(&mut self) {
        self.request.authority.0.zeroize();
        self.request.client_state.0.zeroize();
        if let Some(v) = &mut self.request.pkce_verifier {
            v.0.zeroize();
        }
    }
}

impl PendingCreation {
    pub(super) fn browser(
        authority: String,
        client_state: String,
        verifier: String,
        start: AccountCommand,
        flow_id: AuthFlowId,
        listener: Box<dyn SignInSession>,
    ) -> Self {
        let locale = match &start {
            AccountCommand::StartGoogle { locale }
            | AccountCommand::StartApple { locale }
            | AccountCommand::StartEmailBrowser { locale } => locale.clone(),
            _ => String::new(),
        };
        Self {
            request: AccountCreationRequestV1 {
                version: Version1,
                authority: OneUseGrant(authority),
                client_state: OAuthState(client_state),
                pkce_verifier: Some(PkceVerifier(verifier)),
            },
            decision: None,
            locale,
            start,
            flow_id,
            listener: Some(listener),
            browser_continuation_opened: false,
            issued_session_id: None,
        }
    }
    pub(super) fn native(authority: String, pending: &native::PendingNative) -> Self {
        Self {
            request: AccountCreationRequestV1 {
                version: Version1,
                authority: OneUseGrant(authority),
                client_state: pending.request.client_state.clone(),
                pkce_verifier: Some(pending.request.pkce_verifier.clone()),
            },
            decision: None,
            locale: pending.locale.clone(),
            start: AccountCommand::StartGoogle {
                locale: pending.locale.clone(),
            },
            flow_id: pending.request.flow_id,
            listener: None,
            browser_continuation_opened: false,
            issued_session_id: None,
        }
    }
}

impl ProductionAccountService {
    pub(super) fn expired_creation(&mut self, generation: u64) -> ServiceOutcome {
        self.pending_creation = None;
        self.pending_native = None;
        self.pending_exchange = None;
        self.pending_logout = false;
        let mut state = self.state(
            generation,
            if self.access.is_some() {
                AccountStage::SignedIn
            } else {
                AccountStage::SignedOut
            },
        );
        state.failure_code = Some("account_confirmation_expired".into());
        ServiceOutcome {
            state,
            credential: CredentialChange::Keep,
            result: AccountOperationResult::Failed {
                code: "account_confirmation_expired".into(),
            },
            follow_up: None,
        }
    }
    pub(super) fn creation_outcome(&self, generation: u64) -> ServiceOutcome {
        ServiceOutcome {
            state: self.state(generation, AccountStage::AwaitingAccountCreation),
            credential: CredentialChange::Keep,
            result: AccountOperationResult::AccountCreationRequired,
            follow_up: None,
        }
    }

    pub(super) fn finish_creation(
        &mut self,
        generation: u64,
        decision: Option<bool>,
        cancel: &Cancel,
        progress: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        if self.pending_exchange.is_some() {
            return self.finish_exchange(generation);
        }
        let exchanging = self.state(generation, AccountStage::Exchanging);
        let pending = self
            .pending_creation
            .as_mut()
            .ok_or_else(|| ServiceError::terminal("account_confirmation_missing"))?;
        if pending.decision.is_none() {
            pending.decision = if cancel.cancelled() {
                Some(false)
            } else {
                decision
            };
        }
        let Some(confirm) = pending.decision else {
            return Ok(self.creation_outcome(generation));
        };
        progress(ServiceProgress::State(exchanging));
        // Once sent, opposite intent cannot replace an uncertain exact request.
        // The server returns whichever decision committed; retry uses this same body.
        let response = if confirm {
            self.client.confirm_account_creation(&pending.request)
        } else {
            self.client.cancel_account_creation(&pending.request)
        };
        let response = match response {
            Err(ClientError::Api { code, .. })
                if code.as_str() == koru_account_wire::ErrorCode::FLOW_EXPIRED =>
            {
                return Ok(self.expired_creation(generation));
            }
            value => value.map_err(map_client_error)?,
        };
        if self.pending_logout
            && matches!(
                &response,
                AccountCreationResponseV1::Native {
                    completion: GoogleNativeTokenCompleteResponseV1::CollisionRequired { .. }
                } | AccountCreationResponseV1::Browser {
                    callback: GoogleCallbackResponseV1::CollisionRequired { .. }
                }
            )
        {
            self.pending_creation = None;
            self.pending_native = None;
            return Ok(ServiceOutcome {
                state: self.state(generation, AccountStage::Cancelled),
                credential: CredentialChange::Keep,
                result: AccountOperationResult::Cancelled,
                follow_up: None,
            });
        }
        let pending = self
            .pending_creation
            .as_mut()
            .expect("retained confirmation");
        match response {
            AccountCreationResponseV1::Cancelled { .. } => {
                self.pending_creation = None;
                self.pending_native = None;
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::Cancelled),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::Cancelled,
                    follow_up: None,
                })
            }
            AccountCreationResponseV1::Native { completion } if pending.listener.is_none() => {
                match completion {
                    GoogleNativeTokenCompleteResponseV1::ExchangeReady {
                        flow_id, grant, ..
                    } => {
                        self.prepare_native_exchange(flow_id, grant);
                        self.finish_exchange(generation)
                    }
                    GoogleNativeTokenCompleteResponseV1::CollisionRequired {
                        mut reauthentication,
                        ..
                    } => {
                        let locale = pending.locale.clone();
                        self.continue_native_collision(generation, &mut reauthentication)?;
                        self.pending_creation = None;
                        self.start_native_token(generation, locale, cancel, progress)
                    }
                    _ => Err(ServiceError::terminal("account_confirmation_invalid")),
                }
            }
            AccountCreationResponseV1::Browser { callback } if pending.listener.is_some() => {
                let (flow_id, grant) = match callback {
                    GoogleCallbackResponseV1::Completed {
                        grant,
                        native_return: Some(delivery),
                        ..
                    } if delivery.client_state == pending.request.client_state => {
                        (pending.flow_id, grant)
                    }
                    GoogleCallbackResponseV1::CollisionRequired {
                        reauthentication,
                        reauthentication_native_return: Some(delivery),
                        native_return: Some(original),
                        ..
                    } if original.client_state == pending.request.client_state => {
                        if !pending.browser_continuation_opened {
                            let url = checked_browser_url(reauthentication.authorization_url)?;
                            pending.browser_continuation_opened = true;
                            progress(ServiceProgress::Browser(url));
                        }
                        let callback = match pending
                            .listener
                            .as_mut()
                            .expect("checked listener")
                            .wait(&delivery.client_state.0, cancel)
                        {
                            Err(_) if cancel.cancelled() => {
                                // This known terminal decision continued an existing-account
                                // collision; it did not create a new account or session.
                                self.pending_creation = None;
                                return Ok(ServiceOutcome {
                                    state: self.state(generation, AccountStage::Cancelled),
                                    credential: CredentialChange::Keep,
                                    result: AccountOperationResult::Cancelled,
                                    follow_up: None,
                                });
                            }
                            result => result?,
                        };
                        let ParsedCallback::Completed(grant) =
                            parse_callback(&callback, false, self.clock.as_ref())?
                        else {
                            return Err(ServiceError::terminal("authentication_failed"));
                        };
                        (reauthentication.flow_id, OneUseGrant(grant))
                    }
                    _ => return Err(ServiceError::terminal("account_confirmation_invalid")),
                };
                self.pending_exchange = Some(Pending {
                    command: pending.start.clone(),
                    request: NativeExchangeRequestV1 {
                        version: Version1,
                        flow_id,
                        grant,
                        client_state: pending.request.client_state.clone(),
                        pkce_verifier: pending
                            .request
                            .pkce_verifier
                            .clone()
                            .expect("native client proof"),
                        device_ownership_proof: None,
                    },
                });
                self.finish_exchange(generation)
            }
            _ => Err(ServiceError::terminal("account_confirmation_invalid")),
        }
    }
}
