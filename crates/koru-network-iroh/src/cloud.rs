// SPDX-License-Identifier: MPL-2.0

//! Cloud transport and admission. The worker-only binder is separate from the
//! existing client endpoint; neither binding nor admission enables data transfer.

use koru_account_session::TokenCapabilityVerifier;
use koru_device::ReplicationEndpointId;
use koru_replication::cloud::{
    Authority, CLOUD_ALPN_V1, ClientHandshake, ConnectionGenerations, HandshakeV1,
    MAX_HANDSHAKE_BYTES, PendingHandshake, PreparedSession, Refusal, VerifiedClientHello, Version1,
    decode, encode,
};
use rand_core::{OsRng, RngCore};
use std::future::Future;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub mod client_io;
pub mod dial;
pub mod records;
pub mod session;
pub mod worker_endpoint;

/// A bounded retryable refusal before expensive admission. No credential or
/// dataset details are disclosed, and sibling streams remain untouched.
pub async fn refuse(writer: &mut iroh::endpoint::SendStream, reason: Refusal) {
    let Ok(bytes) = encode(&HandshakeV1::Refused {
        version: Version1,
        code: reason.code().into(),
    }) else {
        return;
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        writer.write_all(&bytes).await?;
        writer.finish()?;
        let _ = writer.stopped().await;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await;
}

async fn read_body<R: AsyncRead + Unpin>(
    reader: &mut R,
    prefix: [u8; 4],
) -> Result<HandshakeV1, Refusal> {
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_HANDSHAKE_BYTES {
        return Err(Refusal::Malformed);
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|_| Refusal::Malformed)?;
    decode(&body)
}

async fn read_message<R: AsyncRead + Unpin>(reader: &mut R) -> Result<HandshakeV1, Refusal> {
    let mut prefix = [0; 4];
    // A stream that ends before its first byte is the peer declining to
    // speak, which is a different fault from a message that would not parse.
    reader.read_exact(&mut prefix).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            Refusal::Closed
        } else {
            Refusal::Malformed
        }
    })?;
    read_body(reader, prefix).await
}

/// The caller owns current in-memory authority and the connection's monotonic
/// generation allocator. `now_millis` is an injected wall clock; whole-handshake
/// timeout independently uses Tokio's monotonic clock, including the final write.
pub async fn accept(
    connection: &iroh::endpoint::Connection,
    writer: &mut iroh::endpoint::SendStream,
    reader: &mut iroh::endpoint::RecvStream,
    verifier: &TokenCapabilityVerifier,
    authority: &dyn Authority,
    local_endpoint: ReplicationEndpointId,
    generations: &mut ConnectionGenerations,
    now_millis: impl Fn() -> i64,
) -> Result<PreparedSession, Refusal> {
    if connection.alpn() != CLOUD_ALPN_V1 {
        return Err(Refusal::PrincipalNotAllowed);
    }
    let remote = ReplicationEndpointId::from_bytes(*connection.remote_id().as_bytes());
    let result = exchange(
        writer,
        reader,
        verifier,
        authority,
        remote,
        local_endpoint,
        generations,
        now_millis,
    )
    .await;
    if result.is_err() {
        let _ = writer.reset(1u32.into());
        let _ = reader.stop(1u32.into());
    }
    result
}

/// Prepare an owned current-authority context from verified ClientHello inside
/// the same whole-handshake deadline. Retain the returned context through the
/// serving stream (for example, its cohort membership). The callback supplies
/// that context and the scoped fence future. The fence races challenge/finish/
/// acceptance, including idle waits. The host must also fence preparation and
/// the complete serving lifetime with session::with_fence. D231 withdrew the
/// worker route attestation, so the context carries no proof to present: the
/// owned authority's own live lease and epoch are the whole worker-side check.
/// Neither the callback nor success enables data transfer.
pub async fn accept_prepared<A, Prepare, Prepared, Fenced>(
    connection: &iroh::endpoint::Connection,
    writer: &mut iroh::endpoint::SendStream,
    reader: &mut iroh::endpoint::RecvStream,
    verifier: &TokenCapabilityVerifier,
    prepare: Prepare,
    local_endpoint: ReplicationEndpointId,
    generations: &mut ConnectionGenerations,
    now_millis: impl Fn() -> i64,
) -> Result<(PreparedSession, A), Refusal>
where
    A: Authority,
    Prepare: FnOnce(VerifiedClientHello) -> Prepared,
    Prepared: Future<Output = Result<(A, Fenced), Refusal>>,
    Fenced: Future<Output = Refusal>,
{
    if connection.alpn() != CLOUD_ALPN_V1 {
        return Err(Refusal::PrincipalNotAllowed);
    }
    let remote = ReplicationEndpointId::from_bytes(*connection.remote_id().as_bytes());
    let mut refused = false;
    let result = exchange_answering(
        writer,
        reader,
        verifier,
        prepare,
        remote,
        local_endpoint,
        generations,
        now_millis,
        &mut refused,
    )
    .await;
    // A refusal the client was told is left for it to read: a reset would
    // discard it unread. Every other failure resets, as before.
    if result.is_err() && !(refused && delivered(writer).await) {
        let _ = writer.reset(1u32.into());
        let _ = reader.stop(1u32.into());
    }
    result
}

/// Finish the send side after a written `Refused` and wait, as [`refuse`]
/// does, until the client has acknowledged every byte. A QUIC reset discards
/// data its peer has not read yet, so the reset that followed used to lose
/// the refusal it followed (Cloud MCP Q2.3-11). Nothing on the wire changes:
/// the message is the one the client already decodes, and the stream then
/// ends. `false` if it was not acknowledged within the bound.
async fn delivered(writer: &mut iroh::endpoint::SendStream) -> bool {
    if writer.finish().is_err() {
        return false;
    }
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(1), writer.stopped()).await,
        Ok(Ok(None))
    )
}

struct BorrowedAuthority<'a>(&'a dyn Authority);
impl Authority for BorrowedAuthority<'_> {
    fn current(
        &self,
        credential: &koru_replication::cloud::SessionClaims,
        dataset: koru_device::cloud::CloudReplicaPrincipalV1,
    ) -> Result<koru_replication::cloud::CurrentAuthority, Refusal> {
        self.0.current(credential, dataset)
    }
}

async fn exchange<W: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    writer: &mut W,
    reader: &mut R,
    verifier: &TokenCapabilityVerifier,
    authority: &dyn Authority,
    remote: ReplicationEndpointId,
    local: ReplicationEndpointId,
    generations: &mut ConnectionGenerations,
    now_millis: impl Fn() -> i64,
) -> Result<PreparedSession, Refusal> {
    exchange_prepared(
        writer,
        reader,
        verifier,
        |_| async {
            Ok((
                BorrowedAuthority(authority),
                std::future::pending::<Refusal>(),
            ))
        },
        remote,
        local,
        generations,
        now_millis,
    )
    .await
    .map(|(session, _)| session)
}

async fn exchange_prepared<W, R, A, Prepare, Prepared, Fenced>(
    writer: &mut W,
    reader: &mut R,
    verifier: &TokenCapabilityVerifier,
    prepare: Prepare,
    remote: ReplicationEndpointId,
    local: ReplicationEndpointId,
    generations: &mut ConnectionGenerations,
    now_millis: impl Fn() -> i64,
) -> Result<(PreparedSession, A), Refusal>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
    A: Authority,
    Prepare: FnOnce(VerifiedClientHello) -> Prepared,
    Prepared: Future<Output = Result<(A, Fenced), Refusal>>,
    Fenced: Future<Output = Refusal>,
{
    exchange_answering(
        writer,
        reader,
        verifier,
        prepare,
        remote,
        local,
        generations,
        now_millis,
        &mut false,
    )
    .await
}

/// [`exchange_prepared`], also saying whether the worker's refusal was
/// written to the client in full (`refused`).
#[allow(clippy::too_many_arguments)]
async fn exchange_answering<W, R, A, Prepare, Prepared, Fenced>(
    writer: &mut W,
    reader: &mut R,
    verifier: &TokenCapabilityVerifier,
    prepare: Prepare,
    remote: ReplicationEndpointId,
    local: ReplicationEndpointId,
    generations: &mut ConnectionGenerations,
    now_millis: impl Fn() -> i64,
    refused: &mut bool,
) -> Result<(PreparedSession, A), Refusal>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
    A: Authority,
    Prepare: FnOnce(VerifiedClientHello) -> Prepared,
    Prepared: Future<Output = Result<(A, Fenced), Refusal>>,
    Fenced: Future<Output = Refusal>,
{
    // The approved deadline starts at receipt of the first length byte, not at
    // connection creation. EOF before that byte creates no challenge or session.
    let first = reader.read_u8().await.map_err(|_| Refusal::Malformed)?;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let started = tokio::time::Instant::now();
        let mut prefix = [first, 0, 0, 0];
        reader
            .read_exact(&mut prefix[1..])
            .await
            .map_err(|_| Refusal::Malformed)?;
        let hello = read_body(reader, prefix).await?;
        let HandshakeV1::ClientHello { dataset, .. } = &hello else {
            return Err(Refusal::Malformed);
        };
        let dataset = *dataset;
        let verified =
            VerifiedClientHello::verify(verifier, CLOUD_ALPN_V1, &hello, remote, now_millis())?;
        // A refusal the worker decides after the hello verified is the
        // worker's answer, and the client is told it: without this the
        // stream was only reset, and every dataset a worker could not serve
        // read on the desktop as "malformed". Once it is written in full,
        // the stream's owner lets the client read it before the stream ends.
        let (authority, fenced) = match prepare(verified).await {
            Ok(prepared) => prepared,
            Err(reason) => {
                if let Ok(bytes) = encode(&HandshakeV1::Refused {
                    version: Version1,
                    code: reason.code().to_owned(),
                }) {
                    *refused =
                        writer.write_all(&bytes).await.is_ok() && writer.flush().await.is_ok();
                }
                return Err(reason);
            }
        };
        let finish = async {
            let mut nonce = [0; 32];
            OsRng.fill_bytes(&mut nonce);
            let (pending, challenge) = PendingHandshake::begin(
                verifier,
                &authority,
                CLOUD_ALPN_V1,
                hello,
                remote,
                local,
                nonce,
                now_millis(),
                0,
            )?;
            writer
                .write_all(&encode(&challenge)?)
                .await
                .map_err(|_| Refusal::Malformed)?;
            let finish = read_message(reader).await?;
            let mut session_id = [0; 32];
            OsRng.fill_bytes(&mut session_id);
            let prepared = pending.finish(
                &authority,
                finish,
                now_millis(),
                started.elapsed().as_millis() as u64,
                session_id,
                generations.next(dataset)?,
            )?;
            writer
                .write_all(&encode(&prepared.accepted)?)
                .await
                .map_err(|_| Refusal::Malformed)?;
            writer.flush().await.map_err(|_| Refusal::Malformed)?;
            Ok::<_, Refusal>(prepared)
        };
        let prepared = tokio::select! {
            biased;
            reason = fenced => return Err(reason),
            result = finish => result?,
        };
        Ok((prepared, authority))
    })
    .await
    .map_err(|_| Refusal::Timeout)?
}

/// Authenticate a replacement stream on the same QUIC connection. This helper
/// neither creates an endpoint nor dials a connection. The caller switches work
/// only after the returned receipt and keeps the former stream until drained.
///
/// `route_endpoint` is `endpoint_id` of the `CloudSyncRouteV1` the API issued
/// for this dataset. Since D231 withdrew the worker route attestation it is the
/// device's only check on the worker, and a connection that authenticated as a
/// different endpoint is refused `endpoint_mismatch` before `ClientHelloV1`.
pub async fn authenticate(
    connection: &iroh::endpoint::Connection,
    writer: &mut iroh::endpoint::SendStream,
    reader: &mut iroh::endpoint::RecvStream,
    verifier: &TokenCapabilityVerifier,
    credential: String,
    dataset: koru_device::cloud::CloudReplicaPrincipalV1,
    local_endpoint: ReplicationEndpointId,
    route_endpoint: ReplicationEndpointId,
    device_key: &koru_device::DeviceSigningKey,
    previous_generation: u64,
    now_millis: impl Fn() -> i64,
) -> Result<PreparedSession, Refusal> {
    if connection.alpn() != CLOUD_ALPN_V1 {
        return Err(Refusal::PrincipalNotAllowed);
    }
    let remote = ReplicationEndpointId::from_bytes(*connection.remote_id().as_bytes());
    let result = client_exchange(
        writer,
        reader,
        verifier,
        credential,
        dataset,
        local_endpoint,
        remote,
        route_endpoint,
        device_key,
        previous_generation,
        now_millis,
    )
    .await;
    if result.is_err() {
        let _ = writer.reset(1u32.into());
        let _ = reader.stop(1u32.into());
    }
    result
}

async fn client_exchange<W: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    writer: &mut W,
    reader: &mut R,
    verifier: &TokenCapabilityVerifier,
    credential: String,
    dataset: koru_device::cloud::CloudReplicaPrincipalV1,
    local: ReplicationEndpointId,
    remote: ReplicationEndpointId,
    route_endpoint: ReplicationEndpointId,
    device_key: &koru_device::DeviceSigningKey,
    previous_generation: u64,
    now_millis: impl Fn() -> i64,
) -> Result<PreparedSession, Refusal> {
    let mut nonce = [0; 32];
    OsRng.fill_bytes(&mut nonce);
    let (pending, hello) = ClientHandshake::begin(
        verifier,
        credential,
        dataset,
        local,
        remote,
        route_endpoint,
        nonce,
        now_millis(),
    )?;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        writer
            .write_all(&encode(&hello)?)
            .await
            .map_err(|_| Refusal::Malformed)?;
        let challenge = read_message(reader).await?;
        let (receipt, finish) = pending.challenge(challenge, device_key, now_millis())?;
        writer
            .write_all(&encode(&finish)?)
            .await
            .map_err(|_| Refusal::Malformed)?;
        receipt.accepted(
            read_message(reader).await?,
            now_millis(),
            previous_generation,
        )
    })
    .await
    .map_err(|_| Refusal::Timeout)?
}

#[cfg(test)]
mod tests;
