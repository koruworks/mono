// SPDX-License-Identifier: MPL-2.0

//! One authenticated metadata read. No transfer payload, Store job, subscription,
//! upload, import, or adoption is admitted. The caller still owns the complete
//! session::with_fence lifetime and rechecks account/lifecycle before use.
use super::*;

/// Original JoinResponse bytes, bound to the admitted principal and exact unit.
/// This proves a sampled version only, never permission to edit or a receipt.
pub struct ObservedVersion {
    scope: Scope,
    unit: Unit,
    version: Vec<u8>,
    _charge: Reservation,
}
impl std::fmt::Debug for ObservedVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObservedVersion(<redacted>)")
    }
}
impl ObservedVersion {
    /// Compare the inventory digest against original Cloud bytes, and compare
    /// the cold local state semantically. Re-encoding is never head evidence.
    pub fn matches(&self, scope: Scope, unit: Unit, head: [u8; 32], local_version: &[u8]) -> bool {
        self.scope.principal == scope.principal
            && self.scope.profile == scope.profile
            && self.scope.eligibility_workspace == scope.eligibility_workspace
            && self.scope.manifest == scope.manifest
            && self.unit == unit
            && koru_store_core::sync_digest::revision(&self.version) == head
            && loro::VersionVector::decode(&self.version)
                .ok()
                .zip(loro::VersionVector::decode(local_version).ok())
                .is_some_and(|(remote, local)| remote == local)
    }
}

/// Open only a metadata probe on an already authenticated, fenced carrier.
/// The stream closes immediately after the one exact response. Passing the
/// detached local VV avoids unnecessary remote payload work; any attempted
/// payload/start is nevertheless refused before admission.
pub async fn inspect_version<'a, R, W, A>(
    reader: R,
    writer: W,
    config: Config,
    unit: Unit,
    local_version: Vec<u8>,
    authorize: A,
) -> Result<ObservedVersion, Error>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
    A: FnMut(Unit) -> Result<(), Refusal> + Send + 'a,
{
    let client = Client::new(
        reader,
        writer,
        config,
        |_| -> Result<std::future::Ready<Result<Receipt, Error>>, Error> { Err(Error::Invalid) },
        authorize,
        |_| Decision::Defer,
    )?;
    tokio::time::timeout(Duration::from_secs(5), probe(client, unit, local_version))
        .await
        .map_err(|_| Error::Timeout)?
}

async fn probe<'a, R, W>(
    mut client: Client<'a, R, W>,
    unit: Unit,
    local_version: Vec<u8>,
) -> Result<ObservedVersion, Error>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
{
    client.inspection_only = true;
    client.scope.object_unit(unit)?;
    (client.authorize)(unit).map_err(Error::Authority)?;
    loro::VersionVector::decode(&local_version).map_err(|_| Error::Invalid)?;
    let mut version = Some(local_version);
    loop {
        if client.available_writer()
            && let Some(version) = version.take()
        {
            client.join(unit, version)?;
        }
        match client.next().await? {
            Event::Progress => {}
            Event::Metadata(mut messages) if version.is_none() => {
                let Metadata::Joined {
                    unit: received,
                    version,
                    live: false,
                } = messages.values.pop().ok_or(Error::Invalid)?
                else {
                    return Err(Error::Invalid);
                };
                if received != unit || !messages.values.is_empty() {
                    return Err(Error::Invalid);
                }
                (client.authorize)(unit).map_err(Error::Authority)?;
                let observed = ObservedVersion {
                    scope: client.scope,
                    unit,
                    version,
                    _charge: messages._charge,
                };
                client.cancel(Error::Closed);
                return Ok(observed);
            }
            _ => return Err(Error::Invalid),
        }
    }
}

#[cfg(test)]
mod tests;
