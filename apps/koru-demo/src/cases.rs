// SPDX-License-Identifier: MPL-2.0

//! Two compiled workflows. Fixture creation and repetition live outside execution.
use crate::{
    input::{Action, Condition, shortcut},
    inspect::Role,
    locator::LocatorPattern,
};
use koru_keys::ShortcutId;

pub const DOCUMENT_ENTRY: &str = "M3 document entry";
pub const DOCUMENT_BEFORE: &str = "M3 original word";
pub const DOCUMENT_AFTER: &str = "M3 original ünïcode漢字";
pub const TASK_PLACEHOLDER: &str = "M3 checklist placeholder";
pub const TASK_TEXT: &str = "M3 checklist item";

#[derive(Clone, PartialEq, Eq)]
pub enum Target {
    Locator(LocatorPattern),
    Named {
        role: Role,
        name: String,
        language: &'static str,
    },
    Part {
        parent: Box<Target>,
        part: &'static str,
    },
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Locator(locator) => locator.fmt(f),
            Self::Named { role, language, .. } => f
                .debug_struct("Named")
                .field("role", role)
                .field("language", language)
                .finish(),
            Self::Part { parent, part } => f
                .debug_struct("Part")
                .field("parent", parent)
                .field("part", part)
                .finish(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    Input(Action),
    Expect(Condition),
    Present,
    Absent,
    InDocumentViewport(bool),
    ReachByPageDown,
    ReachByPageUp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub id: &'static str,
    pub target: Target,
    pub operation: Operation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    pub name: &'static str,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fixture {
    pub subject: LocatorPattern,
    pub document: LocatorPattern,
    pub entry: Option<LocatorPattern>,
    pub reminder_date: Option<String>,
    pub reminder_time: Option<String>,
}

pub fn names() -> &'static [&'static str] {
    &["document-edit", "task-reminder"]
}

fn locator(text: &str) -> Target {
    Target::Locator(LocatorPattern::parse(text).expect("compiled locator"))
}
fn part(target: &Target, part: &str) -> Target {
    let Target::Locator(pattern) = target else {
        unreachable!("fixture target")
    };
    let mut pattern = pattern.clone();
    pattern.part = part.into();
    Target::Locator(pattern)
}
fn input(id: &'static str, target: &Target, action: Action) -> Step {
    Step {
        id,
        target: target.clone(),
        operation: Operation::Input(action),
    }
}
fn expect(id: &'static str, target: &Target, condition: Condition) -> Step {
    Step {
        id,
        target: target.clone(),
        operation: Operation::Expect(condition),
    }
}
fn present(id: &'static str, target: &Target) -> Step {
    Step {
        id,
        target: target.clone(),
        operation: Operation::Present,
    }
}
fn key(id: &'static str, target: &Target, keys: &[u16]) -> Step {
    input(id, target, Action::Chord(keys.into()))
}
fn registered(id: &'static str, target: &Target, shortcut_id: ShortcutId) -> Step {
    input(
        id,
        target,
        shortcut(shortcut_id).expect("compiled shortcut"),
    )
}
fn text(id: &'static str, target: &Target, value: &str) -> Step {
    input(id, target, Action::Text(value.into()))
}

pub fn compile(name: &str, fixture: &Fixture) -> Result<Case, &'static str> {
    let subject = Target::Locator(fixture.subject.clone());
    let document = Target::Locator(fixture.document.clone());
    let save = locator("koru:w*/v*/s:save#document");
    let mut steps = match name {
        "document-edit" => {
            let entry = Target::Locator(fixture.entry.clone().ok_or("fixture-entry-required")?);
            vec![
                Step {
                    id: "subject-outside-document-viewport",
                    target: subject.clone(),
                    operation: Operation::InDocumentViewport(false),
                },
                Step {
                    id: "focus-entry",
                    target: entry.clone(),
                    operation: Operation::ReachByPageUp,
                },
                expect(
                    "fixture-entry",
                    &entry,
                    Condition::Text(DOCUMENT_ENTRY.into()),
                ),
                expect("entry-focused", &entry, Condition::Focused),
                Step {
                    id: "reach-last-paragraph",
                    target: subject.clone(),
                    operation: Operation::ReachByPageDown,
                },
                expect("last-paragraph-focused", &subject, Condition::Focused),
                expect(
                    "fixture-original",
                    &subject,
                    Condition::Text(DOCUMENT_BEFORE.into()),
                ),
                key("move-caret", &subject, &[0x23]),
                key("select-word-d", &subject, &[0x10, 0x25]),
                expect("selected-d", &subject, Condition::SelectedText("d".into())),
                key("select-word-rd", &subject, &[0x10, 0x25]),
                expect(
                    "selected-rd",
                    &subject,
                    Condition::SelectedText("rd".into()),
                ),
                key("select-word-ord", &subject, &[0x10, 0x25]),
                expect(
                    "selected-ord",
                    &subject,
                    Condition::SelectedText("ord".into()),
                ),
                key("select-last-word", &subject, &[0x10, 0x25]),
                expect(
                    "selection-is-word",
                    &subject,
                    Condition::SelectedText("word".into()),
                ),
                text("replace-selected-word", &subject, "ünïcode漢字"),
                expect(
                    "replacement-visible",
                    &subject,
                    Condition::Text(DOCUMENT_AFTER.into()),
                ),
                registered("undo", &subject, ShortcutId::Undo),
                expect(
                    "undo-visible",
                    &subject,
                    Condition::Text(DOCUMENT_BEFORE.into()),
                ),
                registered("redo", &subject, ShortcutId::Redo),
                expect(
                    "redo-visible",
                    &subject,
                    Condition::Text(DOCUMENT_AFTER.into()),
                ),
                expect("document-durable", &save, Condition::Name("Saved.".into())),
                registered("close-document-tab", &subject, ShortcutId::CloseTab),
                input("reopen-document", &document, Action::Click),
                // An Explore click opens the document but does not promise a
                // paragraph has keyboard focus. Re-establish the same captured
                // entry used on first open before paging through a cold body.
                Step {
                    id: "focus-reopened-entry",
                    target: entry.clone(),
                    operation: Operation::ReachByPageUp,
                },
                expect(
                    "reopened-entry",
                    &entry,
                    Condition::Text(DOCUMENT_ENTRY.into()),
                ),
                expect("reopened-entry-focused", &entry, Condition::Focused),
                Step {
                    id: "reach-reopened-paragraph",
                    target: subject.clone(),
                    operation: Operation::ReachByPageDown,
                },
                expect(
                    "reopened-edit-retained",
                    &subject,
                    Condition::Text(DOCUMENT_AFTER.into()),
                ),
            ]
        }
        "task-reminder" => {
            let date = fixture
                .reminder_date
                .as_deref()
                .ok_or("fixture-reminder-date-required")?;
            let time = fixture
                .reminder_time
                .as_deref()
                .ok_or("fixture-reminder-time-required")?;
            if [date, time].iter().any(|value| {
                value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control)
            }) {
                return Err("fixture-reminder-field-invalid");
            }
            let command_query = locator("koru:w*/v*/command:*#query");
            let command_task = locator("koru:w*/v*/command:*#task");
            let check = part(&subject, "check");
            let task_state = part(&subject, "state");
            let row = part(&subject, "task-row");
            let phrase = locator("koru:w*/v*/remind:*#phrase");
            let more = locator("koru:w*/v*/remind:*#more");
            let date_field = locator("koru:w*/v*/remind:*#date");
            let time_field = locator("koru:w*/v*/remind:*#time");
            let invalid = locator("koru:w*/v*/remind:*#invalid");
            let set = locator("koru:w*/v*/remind:*#set");
            let reminders = locator("koru:w*/v*/s:remind#root");
            let schedule = Target::Part {
                parent: Box::new(reminders),
                part: "schedule",
            };
            vec![
                Step {
                    id: "reach-item",
                    target: subject.clone(),
                    operation: Operation::ReachByPageDown,
                },
                expect(
                    "fixture-placeholder",
                    &subject,
                    Condition::Text(TASK_PLACEHOLDER.into()),
                ),
                input("focus-item", &subject, Action::Focus),
                expect("item-focused", &subject, Condition::Focused),
                Step {
                    id: "fixture-no-task",
                    target: task_state.clone(),
                    operation: Operation::Absent,
                },
                Step {
                    id: "fixture-no-reminder",
                    target: schedule.clone(),
                    operation: Operation::Absent,
                },
                key("item-home", &subject, &[0x24]),
                key("select-placeholder", &subject, &[0x10, 0x23]),
                text("create-item", &subject, TASK_TEXT),
                expect("item-text", &subject, Condition::Text(TASK_TEXT.into())),
                registered("make-checklist", &subject, ShortcutId::Checklist),
                present("checklist-control", &check),
                key("command-at-start", &subject, &[0x24]),
                text("open-command-menu", &subject, "/"),
                expect("command-query-focused", &command_query, Condition::Focused),
                expect(
                    "command-query-empty",
                    &command_query,
                    Condition::Text(String::new()),
                ),
                text("task-command", &command_query, "task"),
                expect(
                    "task-command-typed",
                    &command_query,
                    Condition::Text("task".into()),
                ),
                expect(
                    "task-command-available",
                    &command_task,
                    Condition::Enabled(true),
                ),
                key("confirm-task-command", &command_query, &[0x0d]),
                expect(
                    "task-state",
                    &task_state,
                    Condition::Name("Task: open".into()),
                ),
                // Tasks is the rail's third slot in a fresh window's default
                // order; the digit is the slot's, not the view's.
                registered("open-tasks", &subject, ShortcutId::ShowRailView3),
                input(
                    "open-later-queue",
                    &locator("koru:w*/v*/s:tasks-queue#later"),
                    Action::Click,
                ),
                input("select-intended-task", &row, Action::Click),
                expect("intended-task-focused", &row, Condition::Focused),
                key("open-intended-task", &row, &[0x0d]),
                expect("origin-item-focused", &subject, Condition::Focused),
                registered("open-reminder", &subject, ShortcutId::Reminders),
                present("reminder-field", &phrase),
                input("structured-reminder", &more, Action::Click),
                input("focus-date", &date_field, Action::Focus),
                key("select-date", &date_field, &[0x11, 0x41]),
                text("invalid-date", &date_field, "invalid"),
                present("validation-visible", &invalid),
                expect("invalid-cannot-submit", &set, Condition::Enabled(false)),
                key("select-invalid-date", &date_field, &[0x11, 0x41]),
                text("correct-date", &date_field, date),
                input("focus-time", &time_field, Action::Focus),
                key("select-time", &time_field, &[0x11, 0x41]),
                text("set-time", &time_field, time),
                expect("valid-can-submit", &set, Condition::Enabled(true)),
                input("confirm-reminder", &set, Action::Click),
                expect("return-to-item", &subject, Condition::Focused),
                expect(
                    "schedule-confirmed",
                    &schedule,
                    Condition::NamePrefixWithout {
                        prefix: "Scheduled for ".into(),
                        excluded: "Not confirmed by storage yet".into(),
                    },
                ),
                registered("revisit-reminder", &subject, ShortcutId::Reminders),
                present("saved-editor-open", &date_field),
                expect("saved-date", &date_field, Condition::Text(date.into())),
                expect("saved-time", &time_field, Condition::Text(time.into())),
                input(
                    "close-revisited-reminder",
                    &locator("koru:w*/v*/remind:*#cancel"),
                    Action::Click,
                ),
            ]
        }
        _ => return Err("unknown-case"),
    };
    let mut entry_steps = vec![
        input(
            "show-documents-navigation",
            &locator("koru:w*/v*/s:explore-mode#documents"),
            Action::Click,
        ),
        input("open-fixture-document", &document, Action::Click),
    ];
    entry_steps.append(&mut steps);
    let steps = entry_steps;
    Ok(Case {
        name: if name == "document-edit" {
            "document-edit"
        } else {
            "task-reminder"
        },
        steps,
    })
}

#[cfg(test)]
mod tests;
