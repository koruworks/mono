// SPDX-License-Identifier: MPL-2.0

//! The translation, which is the whole of what this file adds.
//!
//! The surface's own behaviour — what a key does, which hit is selected, when a
//! target is emitted at all — is pinned in `koru-search-ui`, where it lives.
//! What can only be checked here is what this app *makes of* a target, because
//! that is the part the agent will do differently.

use super::*;
use koru_search_ui::{Disposition, Target};
use koru_store_core::ids as store;

fn target() -> Target {
    Target::Store {
        profile: store::ProfileId::new(),
        workspace: store::WorkspaceId::new(),
        document: Some(store::DocumentId::new()),
        node: Some(NodeId::new().to_storage()),
        trashed: false,
        match_text: Some("a phrase".into()),
        disposition: Disposition::CurrentWindow,
    }
}

/// **This app's tests must not query the developer's own search index.**
///
/// `SearchReader::start` spawns threads that dial whatever store is listening
/// on this channel, which on a developer's machine is theirs, with their
/// writing in it — so a `cargo test` run would issue a landing-page query
/// against it and could record a navigation in its history.
///
/// The guard inside `koru-search-ui` is `cfg(test)`, and **a dependency is
/// never compiled with `cfg(test)`**: it covers that crate's own tests and
/// stopped covering these the moment the surface became a crate. The
/// `no-reader` feature, turned on in `[dev-dependencies]`, is what covers them
/// now — and this is the assertion that it is actually on. Deleting the feature
/// would leave every other test in this app passing.
#[test]
fn a_surface_built_under_test_dials_nothing() {
    assert!(!SearchSurface::new().0.has_reader());
    assert!(
        !SearchSurface::for_workspace(ProfileId::new(), WorkspaceId::new(), "Roadmap".into())
            .0
            .has_reader()
    );
}

#[test]
fn an_ai_result_opens_the_exact_chat_block_as_a_tab() {
    let chat = koru_ai_core::ChatId::new();
    let message = koru_ai_core::MessageId::new();
    let block = koru_ai_core::MessageBlockId::new();
    assert_eq!(
        pane_request(Target::Chat {
            chat,
            message,
            block,
            disposition: Disposition::CurrentWindow,
        }),
        Some(PaneRequest::OpenChatLink {
            chat: chat.to_string(),
            block: Some(block.to_string()),
        })
    );
}

#[test]
fn every_id_survives_the_crossing() {
    // Both vocabularies are UUID newtypes, so the conversion is total — but a
    // conversion that swapped two of them would still compile, and every one of
    // these addresses something different.
    let t = target();
    let Target::Store {
        document, ref node, ..
    } = t
    else {
        unreachable!()
    };
    let document = document.unwrap();
    let node = node.clone().unwrap();
    let Some(PaneRequest::Navigate {
        target:
            koru_workspace_host::NavigationTarget::Document {
                document: d,
                node: n,
            },
        match_text,
        ..
    }) = pane_request(t)
    else {
        panic!("a well-formed target must convert");
    };
    assert_eq!(d.as_uuid(), document.as_uuid());
    assert_eq!(n.unwrap().to_storage(), node);
    assert_eq!(match_text.as_deref(), Some("a phrase"));
}

#[test]
fn a_binned_result_keeps_the_same_stable_document_target() {
    // Trash state is location metadata. The central navigation route resolves
    // the current manifest instead of baking a trash root into this request.
    let mut t = target();
    let Target::Store {
        trashed, document, ..
    } = &mut t
    else {
        unreachable!()
    };
    *trashed = true;
    let document = document.unwrap();
    let Some(PaneRequest::Navigate {
        target:
            koru_workspace_host::NavigationTarget::Document {
                document: routed, ..
            },
        ..
    }) = pane_request(t)
    else {
        panic!("a binned target must convert");
    };
    assert_eq!(routed.as_uuid(), document.as_uuid());
}

#[test]
fn a_hit_on_a_documents_title_names_no_node_and_still_opens() {
    // The index stores a title as a fragment with no node, because it is
    // mirrored from the manifest listing rather than read from anybody's tree.
    // Searching a page **by its name** used to find it and refuse to open it;
    // `None` here has to reach the app as `None`, not as a refusal.
    let mut t = target();
    let Target::Store { node, .. } = &mut t else {
        unreachable!()
    };
    *node = None;
    let Some(PaneRequest::Navigate {
        target: koru_workspace_host::NavigationTarget::Document { node, .. },
        ..
    }) = pane_request(t)
    else {
        panic!("a title hit must convert");
    };
    assert!(node.is_none());
}

#[test]
fn a_workspace_level_hit_names_no_document_either() {
    // A match on the workspace's own name. There is nothing more to open, and
    // that is an answer rather than a failure.
    let mut t = target();
    let Target::Store {
        profile,
        workspace,
        document,
        node,
        ..
    } = &mut t
    else {
        unreachable!()
    };
    *document = None;
    *node = None;
    let expected_profile = *profile;
    let expected_workspace = *workspace;
    let Some(PaneRequest::Navigate {
        target: koru_workspace_host::NavigationTarget::Workspace { profile, workspace },
        ..
    }) = pane_request(t)
    else {
        panic!("a workspace hit must convert");
    };
    assert_eq!(profile.as_uuid(), expected_profile.as_uuid());
    assert_eq!(workspace.as_uuid(), expected_workspace.as_uuid());
}

#[test]
fn a_node_id_this_app_cannot_parse_is_refused_rather_than_dropped() {
    // Dropping it would leave `node: None`, which already means something real
    // — "a hit on the title" — and would land the caret at the top of a
    // document the person did not ask for.
    let mut t = target();
    let Target::Store { node, .. } = &mut t else {
        unreachable!()
    };
    *node = Some("not-a-uuid".into());
    assert!(pane_request(t).is_none());
}

#[test]
fn the_disposition_crosses_unchanged() {
    // Carried on the target rather than re-derived from keyboard state here:
    // the mouse and keyboard entry points produce the same object, and this app
    // stays independent of the surface's key bindings.
    for (from, expected) in [
        (
            Disposition::CurrentWindow,
            SearchTargetDisposition::CurrentWindow,
        ),
        (Disposition::NewWindow, SearchTargetDisposition::NewWindow),
    ] {
        let mut t = target();
        let Target::Store { disposition, .. } = &mut t else {
            unreachable!()
        };
        *disposition = from;
        let Some(PaneRequest::Navigate { disposition, .. }) = pane_request(t) else {
            panic!("must convert");
        };
        assert_eq!(disposition, expected);
    }
}
