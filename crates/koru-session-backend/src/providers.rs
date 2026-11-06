// SPDX-License-Identifier: MPL-2.0

//! Provider subscription state is connection-scoped. Readers report through
//! the command queue so only its owner publishes accepted readiness.
use super::*;
use koru_session_protocol::{ExecutableAvailability as A, ProviderReadiness};

pub(super) struct Catalog(Vec<ProviderReadiness>);

impl Catalog {
    pub(super) fn snapshot(&self) -> Vec<ProviderReadiness> {
        self.0.clone()
    }

    pub(super) fn accept(
        &mut self,
        current: u64,
        observed: u64,
        provider: ProviderReadiness,
    ) -> bool {
        if current != observed {
            return false;
        }
        let Some(row) = self.0.iter_mut().find(|row| row.kind == provider.kind) else {
            return false;
        };
        if row == &provider {
            return false;
        }
        *row = provider;
        true
    }
}

/// This is the explicitly finite compatibility answer for hosts predating
/// provider-readiness. It mirrors that binary's one managed registration;
/// local executable probing remains the independent authority for raw rows.
fn legacy(managed: bool, delegation: bool) -> Vec<ProviderReadiness> {
    koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS
        .iter()
        .map(|registration| {
            let is_legacy_provider =
                registration.kind == koru_session_profiles::CODEX_REGISTRATION.kind;
            ProviderReadiness {
                kind: registration.kind.parse().expect("built-in provider"),
                availability: if managed && is_legacy_provider {
                    A::Available { version: None }
                } else {
                    A::incompatible(ErrorCode::CapabilityNotNegotiated)
                },
                capabilities: CapabilitySet::new(if managed && delegation && is_legacy_provider {
                    vec![
                        capability::MANAGED_DELEGATION
                            .parse()
                            .expect("built-in capability"),
                    ]
                } else {
                    vec![]
                }),
                failure: None,
            }
        })
        .collect()
}

pub(super) fn unavailable() -> Vec<ProviderReadiness> {
    let mut rows = legacy(false, false);
    for row in &mut rows {
        row.availability = A::Failed {
            code: ErrorCode::HostUnavailable,
        };
    }
    rows
}

pub(super) fn subscribe<S: BackendSink>(
    client: &Arc<SessionClient>,
    generation: u64,
    sink: &S,
    internal: &SyncSender<BackendCommand>,
) -> Catalog {
    if !client.supports_provider_readiness() {
        let rows = legacy(
            client.host().agreement.version >= 8
                && client
                    .host()
                    .agreement
                    .capabilities
                    .supports(capability::MANAGED_SESSIONS),
            client.host().agreement.version >= koru_session_protocol::DELEGATION_PROTOCOL_VERSION
                && client
                    .host()
                    .agreement
                    .capabilities
                    .supports(capability::DELEGATION),
        );
        send(sink, BackendEvent::Providers(rows.clone()));
        return Catalog(rows);
    }
    let (rows, subscription) = match client.subscribe_providers() {
        Ok(result) => result,
        Err(_) => {
            let rows = unavailable();
            send(sink, BackendEvent::Providers(rows.clone()));
            return Catalog(rows);
        }
    };
    send(sink, BackendEvent::Providers(rows.clone()));
    let internal = internal.clone();
    let client = Arc::clone(client);
    let reader = std::thread::Builder::new()
        .name("koru-session-backend providers".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            loop {
                match subscription.recv_timeout(SUBSCRIPTION_WAIT) {
                    Ok(SubscriptionUpdate::Providers { provider }) => {
                        if internal
                            .send(BackendCommand::ProviderChanged {
                                generation,
                                provider,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(SubscriptionUpdate::Closed { .. }) => {
                        let _ = internal.send(BackendCommand::ProvidersLost { generation });
                        break;
                    }
                    Err(ClientError::TimedOut) => continue,
                    Err(_) => {
                        signal_connection_lost(&client, &internal, generation);
                        break;
                    }
                    Ok(_) => {}
                }
            }
        });
    if reader.is_err() {
        let failed = rows
            .into_iter()
            .map(|mut row| {
                row.availability = A::Failed {
                    code: ErrorCode::HostUnavailable,
                };
                row.capabilities = Default::default();
                row
            })
            .collect::<Vec<_>>();
        send(sink, BackendEvent::Providers(failed.clone()));
        return Catalog(failed);
    }
    Catalog(rows)
}

#[cfg(test)]
mod tests;
