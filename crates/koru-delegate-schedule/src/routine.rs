// SPDX-License-Identifier: MPL-2.0

//! Retained Schedule identity used only to recognize and exclude old Bot data.

use koru_delegate_core::{BotId, BotOwner, BotRequestId, DeviceId, WorkspaceId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RoutineAssociation {
    pub owner: BotOwner,
    pub bot: BotId,
    pub home: WorkspaceId,
    pub creation_request: BotRequestId,
}

impl RoutineAssociation {
    /// Recheck typed values as well as wire values: `from_uuid` is intentionally
    /// available for fixtures and previously validated storage throughout core.
    /// Store-owned scope/workspace IDs retain their existing UUID domain.
    pub fn valid(&self) -> bool {
        self.bot.to_storage().parse::<BotId>().ok() == Some(self.bot)
            && self
                .creation_request
                .to_storage()
                .parse::<BotRequestId>()
                .ok()
                == Some(self.creation_request)
            && DeviceId::parse(&self.owner.device.to_storage()) == Some(self.owner.device)
    }
}
