// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{KoruMcp, published_schema};

/// Every tool as the bridge publishes it, compacted, by name.
fn published() -> Vec<rmcp::model::Tool> {
    let mut router = KoruMcp::tool_router();
    published_schema::compact_router(&mut router);
    router.list_all()
}

/// Each toolset's bytes in the reply: its tools, each serialized as it is
/// sent, plus a separator each.
fn measured() -> Vec<(Toolset, usize, usize)> {
    let tools = published();
    Toolset::ALL
        .into_iter()
        .map(|set| {
            let in_set: Vec<_> = tools
                .iter()
                .filter(|tool| set.tools().contains(&tool.name.as_ref()))
                .collect();
            let bytes = in_set
                .iter()
                .map(|tool| serde_json::to_vec(tool).expect("tool JSON").len() + 1)
                .sum();
            (set, in_set.len(), bytes)
        })
        .collect()
}

#[test]
fn every_published_tool_is_in_exactly_one_toolset() {
    let routed: Vec<String> = published()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    for name in &routed {
        let sets: Vec<Toolset> = Toolset::ALL
            .into_iter()
            .filter(|set| set.tools().contains(&name.as_str()))
            .collect();
        assert_eq!(sets.len(), 1, "{name} is in {sets:?}");
    }
    for set in Toolset::ALL {
        for tool in set.tools() {
            assert!(
                routed.iter().any(|name| name == tool),
                "the {set} toolset names {tool}, which is no published tool"
            );
        }
    }
}

#[test]
fn toolsets_parse_strictly_and_always_include_core() {
    let docs_tasks = Toolsets::parse("docs,tasks").expect("docs,tasks");
    assert_eq!(
        docs_tasks.enabled().collect::<Vec<_>>(),
        [Toolset::Core, Toolset::Docs, Toolset::Tasks]
    );
    assert_eq!(docs_tasks.to_string(), "core, docs, tasks");
    assert_eq!(
        Toolsets::parse(" chat , docs ").expect("spaces around names"),
        Toolsets::parse("docs,chat").expect("docs,chat")
    );
    assert_eq!(
        Toolsets::parse("core")
            .expect("core alone")
            .enabled()
            .count(),
        1
    );
    assert!(
        Toolsets::parse("core,docs,files,versions,tasks,chat")
            .expect("every name")
            .is_all()
    );
    assert!(Toolsets::default().is_all());
    for bad in ["", ",", "docs,", "doc", "docs,Tasks", "all"] {
        assert!(Toolsets::parse(bad).is_err(), "{bad:?} was accepted");
    }
    let unknown = Toolsets::parse("docs,taks").unwrap_err();
    assert!(
        unknown.contains("\"taks\"") && unknown.contains("tasks"),
        "{unknown}"
    );
}

#[test]
fn a_tool_whose_toolset_is_off_is_refused_with_its_toolset_and_the_flag() {
    let docs = Toolsets::parse("docs").expect("docs");
    assert!(docs.refusal("edit_document").is_none());
    assert!(docs.refusal("get_documentation").is_none(), "core is on");
    assert!(docs.refusal("no_such_tool").is_none(), "the router answers");
    let refusal = docs.refusal("list_tasks").expect("tasks is off");
    assert_eq!(refusal.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(
        refusal.message.contains("tasks toolset"),
        "{}",
        refusal.message
    );
    assert!(
        refusal.message.contains("--toolsets docs,tasks"),
        "{}",
        refusal.message
    );
    let data = refusal.data.expect("details");
    assert_eq!(data["toolset"], "tasks");
    assert_eq!(data["enabled_toolsets"], json!(["core", "docs"]));
}

#[test]
fn the_note_says_which_toolsets_are_on_only_when_some_are_off() {
    assert_eq!(Toolsets::all().note(), None);
    let note = Toolsets::parse("docs,tasks")
        .expect("docs,tasks")
        .note()
        .expect("narrowed");
    assert!(note.contains("core, docs, tasks"), "{note}");
    assert!(note.contains("files, versions, chat are off"), "{note}");
}

/// Each toolset's share of `tools/list` stays within its budget, so growth
/// in one group is a decision about that group. The total, through the real
/// `tools/list` path, is `the_published_tool_list_is_compact_and_within_its_budget`.
#[test]
fn each_toolset_is_within_its_budget() {
    let sizes = measured();
    let table: Vec<String> = sizes
        .iter()
        .map(|(set, tools, bytes)| {
            format!(
                "{set}: {tools} tools, {bytes} bytes, ~{} tokens",
                bytes.div_ceil(4)
            )
        })
        .collect();
    println!("{}", table.join("\n"));
    for (set, _, bytes) in &sizes {
        let budget = BUDGETS
            .iter()
            .find(|(budgeted, _)| budgeted == set)
            .map(|(_, budget)| *budget)
            .unwrap_or_else(|| panic!("{set} has no budget"));
        assert!(
            *bytes < budget,
            "the {set} toolset is {bytes} bytes, over its {budget}-byte budget\n{}",
            table.join("\n")
        );
    }
}
