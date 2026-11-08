// SPDX-License-Identifier: MPL-2.0

use super::ServiceError;
use koru_account_runtime::account::lifecycle::{LocalProfileIdentity, LocalProfileInventory};
use koru_api_client::ProfileLifecycleInventory;
use std::path::PathBuf;
/// Production adapter. It connects only when reconciliation is requested, on
/// the account worker thread; it never starts a Gateway recursively.
pub struct GatewayProfileInventory {
    root: Option<PathBuf>,
    authorization_root: PathBuf,
}

impl GatewayProfileInventory {
    pub fn new(paths: &crate::Paths) -> Self {
        Self {
            root: paths.explicit_root().map(std::path::Path::to_path_buf),
            authorization_root: paths.root().to_path_buf(),
        }
    }
}

impl LocalProfileInventory for GatewayProfileInventory {
    fn provision(
        &self,
        profile: koru_account_core::ProfileId,
        initial: &koru_api_client::ProvisionAccountProfile,
    ) -> Result<(), ServiceError> {
        let client =
            koru_api_client::Client::connect_at(crate::segment(), &self.authorization_root)
                .map_err(|_| ServiceError::retryable("profile_provisioning_unavailable"))?;
        client
            .store()
            .provision_account_profile(profile, initial)
            .map(|_| ())
            .map_err(|error| match error.code() {
                koru_store_core::errors::Code::ProfileAccountConflict
                | koru_store_core::errors::Code::ProfileRemoved => {
                    ServiceError::terminal("profile_not_authorized")
                }
                _ => ServiceError::retryable("profile_provisioning_unavailable"),
            })
    }

    fn inventory(&self) -> Result<ProfileLifecycleInventory, ServiceError> {
        let client = match &self.root {
            Some(root) => koru_api_client::Client::open_at(koru_channel::segment(), root),
            None => koru_api_client::Client::open(koru_channel::segment()),
        }
        .map_err(|_| ServiceError::retryable("store_unavailable"))?;
        client
            .store()
            .profile_lifecycle_inventory()
            .map_err(|error| match error.code() {
                koru_store_core::errors::Code::CorruptState => {
                    ServiceError::terminal("profile_account_binding_corrupt")
                }
                _ => ServiceError::retryable("store_unavailable"),
            })
    }
    /// The local Profile catalog, which is the projection of each Profile's own
    /// identity document: `commit_profile_identity` writes the document and this
    /// row in one transaction, so a rename that arrived from another device is
    /// here as soon as it is durable, and `profile.renamed` has already told the
    /// desktop to look again.
    ///
    /// A Profile that account provisioning created locally but whose document has
    /// not arrived is in this list too, carrying the very name the account's copy
    /// carries — provisioning seeded it from there. So preferring this over the
    /// account's row is never worse than the account's row, and is right the
    /// moment a document exists. A Profile with no local row at all is absent,
    /// and its caller falls back.
    fn identities(&self) -> Result<Vec<LocalProfileIdentity>, ServiceError> {
        let client = match &self.root {
            Some(root) => koru_api_client::Client::open_at(koru_channel::segment(), root),
            None => koru_api_client::Client::open(koru_channel::segment()),
        }
        .map_err(|_| ServiceError::retryable("store_unavailable"))?;
        let profiles = client
            .store()
            .profiles()
            .map_err(|_| ServiceError::retryable("store_unavailable"))?;
        Ok(profiles
            .into_iter()
            .map(|profile| LocalProfileIdentity {
                profile_id: profile.id,
                name: profile.name,
                // `und` is the absence of an answer rather than an answer, so
                // it does not displace whatever the account was told once.
                locale: match profile.locale.as_str() {
                    koru_store_core::Locale::UNDETERMINED => None,
                    tag => Some(tag.to_owned()),
                },
            })
            .collect())
    }

    fn authorize(
        &self,
        profile: koru_account_core::ProfileId,
        identity: koru_account_core::KoruIdentityId,
    ) -> Result<bool, ServiceError> {
        let binding =
            koru_store_core::ProfileAccountBinding::from_identity_uuid(identity.as_uuid())
                .map_err(|_| ServiceError::terminal("profile_not_authorized"))?
                .to_storage();
        let client =
            koru_api_client::Client::connect_at(crate::segment(), &self.authorization_root)
                .map_err(|_| ServiceError::retryable("enrollment_unavailable"))?;
        let inventory = client
            .store()
            .profile_lifecycle_inventory()
            .map_err(|_| ServiceError::retryable("enrollment_unavailable"))?;
        Ok(inventory.profiles.iter().any(|candidate| {
            candidate.profile_id == profile
                && candidate.account_binding == binding
                && candidate.removed_at_millis.is_none()
        }))
    }
}
