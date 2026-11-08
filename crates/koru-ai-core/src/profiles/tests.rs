// SPDX-License-Identifier: MPL-2.0

use super::*;

fn catalog(profiles: &[ProfileId], owners: &[(WorkspaceId, ProfileId)]) -> ProfileCatalog {
    ProfileCatalog {
        profiles: profiles.to_vec(),
        workspace_owners: owners.iter().copied().collect(),
    }
}

fn evidence(reads: Vec<(i64, ChatRead)>, grants: Vec<WorkspaceId>) -> ChatPlacementEvidence {
    ChatPlacementEvidence {
        chat_id: ChatId::new(),
        reads: reads
            .into_iter()
            .map(|(at, read)| (Timestamp::from_millis(at), read))
            .collect(),
        grants,
    }
}

#[test]
fn the_newest_read_decides_whatever_its_kind() {
    let (private, work) = (ProfileId::new(), ProfileId::new());
    let (home, office) = (WorkspaceId::new(), WorkspaceId::new());
    let report = DocumentId::new();
    let catalog = catalog(&[private, work], &[(home, private), (office, work)]);
    let documents = |document: DocumentId| (document == report).then_some(office);

    // A document read after a tool's call in the other profile's workspace.
    let chat = evidence(
        vec![
            (10, ChatRead::Workspace(home)),
            (20, ChatRead::Document(report)),
        ],
        vec![home],
    );
    assert_eq!(place_chat(&chat, &catalog, documents), Some(work));

    // The other way round, listed out of order.
    let chat = evidence(
        vec![
            (10, ChatRead::Document(report)),
            (30, ChatRead::Workspace(home)),
        ],
        vec![office],
    );
    assert_eq!(place_chat(&chat, &catalog, documents), Some(private));
}

#[test]
fn a_read_nobody_can_place_falls_through_to_the_next() {
    let (private, work) = (ProfileId::new(), ProfileId::new());
    let (home, gone, removed) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
    let removed_profile = ProfileId::new();
    // `removed` belongs to a profile that is no longer listed; `gone` to
    // nobody the Store knows; the moved document is in no workspace.
    let catalog = catalog(
        &[private, work],
        &[(home, work), (removed, removed_profile)],
    );
    let chat = evidence(
        vec![
            (40, ChatRead::Document(DocumentId::new())),
            (30, ChatRead::Workspace(gone)),
            (20, ChatRead::Workspace(removed)),
            (10, ChatRead::Workspace(home)),
        ],
        Vec::new(),
    );
    assert_eq!(place_chat(&chat, &catalog, |_| None), Some(work));
}

#[test]
fn with_no_read_the_newest_grant_decides() {
    let (private, work) = (ProfileId::new(), ProfileId::new());
    let (home, office, gone) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
    let catalog = catalog(&[private, work], &[(home, private), (office, work)]);
    let chat = evidence(Vec::new(), vec![gone, office, home]);
    assert_eq!(place_chat(&chat, &catalog, |_| None), Some(work));
}

#[test]
fn with_no_evidence_the_first_profile_takes_it_and_with_no_profile_nobody() {
    let (first, second) = (ProfileId::new(), ProfileId::new());
    let chat = evidence(Vec::new(), Vec::new());
    assert_eq!(
        place_chat(&chat, &catalog(&[first, second], &[]), |_| None),
        Some(first)
    );
    assert_eq!(
        place_chat(&chat, &ProfileCatalog::default(), |_| None),
        None
    );
}
