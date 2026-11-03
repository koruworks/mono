// SPDX-License-Identifier: MPL-2.0

//! Resolving an attribute into the pill that shows it.
//!
//! Separate from [`koru_draw::pill`], which holds the palette and the shapes:
//! *how* a pill looks is the window layer's, but *what this attribute is* means
//! reading the document, and the window layer must not.
//!
//! That is not a stylistic split — `koru-draw` does not depend on `koru-core`,
//! so the compiler will not let this code back in.

use koru_core::node::{Attr, AttrValue, Kind};
use koru_core::schema::WorkspaceDoc;
use koru_core::{AttrId, DocRef, ResolvedRef};
use koru_draw::pill::{PillInfo, PillKind};

/// Format a date the way the picker labelled it: `Aug 2`.
pub fn date_label(d: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{} {}", MONTHS[(d.month() as usize - 1).min(11)], d.day())
}

/// Resolve one attribute into its drawable form.
///
/// The kind comes from the *value*, not the role, so a new role like "reviewer"
/// renders as a contact pill without any change here.
/// The document half of a caption key.
///
/// A document pill keys on `"{document}"`; a deep link keys on
/// `"{document}#{node}"`. Anything asked about the *document* — whether it is
/// in the bin, what it is called — wants this half of either.
pub fn document_of(key: &str) -> &str {
    key.split('#').next().unwrap_or(key)
}

/// The caption key for a document pill, with or without a block target.
pub fn entity_key(document: &str, node: Option<&str>) -> String {
    match node {
        Some(node) => format!("{document}#{node}"),
        None => document.to_owned(),
    }
}

/// Namespace a schedule identity away from document ids in the shared local
/// caption cache.
pub fn schedule_entity_key(schedule: &str) -> String {
    format!("schedule:{schedule}")
}

/// The caption a run pill falls back to when the record carries no label —
/// which the writer never leaves empty, so this is the belt to
/// `Attr::labelled`'s braces. Not localized on purpose: the localized label is
/// the one the writer stored, and this is the shape of a record that has none.
const RUN_WITHOUT_A_LABEL: &str = "Run";
const SCHEDULE_WITHOUT_A_LABEL: &str = "Schedule";

/// How many characters of a quoted passage its chip shows before the `…`.
const QUOTE_LABEL_CHARS: usize = 24;

/// A quote chip's caption: the passage's start, on one line, in quotation
/// marks — `"g work, so "worksp…"`. The passage itself stays whole in the
/// record; this is only what the chip shows.
pub fn quote_label(passage: &str) -> String {
    let one_line = passage.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = one_line.chars();
    let head: String = chars.by_ref().take(QUOTE_LABEL_CHARS).collect();
    if chars.next().is_some() {
        format!("\"{}\u{2026}\"", head.trim_end())
    } else {
        format!("\"{head}\"")
    }
}

pub fn info_for(ws: &WorkspaceDoc, attr: &Attr) -> PillInfo {
    // **A quote in the chat's composer** (koru-ai D88), keyed on its role as
    // a run is: a `Text` record would otherwise read as a document caption.
    if attr.role == koru_core::attributes::ROLE_QUOTE {
        let passage = match &attr.value {
            AttrValue::Text(passage) => passage.as_str(),
            _ => "",
        };
        return PillInfo {
            kind: PillKind::Quote,
            label: quote_label(passage),
            dangling: false,
            trashed: false,
            url: None,
            entity: None,
        };
    }
    if attr.role == koru_core::attributes::ROLE_SCHEDULE {
        let schedule = match &attr.value {
            AttrValue::Text(schedule) => Some(schedule.as_str()),
            _ => None,
        };
        return PillInfo {
            kind: PillKind::Schedule,
            label: attr
                .label
                .clone()
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| SCHEDULE_WITHOUT_A_LABEL.to_owned()),
            dangling: false,
            trashed: false,
            url: None,
            entity: schedule.map(schedule_entity_key),
        };
    }
    // **A run pill is keyed on its role**, the way an attachment is. What it
    // points at is a Run document — a document with no node tree, which this
    // renderer cannot inspect and must never draw as an ordinary page.
    //
    // Its caption is the short label the delegating gesture wrote, until the
    // host resolves the run's state off the window thread and pushes a fresh
    // one through `set_entity_titles` under the same entity key. **An
    // unresolved run is not a broken one** — `dangling` stays false — because
    // the run may be executing on a device this one has not heard from yet,
    // and a pill that reads as broken while the work is under way is a lie.
    if attr.role == koru_core::attributes::ROLE_RUN {
        let run = match &attr.value {
            AttrValue::Document(reference) => Some(reference.document),
            // A build whose reader does not normalize the role hands the same
            // record back homed at the run (P1.0 §3.2). Both are read here, so
            // this renderer does not depend on which reader it sits behind.
            AttrValue::Ref(reference) => match reference.home {
                Some(DocRef::Document(document)) => Some(document),
                Some(DocRef::DocumentAt { document, .. }) => Some(document),
                _ => None,
            },
            _ => None,
        };
        return PillInfo {
            kind: PillKind::Run,
            label: attr
                .label
                .clone()
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| RUN_WITHOUT_A_LABEL.to_owned()),
            dangling: false,
            trashed: false,
            url: None,
            entity: run.map(|run| run.to_storage()),
        };
    }
    match &attr.value {
        AttrValue::Date(d) => PillInfo {
            kind: PillKind::Date,
            label: attr.label.clone().unwrap_or_else(|| date_label(d.date())),
            dangling: false,
            trashed: false,
            url: None,
            entity: None,
        },
        AttrValue::Url(u) => PillInfo {
            kind: PillKind::Link,
            // A link's caption is user-authored; fall back to the URL itself.
            label: attr.label.clone().unwrap_or_else(|| u.clone()),
            dangling: false,
            trashed: false,
            url: Some(u.clone()),
            entity: None,
        },
        AttrValue::Text(t) => PillInfo {
            kind: PillKind::Document,
            label: attr.label.clone().unwrap_or_else(|| t.clone()),
            dangling: false,
            trashed: false,
            url: None,
            entity: None,
        },
        // The temporal shapes (ADR-183). Nothing inserts a pill carrying one
        // yet — the attribute chip is where these are drawn — so a stray one
        // renders as a labelled date pill.
        //
        // A floating value's day needs no zone, so it is read directly. A
        // **zoned** one is shown in the machine's zone, which is a *display*
        // choice this layer is entitled to make and the value's own zone is
        // untouched by it.
        AttrValue::FloatingDateTime(wall) => PillInfo {
            kind: PillKind::Date,
            label: attr
                .label
                .clone()
                .unwrap_or_else(|| date_label(wall.date().date())),
            dangling: false,
            trashed: false,
            url: None,
            entity: None,
        },
        AttrValue::ZonedDateTime(zoned) => PillInfo {
            kind: PillKind::Date,
            label: attr.label.clone().unwrap_or_else(|| {
                date_label(
                    crate::utils::zone::SystemZone::detect()
                        .display_date(zoned)
                        .date(),
                )
            }),
            dangling: false,
            trashed: false,
            url: None,
            entity: None,
        },
        AttrValue::Number(n) => PillInfo {
            kind: PillKind::Document,
            label: attr.label.clone().unwrap_or_else(|| n.to_string()),
            dangling: false,
            trashed: false,
            url: None,
            entity: None,
        },
        AttrValue::Bool(b) => PillInfo {
            kind: PillKind::Document,
            label: attr.label.clone().unwrap_or_else(|| b.to_string()),
            dangling: false,
            trashed: false,
            url: None,
            entity: None,
        },
        AttrValue::Document(reference) => PillInfo {
            kind: PillKind::Document,
            label: attr.label.clone().unwrap_or_else(|| "unavailable".into()),
            // A document outside the active LoroDoc cannot be inspected here;
            // location and lifecycle are supplied by Store-backed metadata.
            dangling: false,
            trashed: false,
            url: None,
            // **Both ids when there are two.** The key identifies the caption:
            // `Paragraph::adopt` carries a resolved caption forward only when
            // the key matches, so two links into two headings of one document
            // must not share one, or a rebuild smears one caption onto the
            // other. A separator that neither uuid can contain keeps it
            // unambiguous, and the document half stays parseable back out.
            entity: Some(entity_key(
                &reference.document.to_storage(),
                reference.node.map(|node| node.to_storage()).as_deref(),
            )),
        },
        AttrValue::Ref(r) => {
            // Attachments are workspace files, not nodes of this LoroDoc. Both
            // stored dialects (Document and DocumentAt) resolve through the
            // host's entity caption map — never through `ws.resolve`.
            if attr.role == "attachment" {
                let document = match r.home {
                    Some(DocRef::Document(document)) => Some(document),
                    Some(DocRef::DocumentAt { document, .. }) => Some(document),
                    _ => None,
                };
                return PillInfo {
                    kind: PillKind::File,
                    label: attr.label.clone().unwrap_or_else(|| "unavailable".into()),
                    // The host marks dangling when the file is not live and not
                    // in trash; until then a stored caption is not a break.
                    dangling: document.is_none(),
                    trashed: false,
                    url: None,
                    entity: document.map(|document| document.to_storage()),
                };
            }
            let resolved = ws.resolve(*r);
            let routed_document = matches!(r.home, Some(DocRef::DocumentAt { .. }));
            let kind = match &resolved {
                ResolvedRef::Found { id, .. } => match ws.read(*id).map(|n| n.kind) {
                    Ok(Kind::Contact) => PillKind::Contact,
                    _ => PillKind::Document,
                },
                // Nothing to inspect, so fall back on what the role implies.
                ResolvedRef::Dangling { .. } => {
                    if attr.role == "assignee" {
                        PillKind::Contact
                    } else {
                        PillKind::Document
                    }
                }
            };
            PillInfo {
                kind,
                label: attr
                    .label
                    .clone()
                    .unwrap_or_else(|| resolved.label().to_string()),
                // A routed document is outside the one LoroDoc this renderer
                // can inspect. Its stored caption is a valid manifest/search
                // label, not evidence that the link is broken.
                dangling: resolved.is_dangling() && !routed_document,
                trashed: false,
                url: None,
                entity: match r.home {
                    Some(DocRef::DocumentAt { document, .. }) => Some(document.to_storage()),
                    _ => None,
                },
            }
        }
    }
}

/// Resolve every attribute on a node, keyed by id so a run's `pill` marker can
/// find it.
pub fn info_for_node(
    ws: &WorkspaceDoc,
    node: &koru_core::Node,
) -> std::collections::HashMap<AttrId, PillInfo> {
    node.content
        .attrs()
        .iter()
        .map(|attr| (attr.id, info_for(ws, attr)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use koru_core::node::{Content, Node};
    use koru_core::schema::Parent;
    use koru_core::{DocRef, DocumentId, ProfileId, Ref, WorkspaceId};

    fn ws() -> WorkspaceDoc {
        WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap()
    }

    /// A quote in the chat's composer (koru-ai D88) is its own kind, captioned
    /// by the passage's start on one line in quotation marks; the passage is
    /// never the caption a `Text` record would otherwise get.
    #[test]
    fn a_quote_is_its_own_kind_captioned_by_its_start() {
        let attr = Attr::new(
            koru_core::attributes::ROLE_QUOTE,
            AttrValue::Text("g work, so \"workspaces\"\nstay together in one store".into()),
        );
        let info = info_for(&ws(), &attr);
        assert_eq!(info.kind, PillKind::Quote);
        assert_eq!(info.label, "\"g work, so \"workspaces\"\u{2026}\"");
        assert_eq!(quote_label("short"), "\"short\"");
    }

    /// The key is what `Paragraph::adopt` matches on and what a caption cache
    /// is looked up by, so a deep link has to key on both ids or two links into
    /// one document become interchangeable.
    #[test]
    fn a_document_reference_with_a_node_keys_on_both_ids() {
        let document = DocumentId::new();
        let node = koru_core::NodeId::new();
        let attr = Attr::labelled(
            "document",
            AttrValue::Document(koru_core::DocumentReference::at(document, node)),
            "Spec#Rollout".to_owned(),
        );

        let info = info_for(&ws(), &attr);

        assert_eq!(
            info.entity.as_deref(),
            Some(format!("{}#{}", document.to_storage(), node.to_storage()).as_str())
        );
        assert_eq!(
            document_of(info.entity.as_deref().unwrap()),
            document.to_storage()
        );
    }

    /// Every pill written before this keys on the document alone, and must go
    /// on doing so — the cache that feeds them is keyed that way.
    #[test]
    fn a_document_reference_without_a_node_keys_on_the_document_alone() {
        let document = DocumentId::new();
        let attr = Attr::labelled(
            "document",
            AttrValue::Document(koru_core::DocumentReference::new(document)),
            "Spec".to_owned(),
        );

        let info = info_for(&ws(), &attr);

        assert_eq!(info.entity.as_deref(), Some(document.to_storage().as_str()));
    }

    #[test]
    fn a_date_renders_itself() {
        let attr = Attr::new(
            "due",
            AttrValue::Date(koru_core::time::DateOnly::new(
                NaiveDate::from_ymd_opt(2026, 8, 2).unwrap(),
            )),
        );
        let info = info_for(&ws(), &attr);
        assert_eq!(info.kind, PillKind::Date);
        assert_eq!(info.label, "Aug 2");
    }

    #[test]
    fn a_reference_borrows_its_targets_name_and_kind() {
        let mut ws = ws();
        let kim = ws
            .insert(
                Parent::Root,
                Node::new(Kind::Contact, Content::text("Kim")).unwrap(),
            )
            .unwrap();

        let info = info_for(&ws, &Attr::new("assignee", AttrValue::Ref(Ref::new(kim))));
        assert_eq!(info.kind, PillKind::Contact);
        assert_eq!(info.label, "Kim");
        assert!(!info.dangling);
    }

    #[test]
    fn a_missing_target_still_draws() {
        let info = info_for(
            &ws(),
            &Attr::new(
                "assignee",
                AttrValue::Ref(Ref::new(koru_core::NodeId::new())),
            ),
        );
        assert!(info.dangling);
        assert_eq!(info.label, "unavailable");
        assert_eq!(
            info.kind,
            PillKind::Contact,
            "the role still tells us what it was"
        );
    }

    #[test]
    fn a_routed_document_uses_its_manifest_caption_without_looking_dangling() {
        let (profile, workspace, document) =
            (ProfileId::new(), WorkspaceId::new(), DocumentId::new());
        let attr = Attr::labelled(
            "document",
            AttrValue::Ref(Ref::in_doc(
                koru_core::NodeId::from_uuid(document.as_uuid()),
                DocRef::DocumentAt {
                    profile,
                    workspace,
                    document,
                },
            )),
            "Hello",
        );
        let info = info_for(&ws(), &attr);
        assert_eq!(info.label, "Hello");
        assert!(!info.dangling);
        assert_eq!(info.entity, Some(document.to_storage()));
    }

    /// A link's caption is content, so it survives instead of showing the URL.
    #[test]
    fn a_link_prefers_its_caption() {
        let attr = Attr::labelled(
            "link",
            AttrValue::Url("https://example.com/terms".into()),
            "Terms page",
        );
        let info = info_for(&ws(), &attr);
        assert_eq!(info.kind, PillKind::Link);
        assert_eq!(info.label, "Terms page");

        let bare = Attr::new("link", AttrValue::Url("https://example.com".into()));
        assert_eq!(info_for(&ws(), &bare).label, "https://example.com");
    }

    #[test]
    fn an_attachment_uses_either_document_dialect_and_keys_the_entity() {
        let document = DocumentId::new();
        let via_document = Attr::labelled(
            "attachment",
            AttrValue::Ref(Ref::in_doc(
                koru_core::NodeId::from_uuid(document.as_uuid()),
                DocRef::Document(document),
            )),
            "notes.pdf",
        );
        let via_at = Attr::labelled(
            "attachment",
            AttrValue::Ref(Ref::in_doc(
                koru_core::NodeId::from_uuid(document.as_uuid()),
                DocRef::DocumentAt {
                    profile: ProfileId::new(),
                    workspace: WorkspaceId::new(),
                    document,
                },
            )),
            "notes.pdf",
        );
        for attr in [via_document, via_at] {
            let info = info_for(&ws(), &attr);
            assert_eq!(info.kind, PillKind::File);
            assert_eq!(info.label, "notes.pdf");
            assert!(!info.dangling);
            assert_eq!(info.entity.as_deref(), Some(document.to_storage().as_str()));
        }
    }

    /// **A run pill is a run pill under either reader.**
    ///
    /// This build normalizes `koru.run` to `AttrValue::Document`; a build whose
    /// reader does not hands the identical record back as a `Ref` homed at the
    /// run. Both draw the same pill naming the same run, so a document written
    /// or last read by either build looks the same here.
    #[test]
    fn a_run_pill_resolves_under_either_reading_of_the_same_record() {
        let run = DocumentId::new();
        let typed = Attr::labelled(
            koru_core::attributes::ROLE_RUN,
            AttrValue::Document(koru_core::DocumentReference::new(run)),
            "Delegated run",
        );
        let homed = Attr::labelled(
            koru_core::attributes::ROLE_RUN,
            AttrValue::Ref(Ref::in_doc(
                koru_core::NodeId::from_uuid(run.as_uuid()),
                DocRef::Document(run),
            )),
            "Delegated run",
        );
        for attr in [typed, homed] {
            let info = info_for(&ws(), &attr);
            assert_eq!(info.kind, PillKind::Run);
            assert_eq!(info.label, "Delegated run");
            assert_eq!(info.entity.as_deref(), Some(run.to_storage().as_str()));
            assert_eq!(info.url, None);
        }
    }

    /// **A run this device cannot resolve is not a broken pill.** It may be
    /// executing on a machine this one has not heard from; it draws the label
    /// the delegating gesture stored, which is also exactly what an older
    /// build draws.
    #[test]
    fn an_unresolved_run_draws_its_label_and_not_a_break() {
        let info = info_for(
            &ws(),
            &Attr::labelled(
                koru_core::attributes::ROLE_RUN,
                AttrValue::Document(koru_core::DocumentReference::new(DocumentId::new())),
                "Delegated run",
            ),
        );
        assert!(!info.dangling, "an unresolved run is an ordinary state");
        assert!(!info.trashed);
        assert_eq!(info.label, "Delegated run");
    }

    /// A record with no label still says something rather than drawing an
    /// empty box. The writer always sets one; this is the belt to its braces.
    #[test]
    fn a_run_pill_without_a_label_still_has_a_caption() {
        let info = info_for(
            &ws(),
            &Attr::new(
                koru_core::attributes::ROLE_RUN,
                AttrValue::Document(koru_core::DocumentReference::new(DocumentId::new())),
            ),
        );
        assert!(!info.label.trim().is_empty());
    }

    #[test]
    fn a_schedule_pill_has_its_own_kind_and_namespaced_live_key() {
        let schedule = "00000000-0000-0000-0000-000000000123";
        let info = info_for(
            &ws(),
            &Attr::labelled(
                koru_core::attributes::ROLE_SCHEDULE,
                AttrValue::Text(schedule.into()),
                "Daily research",
            ),
        );
        assert_eq!(info.kind, PillKind::Schedule);
        assert_eq!(info.label, "Daily research");
        assert_eq!(
            info.entity.as_deref(),
            Some("schedule:00000000-0000-0000-0000-000000000123")
        );
        assert!(!info.dangling);
    }

    /// **Nothing on the drawing path can block.** A structural assertion, the
    /// same shape the delegation surface's own suite uses: resolving what a
    /// pill *is* reads the document in memory and nothing else, so a run's
    /// status can only ever reach a frame through a cache somebody else filled.
    #[test]
    fn resolving_a_pill_names_nothing_that_could_block() {
        let production = include_str!("pillinfo.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module's production half");
        for forbidden in ["koru_api_client", "std::fs", "std::thread", "reqwest"] {
            assert!(
                !production.contains(forbidden),
                "pillinfo.rs names `{forbidden}`; drawing must reach nothing"
            );
        }
    }

    /// Kind follows the value, so a new role needs no change here.
    #[test]
    fn an_unknown_role_still_resolves() {
        let mut ws = ws();
        let person = ws
            .insert(
                Parent::Root,
                Node::new(Kind::Contact, Content::text("Ada")).unwrap(),
            )
            .unwrap();
        let info = info_for(
            &ws,
            &Attr::new("reviewer", AttrValue::Ref(Ref::new(person))),
        );
        assert_eq!(info.kind, PillKind::Contact);
        assert_eq!(info.label, "Ada");
    }
}
