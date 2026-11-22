// SPDX-License-Identifier: MPL-2.0
//! Native adaptation of the shared, reference-only proposed catalog.
use crate::panes::explore::rail::{Bucket, RailTarget};
pub(super) const FIXTURE: &str = koru_ai_core::mini_apps::FIXTURE;
#[cfg(test)]
const MAX_BYTES: usize = koru_ai_core::mini_apps::MAX_BYTES;
pub(super) struct Declaration {
    pub target: RailTarget,
    pub swapped: bool,
    pub document: Option<koru_core::DocumentId>,
}
impl Declaration {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        Self::from_shared(koru_ai_core::mini_apps::Declaration::parse(bytes)?)
    }
    pub fn from_shared(value: koru_ai_core::mini_apps::Declaration) -> Result<Self, &'static str> {
        use koru_ai_core::mini_apps::TaskSelection;
        let target = match value
            .task_selection()
            .ok_or("composition requires the native composition host")?
        {
            TaskSelection::List(id) => RailTarget::List(koru_core::ids::ListId::from_uuid(*id)),
            TaskSelection::Bucket(bucket) => RailTarget::Bucket(match bucket.as_str() {
                "today" => Bucket::Today,
                "next" => Bucket::Next,
                "upcoming" => Bucket::Upcoming,
                "later" => Bucket::Later,
                "completed" => Bucket::Completed,
                _ => return Err("unsupported bucket"),
            }),
        };
        Ok(Self {
            target,
            swapped: value.swapped().ok_or("unsupported legacy arrangement")?,
            document: value
                .initial_document()
                .map(|id| koru_core::DocumentId::from_uuid(id.as_uuid())),
        })
    }
}
#[cfg(test)]
mod tests;
