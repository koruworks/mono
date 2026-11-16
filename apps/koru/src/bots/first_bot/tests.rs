// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::bots::{BotCommand, BotFolderChoice, managed_folder};
use koru_ai_core::{AiProviderId, ModelSelection, ReasoningChoice};
use koru_ai_ui::agent_catalog::config::picker::{Listing, Readiness};
use koru_draw::brand::ProfileMark;
use koru_draw::icons;
use koru_store_core::ids::WorkspaceId;

use super::*;
use crate::bots::creation::{CreateForm, NEW_BOT_PERMISSION};
use crate::bots::settings::ModelDraft;

fn offers<'a>(ids: &[&'a str]) -> Vec<ModeOffer<'a>> {
    ids.iter()
        .map(|id| ModeOffer {
            id: *id,
            name: None,
            description: None,
        })
        .collect()
}

fn agent(id: &'static str, hidden: bool, readiness: Option<Readiness>) -> Agent<'static, String> {
    Agent {
        id: id.to_owned(),
        key: None,
        label: id,
        mark: ProfileMark::Glyph(icons::ROBOT),
        readiness,
        hidden,
        listing: Listing::Loading,
    }
}

/// **A new Bot starts in its agent's automatic mode**: the mode that
/// approves safe actions on its own, never the one that runs everything
/// without asking, and never one the agent did not list.
#[test]
fn a_new_bot_starts_in_auto_and_never_in_full_access() {
    assert_eq!(FIRST_BOT_PERMISSION, NewChatPermission::Auto);
    let claude = offers(&[
        "default",
        "acceptEdits",
        "plan",
        "auto",
        "bypassPermissions",
    ]);
    assert_eq!(
        starting_mode("agent-claude", &claude, Some("default")).as_deref(),
        Some("auto")
    );
    let codex = offers(&["ask", "auto-review", "full-access"]);
    assert_eq!(
        starting_mode("agent-codex", &codex, Some("ask")).as_deref(),
        Some("auto-review")
    );
    // Whatever the agent reports as its own start, even full access.
    assert_eq!(
        starting_mode("agent-codex", &codex, Some("full-access")).as_deref(),
        Some("auto-review")
    );
    for (agent, modes) in [("agent-claude", &claude), ("agent-codex", &codex)] {
        let mode = starting_mode(agent, modes, None).unwrap();
        assert!(!modes::is_full_access(agent, &mode), "{agent}: {mode}");
        assert!(modes.iter().any(|offer| offer.id == mode));
    }
}

/// An agent with no automatic mode has nothing to approve with: its Bot
/// starts in its asking mode. One that lists no modes has none to start in.
#[test]
fn an_agent_without_an_automatic_mode_starts_asking() {
    let opencode = offers(&["build", "plan"]);
    assert_eq!(
        starting_mode("agent-opencode", &opencode, Some("build")).as_deref(),
        Some("build")
    );
    // An agent Koru has no table for, by the known spellings.
    let unknown = offers(&["suggest", "yolo"]);
    assert_eq!(
        starting_mode("agent-new", &unknown, Some("yolo")).as_deref(),
        Some("suggest")
    );
    let with_auto = offers(&["suggest", "auto-approve", "yolo"]);
    assert_eq!(
        starting_mode("agent-new", &with_auto, None).as_deref(),
        Some("auto-approve")
    );
    assert_eq!(starting_mode("agent-new", &[], None), None);
}

/// The create form is not changed: it still starts a Bot in the asking
/// mode (O42). Only the dialog's path asks for the automatic one.
#[test]
fn the_create_form_still_starts_a_bot_in_the_asking_mode() {
    assert_eq!(NEW_BOT_PERMISSION, NewChatPermission::Manual);
    assert_ne!(NEW_BOT_PERMISSION, FIRST_BOT_PERMISSION);
    let claude = offers(&["default", "auto", "bypassPermissions"]);
    assert_eq!(
        modes::default_mode(
            "agent-claude",
            &claude,
            None,
            NEW_BOT_PERMISSION,
            NEW_BOT_PERMISSION
        )
        .as_deref(),
        Some("default")
    );
}

/// **The first agent of the list**, in the order it is given, whatever its
/// readiness; one the person turned off is passed over.
#[test]
fn a_new_bot_starts_on_the_first_agent_of_the_list_that_is_not_turned_off() {
    let listed = [
        agent("agent-codex", false, Some(Readiness::NotInstalled)),
        agent("agent-claude", false, Some(Readiness::Ready)),
        agent("agent-opencode", false, None),
    ];
    // The first, not the first ready one: the person sees its readiness.
    assert_eq!(default_agent(&listed).as_deref(), Some("agent-codex"));
    let first_off = [
        agent("agent-codex", true, Some(Readiness::Ready)),
        agent("agent-claude", false, None),
    ];
    assert_eq!(default_agent(&first_off).as_deref(), Some("agent-claude"));
    // None to start on: the dialog refuses in words, it does not guess.
    let all_off = [agent("agent-codex", true, None)];
    assert_eq!(default_agent(&all_off), None);
    assert_eq!(default_agent::<String>(&[]), None);
}

/// **A folder of the Bot's own under Koru's local data**, exactly where
/// the Bot's owner makes a managed folder. Asking where it would be is path
/// arithmetic: no folder is named into existence (and no test touches disk).
#[test]
fn the_default_folder_is_the_owners_managed_folder_under_the_channel_root() {
    let channel = Path::new("R").join("Koru").join("dev");
    let bot = BotId::mint();
    let folder = default_folder(&channel, bot);
    assert_eq!(
        folder,
        channel
            .join("ai")
            .join("bots")
            .join(bot.to_storage())
            .join("work")
    );
    // The same path the owner derives from its AI root.
    assert_eq!(
        folder,
        managed_folder(&koru_channel::ai_root_under(&channel), bot)
    );
    assert!(folder.starts_with(&channel));
    // Per Bot: two Bots never share one.
    assert_ne!(folder, default_folder(&channel, BotId::mint()));
    // Stable for a Bot: the dialog can show it again and again.
    assert_eq!(folder, default_folder(&channel, bot));
}

/// **The dialog's Create**, through the unchanged form: the first agent,
/// the automatic mode, the managed folder and the picked face.
#[test]
fn the_dialogs_defaults_reach_the_creation_command() {
    let listed = [
        agent("agent-codex", false, None),
        agent("agent-claude", false, None),
    ];
    let provider = default_agent(&listed).unwrap();
    let codex = offers(&["ask", "auto-review", "full-access"]);
    let mut form = CreateForm::new(WorkspaceId::new());
    form.edit_name("  Scout ");
    form.edit_model(ModelDraft {
        model: ModelSelection {
            provider: AiProviderId::parse(provider.clone()).unwrap(),
            requested_model: "default".to_owned(),
            default_reasoning: ReasoningChoice::Default,
        },
        mode: None,
        fast: None,
    });
    // A face outside the table is not taken; one inside is.
    assert!(!form.choose_avatar(Some(koru_draw::face::FACE_COUNT)));
    assert!(form.choose_avatar(Some(9)));
    let Some(BotCommand::CreateBotWithRole {
        config,
        folder,
        role,
        ..
    }) = form.create(starting_mode(&provider, &codex, None))
    else {
        panic!("the dialog's draft creates");
    };
    assert_eq!(config.name, "Scout");
    assert_eq!(config.model.provider.as_str(), "agent-codex");
    assert_eq!(config.mode.as_deref(), Some("auto-review"));
    assert_eq!(config.avatar, Some(9));
    assert_eq!(folder, BotFolderChoice::Managed {});
    // The Role page is made empty, titled by the name, for setup to fill.
    assert_eq!(
        role,
        koru_ai_core::bots::BotRoleInput::Authored {
            title: "Scout".to_owned(),
            markdown: String::new(),
        }
    );
    // Once sent, the face cannot change under the same request.
    assert!(!form.choose_avatar(Some(3)));
}

/// A draft that picks no face sends none: the owner derives it.
#[test]
fn a_draft_with_no_face_sends_no_avatar() {
    let mut form = CreateForm::new(WorkspaceId::new());
    form.edit_name("Scout");
    form.edit_model(ModelDraft {
        model: ModelSelection {
            provider: AiProviderId::parse("agent-codex").unwrap(),
            requested_model: "default".to_owned(),
            default_reasoning: ReasoningChoice::Default,
        },
        mode: None,
        fast: None,
    });
    assert!(form.choose_avatar(Some(4)));
    assert!(form.choose_avatar(None));
    let Some(BotCommand::CreateBotWithRole { config, .. }) = form.create(None) else {
        panic!("creates");
    };
    assert_eq!(config.avatar, None);
    let json = serde_json::to_string(&config).unwrap();
    assert!(!json.contains("avatar"), "{json}");
}
