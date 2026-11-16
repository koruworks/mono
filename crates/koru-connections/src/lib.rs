// SPDX-License-Identifier: MPL-2.0

//! A person's private connections (M5).
//!
//! A **connection** is one signed-in account at one vendor. This crate owns
//! three things and names no vendor in any of them:
//!
//! - **the vendor port** ([`ConnectionVendor`]): authorize, refresh, read one
//!   item, revoke. One adapter crate per qualified vendor implements it. None
//!   exists yet: a real adapter waits for a service to be chosen and
//!   qualified. Everything here is verified against the scripted vendor in
//!   [`support`];
//! - **the credential slot** ([`connection_slot`]): one entry per connection
//!   in the existing credential store, scoped so it can never be the slot
//!   another owner of the same vendor origin uses. Only the refresh token is
//!   stored; ai.db holds a digest of the slot's name and nothing else;
//! - **the service** ([`ConnectionService`]): connect, sign in again, choose
//!   more files, disconnect, and the one read of one picked item, performed
//!   under the obligations of the delegate's effect ledger.
//!
//! The service is the only code that holds a secret, and only while it uses
//! one. It reaches ai.db, the delegate, the credential store and the clock
//! through the ports in [`ports`], so nothing here opens a database, a
//! socket, a browser or an OS keychain. The loopback listener that receives
//! the vendor's redirect belongs to whoever composes the service: it passes
//! the redirect address to a `begin` call and the callback to
//! [`ConnectionService::complete_trip`].
//!
//! **Not here:** what a Bot is granted (its policy, in ai.db), the question a
//! person is asked (the delegate's decision), and the model's tool. A grant
//! removed, or an account disconnected, reaches a read through the two
//! replies the delegate gives: a refused dispatch mark, and `deliver: false`.

mod identity;
pub mod ports;
mod service;
mod vendor;

#[cfg(any(test, feature = "test-support"))]
pub mod support;

#[cfg(test)]
mod tests;

pub use identity::{
    account_subject_digest, connection_slot, credential_slot_digest, item_identity_digest,
    item_ref_digest, process_dispatcher,
};
pub use ports::{
    Clock, ConnectionChange, ConnectionChangeSink, ConnectionChanged, ConnectionStore,
    EffectLedger, LedgerError, NoConnectionChangeSink, PresentCaller, ReadCaller, SystemClock,
};
pub use service::{
    ADVANCE_BACKOFF_FIRST, ADVANCE_BACKOFF_MAX, AUTHORIZE_DEADLINE, CONSENT_WAIT, ConnectionPorts,
    ConnectionService, Disconnected, PrivateReadContent, REVOKE_DEADLINE, ReadOutcome, Recovered,
    TripStart, VENDOR_READ_DEADLINE,
};
pub use vendor::{
    AccessToken, AuthorizationCallback, AuthorizationRequest, AuthorizedAccount, ConnectionVendor,
    ItemContent, ItemIdentityDigest, PickedItem, READ_LIMIT_BYTES, RefreshedAccess, RevokeOutcome,
    VendorError, VendorItemRef,
};
