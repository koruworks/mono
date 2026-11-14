// SPDX-License-Identifier: MPL-2.0

//! Single bounded reads, called only on the account worker. A caller that
//! paginates must restart on `usage_revision_changed`, never combine epochs.
use super::*;
use koru_account_wire::cloud_plan::{
    CloudPlanResponseV1, CloudUsageWorkspacesQueryV1, CloudUsageWorkspacesResponseV1,
};

impl AccountClient {
    pub fn cloud_plan(
        &self,
        authorization: &AccountAuthorization<'_>,
    ) -> Result<CloudPlanResponseV1, ClientError> {
        self.execute(
            RouteId::ReadCloudPlan,
            vec![],
            Some(authorization),
            false,
            None,
        )
    }

    pub fn cloud_usage_workspaces(
        &self,
        authorization: &AccountAuthorization<'_>,
        query: &CloudUsageWorkspacesQueryV1,
    ) -> Result<CloudUsageWorkspacesResponseV1, ClientError> {
        query.validate().map_err(|_| ClientError::InvalidRequest)?;
        let mut path = format!(
            "{}?limit={}",
            route_by_id(RouteId::ReadCloudUsageWorkspaces).path,
            query.limit.unwrap_or(100)
        );
        if let Some(cursor) = &query.cursor {
            path.push_str("&cursor=");
            path.push_str(cursor);
        }
        let page: CloudUsageWorkspacesResponseV1 = self.execute(
            RouteId::ReadCloudUsageWorkspaces,
            vec![],
            Some(authorization),
            false,
            Some(path),
        )?;
        if page.items.len() > usize::from(query.limit.unwrap_or(100))
            || (page.next_cursor.is_some() && page.next_cursor == query.cursor)
        {
            return Err(ClientError::InvalidResponse);
        }
        Ok(page)
    }
}
