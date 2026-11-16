// SPDX-License-Identifier: MPL-2.0

//! **What a Bot made through the *New bot* dialog starts with** (QUESTIONS
//! O155). The dialog asks for a name, a face and where the Bot runs, so
//! everything else the create form asks for is decided here, in one place,
//! as plain functions with no drawing, no thread and no I/O:
//!
//! - **its agent**: the first agent of the person's list
//!   ([`default_agent`]);
//! - **its working folder**: a folder of its own under Koru's local data,
//!   which the dialog shows and lets the person change
//!   ([`default_folder`]);
//! - **its mode**: the agent's automatic mode rather than its asking one
//!   ([`starting_mode`], [`FIRST_BOT_PERMISSION`]): "the mode is auto by
//!   default since they are bots".
//!
//! The create form ([`crate::bots::creation`], [`crate::bots::view`]) is not
//! changed by any of this: a Bot made there still starts on the preferred
//! agent, in the asking mode (`NEW_BOT_PERMISSION`, O42).
//!
//! The dialog sends the same `CreateBotWithRole` through the same
//! [`CreateForm`](crate::bots::creation::CreateForm): it seeds the draft's
//! agent with [`default_agent`], passes [`starting_mode`] to
//! `CreateForm::create`, leaves the folder as the managed default unless the
//! person chose one, and sets the face with `CreateForm::choose_avatar`.

use std::path::{Path, PathBuf};

use koru_ai_core::bots::BotId;
use koru_ai_ui::agent_catalog::config::modes::{self, ModeOffer, NewChatPermission};
use koru_ai_ui::agent_catalog::config::picker::Agent;

// One table of faces, two crates that must agree on its length: the one
// that draws them and the one that stores the choice and bounds the column.
const _: () = assert!(
    koru_draw::face::FACE_COUNT == koru_ai_core::bots::BOT_AVATAR_COUNT,
    "the face table and the stored avatar bound differ"
);

/// **The permission a Bot made through the *New bot* dialog starts in**
/// (QUESTIONS O155, the owner's answer, reversing O42 for this path): the
/// agent's mode that approves safe actions on its own. Never the mode that
/// runs everything without asking, and never the person's own default for
/// new chats. One constant, so a change of mind is one line.
pub(crate) const FIRST_BOT_PERMISSION: NewChatPermission = NewChatPermission::Auto;

/// **The mode a Bot made through the dialog starts in**, from the agent's
/// own listed modes: its automatic mode; for an agent that offers none, its
/// asking mode (it has nothing to approve with). `None` only for an agent
/// that lists no modes, which leaves the runtime's own start.
///
/// The mapping itself is the chat's ([`modes::default_mode`]); this only
/// says which permission a new Bot asks it for.
pub(crate) fn starting_mode(
    agent: &str,
    offers: &[ModeOffer<'_>],
    reported_default: Option<&str>,
) -> Option<String> {
    modes::default_mode(
        agent,
        offers,
        reported_default,
        FIRST_BOT_PERMISSION,
        FIRST_BOT_PERMISSION,
    )
}

/// **The agent a Bot made through the dialog starts on**: the first agent
/// of the list as the control orders it (`Catalog::control_agents`: the
/// person's order from *Agent providers*) that the person has not turned
/// off. `None` when there is none, and then the dialog refuses to create in
/// the form's own words (`BotsAgentRequired`) rather than pick silently.
///
/// Deliberately not the create form's `Catalog::preferred` (the default
/// agent, else the first ready one): the owner's answer is the first in the
/// list, shown with its readiness, and the person can switch.
pub(crate) fn default_agent<P: Clone>(agents: &[Agent<'_, P>]) -> Option<P> {
    agents
        .iter()
        .find(|agent| !agent.hidden)
        .map(|agent| agent.id.clone())
}

/// **Where a new Bot works when the person chooses no folder**, for the
/// dialog to show before the Bot exists: `<channel root>/ai/bots/<BotId>/work`,
/// under Koru's local data (`%LOCALAPPDATA%\Koru\<channel>` on Windows).
///
/// This is the path the Bot's owner derives for `BotFolderChoice::Managed`
/// ([`koru_ai_core::bots::managed_folder`] under
/// [`koru_channel::ai_root_under`]), computed the same way so the two cannot
/// differ. It names a path and creates nothing: the owner makes the folder
/// when the Bot is created. The dialog still sends `Managed {}`, never this
/// path, so the owner alone decides where a managed folder is; a folder the
/// person picks instead is sent as `Selected` and must already exist.
pub(crate) fn default_folder(channel_root: &Path, bot: BotId) -> PathBuf {
    koru_ai_core::bots::managed_folder(&koru_channel::ai_root_under(channel_root), bot)
}

/// **The channel root this window runs against**: the `--root` it was
/// launched with, else this channel's own. `None` when neither resolves (no
/// local data directory in the environment), and then the dialog names no
/// path. The dialog reads it once when it opens and works every folder it
/// shows out from it with [`default_folder`], so a draft that takes new
/// identities needs no second look.
// Reads the process's arguments and environment (and, on macOS only, asks
// whether two directories exist), so no test calls it; the rule applied to
// what it answers is `default_folder`'s, which the tests hold.
pub(crate) fn channel_root_here() -> Option<PathBuf> {
    koru_workspace_host::cli::root_from_args().or_else(koru_channel::channel_root)
}

/// [`default_folder`] under [`channel_root_here`], for a caller that asks
/// about one Bot once.
#[allow(dead_code)] // the dialog keeps the root and asks `default_folder`
pub(crate) fn default_folder_here(bot: BotId) -> Option<PathBuf> {
    Some(default_folder(&channel_root_here()?, bot))
}

#[cfg(test)]
mod tests;
