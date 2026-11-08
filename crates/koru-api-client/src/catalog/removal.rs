// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_client::http::{
    BeginProfileRemoval, ProfileRemovalAttempt, RemovalKind, RemovalMark,
};
use koru_store_core::ids::{ProfileId, RemovalAttemptId};

impl CatalogClient<'_> {
    pub fn profile_removal(&self, id: ProfileId) -> WorkspaceResult<Option<RemovalMark>> {
        self.client
            .store()
            .get_public_json(&format!("/v1/profiles/{id}/removal"))
            .map_err(map_public)
    }

    pub fn begin_profile_removal(
        &self,
        id: ProfileId,
        kind: RemovalKind,
    ) -> WorkspaceResult<RemovalMark> {
        self.client
            .store()
            .post_public_json(
                &format!("/v1/profiles/{id}/removal"),
                &BeginProfileRemoval { kind },
                RetryPolicy::AuthenticationOnly,
            )
            .map_err(map_public)
    }

    pub fn stop_profile_removal(
        &self,
        id: ProfileId,
        attempt: RemovalAttemptId,
    ) -> WorkspaceResult<()> {
        self.client
            .store()
            .post_public_json(
                &format!("/v1/profiles/{id}/removal/stop"),
                &ProfileRemovalAttempt { attempt },
                RetryPolicy::AuthenticationOnly,
            )
            .map_err(map_public)
    }

    pub fn keep_profile(&self, id: ProfileId, attempt: RemovalAttemptId) -> WorkspaceResult<()> {
        self.client
            .store()
            .delete_public_json(
                &format!("/v1/profiles/{id}/removal"),
                &ProfileRemovalAttempt { attempt },
                RetryPolicy::AuthenticationOnly,
            )
            .map_err(map_public)
    }
}
