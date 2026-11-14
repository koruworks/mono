// SPDX-License-Identifier: MPL-2.0

//! Single bounded reads, called only on the account worker. A caller that
//! paginates must restart on `usage_revision_changed`, never combine epochs.
use super::*;
use koru_account_wire::cloud_plan::{
    CloudPlanResponseV1, CloudUsageWorkspacesQueryV1, CloudUsageWorkspacesResponseV1, ReadUuid,
    documents::{CloudUsageDocumentsQueryV1, CloudUsageDocumentsResponseV1},
};

impl AccountClient {
    /// One page of the exact retained incarnation, never a global DocumentId
    /// lookup. These facts do not grant content editing or cleanup permission.
    pub fn cloud_usage_documents(
        &self,
        authorization: &AccountAuthorization<'_>,
        workspace: ReadUuid,
        query: &CloudUsageDocumentsQueryV1,
    ) -> Result<CloudUsageDocumentsResponseV1, ClientError> {
        query.validate().map_err(|_| ClientError::InvalidRequest)?;
        let route = route_by_id(RouteId::ReadCloudUsageDocuments);
        let mut path = format!(
            "{}?incarnation_id={}&limit={}",
            route
                .path
                .replace("{workspace_id}", &workspace.0.to_string()),
            query.incarnation_id.0,
            query.limit.unwrap_or(100)
        );
        if let Some(cursor) = &query.cursor {
            path.push_str("&cursor=");
            path.push_str(cursor);
        }
        let page: CloudUsageDocumentsResponseV1 = self.execute(
            RouteId::ReadCloudUsageDocuments,
            vec![],
            Some(authorization),
            false,
            Some(path),
        )?;
        if page.workspace_id != workspace
            || page.incarnation_id != query.incarnation_id
            || page.items.len() > usize::from(query.limit.unwrap_or(100))
            || (page.next_cursor.is_some() && page.next_cursor == query.cursor)
        {
            return Err(ClientError::InvalidResponse);
        }
        Ok(page)
    }

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
