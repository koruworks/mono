// SPDX-License-Identifier: MPL-2.0

//! One private, bounded A2UI surface. This is not a general catalog: dynamic
//! functions, templates, data models and arbitrary child graphs are refused
//! before a host, query worker or document session can be constructed.

use crate::panes::explore::rail::{Bucket, RailTarget};
use serde::Deserialize;

pub(super) const FIXTURE: &str = include_str!("fixture.json");
const MAX_BYTES: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: String,
    #[serde(rename = "createSurface")]
    surface: Surface,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Surface {
    surface_id: String,
    catalog_id: String,
    send_data_model: bool,
    components: Vec<Component>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Component {
    id: String,
    component: String,
    children: Option<Vec<String>>,
    arrangement: Option<String>,
    bucket: Option<String>,
    list: Option<ListReference>,
    document: Option<DocumentReference>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ListReference {
    workspace: String,
    list_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DocumentReference {
    workspace: String,
    document_id: String,
}

pub(super) struct Declaration {
    pub target: RailTarget,
    pub swapped: bool,
    pub document: Option<koru_core::DocumentId>,
}

impl Declaration {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_BYTES {
            return Err("surface exceeds byte limit");
        }
        let envelope: Envelope =
            serde_json::from_slice(bytes).map_err(|_| "unsupported declaration")?;
        let surface = envelope.surface;
        if envelope.version != "1.0"
            || surface.surface_id != "tasks-document"
            || surface.catalog_id != "koru:p1"
            || surface.send_data_model
            || surface.components.len() != 5
        {
            return Err("unsupported surface");
        }
        let expected = [
            ("root", "TaskDocument"),
            ("selector", "TaskSelector"),
            ("tasks", "Tasks"),
            ("search", "DocumentSearch"),
            ("document", "DocumentEditor"),
        ];
        for (component, (id, kind)) in surface.components.iter().zip(expected) {
            if component.id != id
                || component.component != kind
                || (id != "root"
                    && (component.children.is_some() || component.arrangement.is_some()))
                || (id != "tasks" && (component.bucket.is_some() || component.list.is_some()))
                || (id != "document" && component.document.is_some())
            {
                return Err("unsupported component");
            }
        }
        let root = &surface.components[0];
        if root.children.as_deref()
            != Some(&[
                "selector".into(),
                "tasks".into(),
                "search".into(),
                "document".into(),
            ])
        {
            return Err("unsupported child graph");
        }
        let swapped = match root.arrangement.as_deref() {
            Some("tasks-first") => false,
            Some("document-first") => true,
            _ => return Err("unsupported arrangement"),
        };
        let task = &surface.components[2];
        let target = if let Some(list) = &task.list {
            if task.bucket.is_some() || list.workspace != "current" {
                return Err("unsupported list scope");
            }
            RailTarget::List(
                koru_core::ids::ListId::parse(&list.list_id).ok_or("invalid list reference")?,
            )
        } else {
            RailTarget::Bucket(match task.bucket.as_deref() {
                Some("today") => Bucket::Today,
                Some("next") => Bucket::Next,
                Some("upcoming") => Bucket::Upcoming,
                Some("later") => Bucket::Later,
                Some("completed") => Bucket::Completed,
                _ => return Err("unsupported bucket"),
            })
        };
        let document = surface.components[4]
            .document
            .as_ref()
            .map(|reference| {
                if reference.workspace != "current" {
                    return Err("unsupported document scope");
                }
                koru_core::DocumentId::parse(&reference.document_id)
                    .ok_or("invalid document reference")
            })
            .transpose()?;
        Ok(Self {
            target,
            swapped,
            document,
        })
    }
}

#[cfg(test)]
mod tests;
