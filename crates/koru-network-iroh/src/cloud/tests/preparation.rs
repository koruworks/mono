// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct Retained {
    provider: Provider,
    dropped: Arc<AtomicUsize>,
}
impl Authority for Retained {
    fn current(
        &self,
        credential: &koru_replication::cloud::SessionClaims,
        dataset: koru_device::cloud::CloudReplicaPrincipalV1,
    ) -> Result<CurrentAuthority, Refusal> {
        self.provider.current(credential, dataset)
    }
}
impl Drop for Retained {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn hello(v: &serde_json::Value) -> HandshakeV1 {
    HandshakeV1::ClientHello {
        version: koru_replication::cloud::Version1,
        dataset: provider(v).facts.dataset,
        client_nonce: koru_replication::cloud::Bytes([7; 32]),
        credential: v["credential"].as_str().unwrap().into(),
    }
}

#[tokio::test]
async fn signed_exchange_returns_the_retained_prepared_context() {
    let v = vectors();
    let p = provider(&v);
    let verifier = verifier();
    let dropped = Arc::new(AtomicUsize::new(0));
    let retained = Retained {
        provider: provider(&v),
        dropped: dropped.clone(),
    };
    let (client, server) = tokio::io::duplex(65536);
    let (mut client_reader, mut client_writer) = tokio::io::split(client);
    let (mut reader, mut writer) = tokio::io::split(server);
    let mut generations = ConnectionGenerations::default();
    let server = exchange_prepared(
        &mut writer,
        &mut reader,
        &verifier,
        |verified| async move {
            assert_eq!(verified.dataset(), retained.provider.facts.dataset);
            assert_eq!(
                verified.claims().principal,
                koru_replication::cloud::SessionPrincipal::Device {
                    device_id: retained.provider.claims.device_id
                }
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
            Ok((retained, std::future::pending::<Refusal>()))
        },
        p.claims.client_endpoint_id,
        p.facts.worker_endpoint,
        &mut generations,
        || 1_800_000_000_000,
    );
    let key = DeviceSigningKey::from_secret_bytes([3; 32]);
    let client = client_exchange(
        &mut client_writer,
        &mut client_reader,
        &verifier,
        v["credential"].as_str().unwrap().into(),
        p.facts.dataset,
        p.claims.client_endpoint_id,
        p.facts.worker_endpoint,
        p.facts.worker_endpoint,
        &key,
        0,
        || 1_800_000_000_000,
    );
    let (server, client) = tokio::join!(server, client);
    let (prepared, context) = server.unwrap();
    assert_eq!(prepared.generation, client.unwrap().generation);
    assert_eq!(prepared.admit_transfer(), Err(Refusal::SyncUnavailable));
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(context);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_credential_or_endpoint_never_calls_preparation() {
    for invalid_credential in [true, false] {
        let v = vectors();
        let p = provider(&v);
        let verifier = verifier();
        let mut message = hello(&v);
        if invalid_credential {
            let HandshakeV1::ClientHello { credential, .. } = &mut message else {
                unreachable!()
            };
            *credential = "invalid".into();
        }
        let remote = if invalid_credential {
            p.claims.client_endpoint_id
        } else {
            p.facts.worker_endpoint
        };
        let (mut client, server) = tokio::io::duplex(65536);
        client.write_all(&encode(&message).unwrap()).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(server);
        let mut generations = ConnectionGenerations::default();
        let calls = AtomicUsize::new(0);
        let result = exchange_prepared(
            &mut writer,
            &mut reader,
            &verifier,
            |_| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(Provider, std::future::Pending<Refusal>), _>(Refusal::SyncUnavailable)
            },
            remote,
            provider(&v).facts.worker_endpoint,
            &mut generations,
            || 1_800_000_000_000,
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        if invalid_credential {
            assert!(matches!(result, Err(Refusal::Credential(_))));
        } else {
            assert_eq!(result.err(), Some(Refusal::EndpointMismatch));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn stalled_preparation_consumes_the_original_deadline_and_drops_context() {
    let v = vectors();
    let p = provider(&v);
    let verifier = verifier();
    let dropped = Arc::new(AtomicUsize::new(0));
    let retained = Retained {
        provider: provider(&v),
        dropped: dropped.clone(),
    };
    let (mut client, server) = tokio::io::duplex(65536);
    client
        .write_all(&encode(&hello(&v)).unwrap())
        .await
        .unwrap();
    let (mut reader, mut writer) = tokio::io::split(server);
    let mut generations = ConnectionGenerations::default();
    let started = tokio::time::Instant::now();
    let result = exchange_prepared(
        &mut writer,
        &mut reader,
        &verifier,
        |_| async move {
            std::future::pending::<()>().await;
            Ok((retained, std::future::pending::<Refusal>()))
        },
        p.claims.client_endpoint_id,
        p.facts.worker_endpoint,
        &mut generations,
        || 1_800_000_000_000,
    )
    .await;
    assert_eq!(result.err(), Some(Refusal::Timeout));
    assert_eq!(started.elapsed(), Duration::from_secs(30));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn preparation_does_not_restart_the_finish_deadline() {
    let v = vectors();
    let p = provider(&v);
    let verifier = verifier();
    let dropped = Arc::new(AtomicUsize::new(0));
    let retained = Retained {
        provider: provider(&v),
        dropped: dropped.clone(),
    };
    let (mut client, server) = tokio::io::duplex(65536);
    client
        .write_all(&encode(&hello(&v)).unwrap())
        .await
        .unwrap();
    let (mut reader, mut writer) = tokio::io::split(server);
    let mut generations = ConnectionGenerations::default();
    let started = tokio::time::Instant::now();
    let result = exchange_prepared(
        &mut writer,
        &mut reader,
        &verifier,
        |_| async move {
            tokio::time::sleep(Duration::from_secs(20)).await;
            Ok((retained, std::future::pending::<Refusal>()))
        },
        p.claims.client_endpoint_id,
        p.facts.worker_endpoint,
        &mut generations,
        || 1_800_000_000_000,
    )
    .await;
    assert_eq!(result.err(), Some(Refusal::Timeout));
    assert_eq!(started.elapsed(), Duration::from_secs(30));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelling_after_challenge_drops_the_prepared_context() {
    let v = vectors();
    let p = provider(&v);
    let verifier = verifier();
    let dropped = Arc::new(AtomicUsize::new(0));
    let retained = Retained {
        provider: provider(&v),
        dropped: dropped.clone(),
    };
    let (mut client, server) = tokio::io::duplex(65536);
    client
        .write_all(&encode(&hello(&v)).unwrap())
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(server);
        let mut generations = ConnectionGenerations::default();
        exchange_prepared(
            &mut writer,
            &mut reader,
            &verifier,
            |_| async move { Ok((retained, std::future::pending::<Refusal>())) },
            p.claims.client_endpoint_id,
            p.facts.worker_endpoint,
            &mut generations,
            || 1_800_000_000_000,
        )
        .await
    });
    assert!(matches!(
        read_message(&mut client).await.unwrap(),
        HandshakeV1::WorkerChallenge { .. }
    ));
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    task.abort();
    assert!(task.await.err().unwrap().is_cancelled());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn observed_scope_fence_interrupts_idle_finish_and_releases_membership() {
    let v = vectors();
    let p = provider(&v);
    let verifier = verifier();
    let dropped = Arc::new(AtomicUsize::new(0));
    let retained = Retained {
        provider: provider(&v),
        dropped: dropped.clone(),
    };
    let (signal, fenced) = tokio::sync::oneshot::channel();
    let (mut client, server) = tokio::io::duplex(65536);
    client
        .write_all(&encode(&hello(&v)).unwrap())
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(server);
        let mut generations = ConnectionGenerations::default();
        exchange_prepared(
            &mut writer,
            &mut reader,
            &verifier,
            |_| async move {
                Ok((retained, async move {
                    fenced.await.unwrap_or(Refusal::AuthorizationUnavailable)
                }))
            },
            p.claims.client_endpoint_id,
            p.facts.worker_endpoint,
            &mut generations,
            || 1_800_000_000_000,
        )
        .await
    });
    assert!(matches!(
        read_message(&mut client).await.unwrap(),
        HandshakeV1::WorkerChallenge { .. }
    ));
    signal.send(Refusal::CloudDisabled).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.err(), Some(Refusal::CloudDisabled));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn credential_expiring_during_preparation_cannot_receive_a_challenge() {
    let v = vectors();
    let p = provider(&v);
    let verifier = verifier();
    let dropped = Arc::new(AtomicUsize::new(0));
    let retained = Retained {
        provider: provider(&v),
        dropped: dropped.clone(),
    };
    let clock = std::sync::atomic::AtomicI64::new(1_800_000_000_000);
    let now = &clock;
    let (mut client, server) = tokio::io::duplex(65536);
    client
        .write_all(&encode(&hello(&v)).unwrap())
        .await
        .unwrap();
    let (mut reader, mut writer) = tokio::io::split(server);
    let mut generations = ConnectionGenerations::default();
    let result = exchange_prepared(
        &mut writer,
        &mut reader,
        &verifier,
        |verified| async move {
            now.store(verified.claims().exp * 1000, Ordering::SeqCst);
            Ok((retained, std::future::pending::<Refusal>()))
        },
        p.claims.client_endpoint_id,
        p.facts.worker_endpoint,
        &mut generations,
        || now.load(Ordering::SeqCst),
    )
    .await;
    assert_eq!(
        result.err(),
        Some(Refusal::Credential(
            koru_account_session::TokenCapabilityVerificationError::CredentialExpired,
        ))
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    drop(writer);
    // split halves share the DuplexStream; retaining either half keeps the
    // peer's write side alive. Close both before asserting no challenge bytes.
    drop(reader);
    let mut output = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut output))
        .await
        .unwrap()
        .unwrap();
    assert!(output.is_empty());
}
