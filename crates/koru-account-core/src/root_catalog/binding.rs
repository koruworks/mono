// SPDX-License-Identifier: MPL-2.0

//! The account's one catalog binding (TECHNICAL.final §4.1 "Account
//! bootstrap atomically gets/creates the account's catalog binding. A third
//! linked device does not rename that binding"; §3.3 "Multiple verified
//! provider identities can map to one canonical Koru account; email equality
//! or a CRDT field alone is never merge authority").
//!
//! The rule is a pure function over what the service already holds for the
//! account and what the device declares it holds. The service runs it inside
//! the transaction that inserts the binding; a lost insert race is answered
//! by reading the row that won, which is the same answer the loser would
//! have computed had it read first. [`BindingTable`] is that insert-or-read
//! rule in memory, the reference every storage implementation is measured
//! against.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{AuthIdentityId, CatalogScopeId, KoruIdentityId};

/// The binding as the service stores it and answers it. `binding_generation`
/// starts at 1 and moves only if the service ever has to re-mint a scope,
/// which nothing in this plan does; it exists so a later fence has a number
/// to compare and a client never has to compare scope ids by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCatalogBinding {
    pub account_id: KoruIdentityId,
    pub catalog_scope_id: CatalogScopeId,
    pub bound_at_ms: i64,
    pub binding_generation: u64,
}

/// What a device already records as its account-scope binding
/// (`Store::bind_account_scope`), declared so the service can refuse a
/// device that belongs to another account before it reads or writes
/// anything. A device with no binding yet declares nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredBinding {
    pub account_id: KoruIdentityId,
    pub catalog_scope_id: CatalogScopeId,
}

/// One get-or-create request, as the session establishes it. The account
/// and the identity come from the verified session, never from a body; the
/// declaration is the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindingRequest {
    pub account_id: KoruIdentityId,
    /// Which verified identity of the account is asking. Carried for the
    /// audit line only: the rule never reads it, which is how several
    /// identities of one account get one binding.
    pub authenticated_by: AuthIdentityId,
    pub declared: Option<DeclaredBinding>,
    pub now_ms: i64,
}

/// What the service does with the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingDecision {
    /// The account is bound; answer this, unchanged. Idempotent.
    Existing(AccountCatalogBinding),
    /// Insert this binding. If the insert loses to a concurrent first
    /// sign-in, read the row that won and answer `Existing` with it: the
    /// scope id is whichever insert committed, never the loser's.
    Create(AccountCatalogBinding),
}

impl BindingDecision {
    pub fn binding(self) -> AccountCatalogBinding {
        match self {
            Self::Existing(binding) | Self::Create(binding) => binding,
        }
    }
}

/// Why a binding is refused. None of these merges anything: a refused device
/// keeps its local work and its local binding exactly as they were.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingRefusal {
    /// The device's catalog is bound to another account (§4.1 "Two
    /// already-bound different accounts cannot join through ordinary
    /// sign-in/pairing"). Separate accounts stay separate.
    AccountConflict {
        declared: KoruIdentityId,
        session: KoruIdentityId,
    },
    /// The device declares this account's scope under a different id than
    /// the one the service holds — or one the service never minted. A third
    /// device never renames the binding, and a device never invents one.
    ScopeMismatch {
        declared: CatalogScopeId,
        bound: Option<CatalogScopeId>,
    },
    /// The row the service holds for the account names another account.
    /// Storage violated its own key; nothing is answered from it.
    Integrity,
}

impl BindingRefusal {
    /// Stable lowercase wire codes; every transport serializes them unchanged.
    pub const fn code(self) -> &'static str {
        match self {
            Self::AccountConflict { .. } => "catalog_binding_conflict",
            Self::ScopeMismatch { .. } => "catalog_binding_mismatch",
            Self::Integrity => "catalog_binding_corrupt",
        }
    }
}

/// The rule. `existing` is the account's row if the service holds one;
/// `mint` is called at most once, only when a binding is to be created.
pub fn decide_binding(
    existing: Option<AccountCatalogBinding>,
    request: &BindingRequest,
    mint: impl FnOnce() -> CatalogScopeId,
) -> Result<BindingDecision, BindingRefusal> {
    if let Some(declared) = request.declared
        && declared.account_id != request.account_id
    {
        return Err(BindingRefusal::AccountConflict {
            declared: declared.account_id,
            session: request.account_id,
        });
    }
    match existing {
        Some(bound) => {
            if bound.account_id != request.account_id {
                return Err(BindingRefusal::Integrity);
            }
            if let Some(declared) = request.declared
                && declared.catalog_scope_id != bound.catalog_scope_id
            {
                return Err(BindingRefusal::ScopeMismatch {
                    declared: declared.catalog_scope_id,
                    bound: Some(bound.catalog_scope_id),
                });
            }
            Ok(BindingDecision::Existing(bound))
        }
        None => {
            if let Some(declared) = request.declared {
                // The service never held a binding for this account, yet
                // the device says it does: the device cannot be the authority
                // for a scope the service did not mint.
                return Err(BindingRefusal::ScopeMismatch {
                    declared: declared.catalog_scope_id,
                    bound: None,
                });
            }
            Ok(BindingDecision::Create(AccountCatalogBinding {
                account_id: request.account_id,
                catalog_scope_id: mint(),
                bound_at_ms: request.now_ms,
                binding_generation: 1,
            }))
        }
    }
}

/// The insert-or-read rule over one map keyed by account: the reference
/// implementation of what the service's unique key on `account_id` does.
/// Memory only; the service's table is PostgreSQL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BindingTable {
    rows: BTreeMap<KoruIdentityId, AccountCatalogBinding>,
}

impl BindingTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, account_id: KoruIdentityId) -> Option<AccountCatalogBinding> {
        self.rows.get(&account_id).copied()
    }

    /// One get-or-create under the rule. A `Create` whose key is already
    /// taken by the time it is applied — the race — answers the row that
    /// won, exactly as the SQL path reads back after a refused insert.
    pub fn get_or_create(
        &mut self,
        request: &BindingRequest,
        mint: impl FnOnce() -> CatalogScopeId,
    ) -> Result<AccountCatalogBinding, BindingRefusal> {
        let decision = decide_binding(self.get(request.account_id), request, mint)?;
        Ok(self.apply(decision))
    }

    /// Apply a decision computed earlier, possibly against a stale read.
    pub fn apply(&mut self, decision: BindingDecision) -> AccountCatalogBinding {
        match decision {
            BindingDecision::Existing(binding) => binding,
            BindingDecision::Create(binding) => {
                *self.rows.entry(binding.account_id).or_insert(binding)
            }
        }
    }
}
