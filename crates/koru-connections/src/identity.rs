// SPDX-License-Identifier: MPL-2.0

//! The digests ai.db stores about a connection, the credential slot one
//! connection owns, and the one dispatcher identity of this process.
//!
//! Every digest is BLAKE3 over a domain and length-prefixed parts, so two
//! different lists of parts never hash the same bytes. None is a digest of a
//! secret.

use std::sync::OnceLock;

use koru_ai_core::connections::{ConnectionDigest, ConnectionId, ProviderToken};
use koru_delegate_core::PrivateDispatcherId;
use koru_oauth_credentials::{CredentialContext, CredentialEntry, Error as CredentialError};

use crate::vendor::{ItemIdentityDigest, VendorItemRef};

fn digest(domain: &str, parts: &[&[u8]]) -> ConnectionDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(domain.len() as u64).to_le_bytes());
    hasher.update(domain.as_bytes());
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    *hasher.finalize().as_bytes()
}

/// `connections.account_subject_digest`: the vendor's stable account
/// identifier, which is not stored itself.
pub fn account_subject_digest(provider: ProviderToken, subject: &str) -> ConnectionDigest {
    digest(
        "koru.connection.account.v1",
        &[provider.as_str().as_bytes(), subject.as_bytes()],
    )
}

/// `connection_items.item_ref_digest`.
pub fn item_ref_digest(provider: ProviderToken, item: &VendorItemRef) -> ConnectionDigest {
    digest(
        "koru.connection.item.v1",
        &[provider.as_str().as_bytes(), item.as_str().as_bytes()],
    )
}

/// `connection_items.identity_digest` (D17): the vendor's item identifier and
/// the vendor's identifier of the item's owning account. An adapter that
/// cannot read the owner passes `None`; the check can then never fail.
pub fn item_identity_digest(
    provider: ProviderToken,
    item: &VendorItemRef,
    owning_account: Option<&str>,
) -> ItemIdentityDigest {
    ItemIdentityDigest(digest(
        "koru.connection.item-identity.v1",
        &[
            provider.as_str().as_bytes(),
            item.as_str().as_bytes(),
            &[u8::from(owning_account.is_some())],
            owning_account.unwrap_or_default().as_bytes(),
        ],
    ))
}

/// The actor a connection's credential is scoped to. The prefix keeps it
/// apart from every other owner of the same keychain service.
fn slot_actor(connection: ConnectionId) -> String {
    format!("koru-bots-v1:{}", connection.to_storage())
}

/// The one credential slot of one connection: the installation's entry for
/// the vendor's origin, scoped to this connection. It is never the bare
/// origin entry, which another owner of the same origin (a document importer
/// signed in to the same vendor) keeps its own token in.
pub fn connection_slot(
    context: &CredentialContext,
    service_origin: &str,
    connection: ConnectionId,
) -> Result<CredentialEntry, CredentialError> {
    context
        .entry(service_origin)?
        .for_provider_actor(&slot_actor(connection))
}

/// `connections.credential_slot_digest`: a digest of the slot's account
/// name. It detects a changed credential context (another root, channel or
/// device), so the service reports a missing credential instead of reading
/// another slot.
pub fn credential_slot_digest(slot: &CredentialEntry) -> ConnectionDigest {
    digest("koru.connection.slot.v1", &[slot.account().as_bytes()])
}

/// The fingerprint of a connection command's arguments: 64 lowercase hex.
pub(crate) fn command_fingerprint(kind: &str, parts: &[&[u8]]) -> String {
    let mut all: Vec<&[u8]> = vec![kind.as_bytes()];
    all.extend_from_slice(parts);
    blake3::Hash::from_bytes(digest("koru.connection.command.v1", &all))
        .to_hex()
        .to_string()
}

static DISPATCHER: OnceLock<PrivateDispatcherId> = OnceLock::new();

/// **The one dispatcher identity of this process**, minted on first use and
/// never again: two services in one process carry the same id. It is on every
/// effect row this process opens and on every private command it sends, and
/// it is what lets a later process close what this one left.
pub fn process_dispatcher() -> PrivateDispatcherId {
    *DISPATCHER.get_or_init(PrivateDispatcherId::mint)
}
