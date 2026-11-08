// SPDX-License-Identifier: MPL-2.0

//! New node ids the server mints, so an agent never has to.
//!
//! An agent writing a document used to invent a UUID for every block it
//! inserted and for the title node, and most spent a step running a command
//! to get them: a language model cannot produce a random UUID by itself. Now a
//! new node's id — `insert`'s `node_id` in `edit_document` and in
//! `create_document`'s seed, `create_document`'s `root_node_id`, and
//! `add_image`'s `node_id` — is one of three things:
//!
//! - **a UUID**, used exactly as given, as it always was;
//! - **absent**, and the server mints one; or
//! - **a local name**: any other string, such as `"intro"`. The server mints
//!   a UUID for it, and a later operation in the same request names the node
//!   by it in `node_id`, `parent_id` or `after_node_id`.
//!
//! The request is rewritten to UUIDs before anything runs, so validation, the
//! CRDT and the store see exactly what a caller that minted its own ids would
//! have sent. **A stored node id is a v4 UUID either way**: a name lives for
//! one request and is stored nowhere. The answer carries `created`, each local
//! name — or, for an id left out, the operation's index — mapped to the UUID
//! minted for it.
//!
//! A name is usable only after the insert that defines it, is defined once,
//! and is never all digits, which would read as an index in `created`. A
//! string that looks like a UUID but does not parse is refused rather than
//! taken for a name, so a mistyped id is not silently replaced by a new one.
//!
//! `extract_to_document` takes no new node id — the destination's ids and the
//! trail link's are minted by the move itself — so it has nothing here.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::{EditOperation, InitialOperation, NodeId, ToolError, WorkspaceDoc};

/// What `create_document` calls its title node when `root_node_id` is left
/// out and a seed is sent, so the seed's inserts can name their parent.
pub(crate) const ROOT_NAME: &str = "root";

/// Each local name a request defined, and each id it left out by operation
/// index, mapped to the UUID minted for it: the `created` an answer carries.
pub(crate) type Created = Map<String, Value>;

/// The field a new node's id is in: optional where a caller may leave it out,
/// required where the server built the operation itself (`add_image`'s).
enum NewId<'a> {
    Optional(&'a mut Option<String>),
    Required(&'a mut String),
}

impl NewId<'_> {
    fn raw(&self) -> Option<&str> {
        match self {
            Self::Optional(value) => value.as_deref(),
            Self::Required(value) => Some(value.as_str()),
        }
    }

    fn set(&mut self, id: String) {
        match self {
            Self::Optional(value) => **value = Some(id),
            Self::Required(value) => **value = id,
        }
    }
}

/// One operation's id fields: the new node it makes, if any, and the
/// existing nodes it names.
struct Slots<'a> {
    new_id: Option<NewId<'a>>,
    references: Vec<(&'static str, &'a mut String)>,
}

impl<'a> Slots<'a> {
    fn names(node_id: &'a mut String) -> Self {
        Self {
            new_id: None,
            references: vec![("node_id", node_id)],
        }
    }

    fn inserts(
        new_id: NewId<'a>,
        parent_id: &'a mut String,
        after_node_id: &'a mut Option<String>,
    ) -> Self {
        Self {
            new_id: Some(new_id),
            references: placement(parent_id, after_node_id),
        }
    }
}

fn placement<'a>(
    parent_id: &'a mut String,
    after_node_id: &'a mut Option<String>,
) -> Vec<(&'static str, &'a mut String)> {
    let mut references = vec![("parent_id", parent_id)];
    if let Some(after) = after_node_id {
        references.push(("after_node_id", after));
    }
    references
}

/// Every id field of an edit. The match is exhaustive on purpose: a new
/// operation does not compile until it says which of its fields name nodes.
fn edit_slots(operation: &mut EditOperation) -> Slots<'_> {
    use EditOperation as Op;
    match operation {
        Op::Insert {
            node_id,
            parent_id,
            after_node_id,
            ..
        } => Slots::inserts(NewId::Optional(node_id), parent_id, after_node_id),
        Op::InsertImage {
            node_id,
            parent_id,
            after_node_id,
            ..
        } => Slots::inserts(NewId::Required(node_id), parent_id, after_node_id),
        Op::ReplaceImage { node_id, .. } => Slots::names(node_id),
        Op::Move {
            node_id,
            parent_id,
            after_node_id,
            ..
        } => {
            let mut slots = Slots::names(node_id);
            slots.references.extend(placement(parent_id, after_node_id));
            slots
        }
        // An inline `/document` pill's `node_id` names a block in another
        // document, never one this request makes, so it is left alone.
        Op::ReplaceText { node_id, .. }
        | Op::InsertInline { node_id, .. }
        | Op::Remove { node_id, .. }
        | Op::SetDone { node_id, .. }
        | Op::SetLanguage { node_id, .. }
        | Op::SetAlign { node_id, .. }
        | Op::SetMark { node_id, .. }
        | Op::SetHeading { node_id, .. }
        | Op::SetHighlight { node_id, .. }
        | Op::SetCalloutLook { node_id, .. }
        | Op::SetAttribute { node_id, .. }
        | Op::ClearAttribute { node_id, .. }
        | Op::AddTag { node_id, .. }
        | Op::RemoveTag { node_id, .. } => Slots::names(node_id),
    }
}

/// Every id field of a seed operation, as [`edit_slots`].
fn seed_slots(operation: &mut InitialOperation) -> Slots<'_> {
    use InitialOperation as Op;
    match operation {
        Op::Insert {
            node_id,
            parent_id,
            after_node_id,
            ..
        } => Slots::inserts(NewId::Optional(node_id), parent_id, after_node_id),
        Op::InsertInline { node_id, .. }
        | Op::SetDone { node_id, .. }
        | Op::SetLanguage { node_id, .. }
        | Op::SetAlign { node_id, .. }
        | Op::SetMark { node_id, .. }
        | Op::SetCalloutLook { node_id, .. } => Slots::names(node_id),
    }
}

/// The names one request defines and what has been minted for them so far.
#[derive(Default)]
struct Names {
    /// Each local name the request defines, and the field that defines it.
    defined_at: HashMap<String, String>,
    /// Each local name reached so far, and the UUID minted for it.
    minted: HashMap<String, String>,
    created: Created,
}

impl Names {
    /// The first pass: every definition, before anything is minted, so a use
    /// before its definition can be told from a name nothing defines, and a
    /// name defined twice is refused outright.
    fn declare(&mut self, field: String, raw: Option<&str>) -> Result<(), ToolError> {
        let Some(name) = raw
            .map(|raw| local_name(&field, raw))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        if let Some(first) = self.defined_at.get(name) {
            return Err(refusal(
                &field,
                name,
                format!(
                    "local name {name:?} is defined twice, by {first} and by {field}; each new node needs a name of its own"
                ),
            ));
        }
        self.defined_at.insert(name.to_owned(), field);
        Ok(())
    }

    /// A reference to a node: a UUID stays, a name reached so far becomes
    /// the UUID minted for it.
    fn resolve(&self, field: &str, value: &mut String) -> Result<(), ToolError> {
        let Some(name) = local_name(field, value)? else {
            return Ok(());
        };
        match (self.minted.get(name), self.defined_at.get(name)) {
            (Some(uuid), _) => {
                *value = uuid.clone();
                Ok(())
            }
            (None, Some(definition)) => Err(refusal(
                field,
                name,
                format!(
                    "{field} names {name:?} before {definition} defines it; a local name is usable only after the insert that defines it"
                ),
            )),
            (None, None) => Err(refusal(
                field,
                name,
                format!(
                    "{field} is {name:?}, which is neither a UUID nor a local name an earlier insert in this request defined"
                ),
            )),
        }
    }

    /// A new node's id: a UUID stays; a name, or nothing, gets a minted UUID,
    /// recorded in `created` under the name or under `index`.
    fn define(&mut self, field: &str, index: &str, slot: &mut NewId<'_>) -> Result<(), ToolError> {
        let name = match slot.raw() {
            None => None,
            Some(raw) => match local_name(field, raw)? {
                None => return Ok(()),
                Some(name) => Some(name.to_owned()),
            },
        };
        let uuid = NodeId::new().to_storage();
        if let Some(name) = &name {
            self.minted.insert(name.clone(), uuid.clone());
        }
        self.created.insert(
            name.unwrap_or_else(|| index.to_owned()),
            Value::String(uuid.clone()),
        );
        slot.set(uuid);
        Ok(())
    }

    /// Both passes over one list of operations, after anything defined before
    /// it (`create_document`'s title node) has been.
    fn declare_all<T>(
        &mut self,
        list: &str,
        operations: &mut [T],
        slots: fn(&mut T) -> Slots<'_>,
    ) -> Result<(), ToolError> {
        for (index, operation) in operations.iter_mut().enumerate() {
            if let Some(new_id) = slots(operation).new_id {
                self.declare(format!("{list}[{index}].node_id"), new_id.raw())?;
            }
        }
        Ok(())
    }

    fn resolve_all<T>(
        &mut self,
        list: &str,
        operations: &mut [T],
        slots: fn(&mut T) -> Slots<'_>,
    ) -> Result<(), ToolError> {
        for (index, operation) in operations.iter_mut().enumerate() {
            let Slots { new_id, references } = slots(operation);
            // References first: an insert never names the node it makes.
            for (field, value) in references {
                self.resolve(&format!("{list}[{index}].{field}"), value)?;
            }
            if let Some(mut new_id) = new_id {
                let field = format!("{list}[{index}].node_id");
                self.define(&field, &index.to_string(), &mut new_id)?;
            }
        }
        Ok(())
    }
}

/// `None` for a UUID, the name for a local name, and a refusal for a string
/// that can be neither.
fn local_name<'a>(field: &str, raw: &'a str) -> Result<Option<&'a str>, ToolError> {
    if NodeId::parse(raw).is_some() {
        return Ok(None);
    }
    let why = if raw.trim().is_empty() {
        format!(
            "{field} is empty; leave a new node's id out to have one minted, or send a UUID or a local name"
        )
    } else if raw.chars().all(|c| c.is_ascii_digit()) {
        format!(
            "{field} is {raw:?}, all digits; a local name needs a letter, because created keys an id left out by its operation's index"
        )
    } else if looks_like_uuid(raw) {
        format!(
            "{field} is {raw:?}, which looks like a UUID but is not one; send a whole UUID, or a short local name such as \"intro\""
        )
    } else {
        return Ok(Some(raw));
    };
    Err(refusal(field, raw, why))
}

/// Hex digits and UUID punctuation only, and most of a UUID's worth of
/// digits: a mistyped id, not a name anybody chose.
fn looks_like_uuid(raw: &str) -> bool {
    let body = raw.strip_prefix("urn:uuid:").unwrap_or(raw);
    let digits = body.chars().filter(char::is_ascii_hexdigit).count();
    digits >= 24
        && body
            .chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, '-' | '{' | '}'))
}

fn refusal(field: &str, name: &str, message: String) -> ToolError {
    ToolError {
        code: "invalid_request".into(),
        message,
        details: Some(json!({ "field": field, "local_name": name })),
    }
}

/// `edit_document`'s operations, rewritten to UUIDs in place.
pub(crate) fn resolve_edit(operations: &mut [EditOperation]) -> Result<Created, ToolError> {
    let mut names = Names::default();
    names.declare_all("operations", operations, edit_slots)?;
    names.resolve_all("operations", operations, edit_slots)?;
    Ok(names.created)
}

/// `create_document`'s title node and seed, rewritten to UUIDs in place.
///
/// With a seed, a `root_node_id` left out is the local name [`ROOT_NAME`], so
/// the seed's inserts have a parent to name. Without one nothing can name the
/// title node, so a left-out id stays left out and the document is made as it
/// always was.
pub(crate) fn resolve_seed(
    root_node_id: &mut Option<String>,
    operations: Option<&mut [InitialOperation]>,
) -> Result<Created, ToolError> {
    let mut names = Names::default();
    let root_field = match (root_node_id.is_none(), &operations) {
        (true, Some(_)) => {
            *root_node_id = Some(ROOT_NAME.to_owned());
            format!("root_node_id (left out, so the title node is {ROOT_NAME:?})")
        }
        _ => "root_node_id".to_owned(),
    };
    if root_node_id.is_some() {
        names.declare(root_field.clone(), root_node_id.as_deref())?;
    }
    let mut operations = operations;
    if let Some(operations) = operations.as_deref_mut() {
        names.declare_all("initial_operations", operations, seed_slots)?;
    }
    if root_node_id.is_some() {
        names.define(&root_field, ROOT_NAME, &mut NewId::Optional(root_node_id))?;
    }
    if let Some(operations) = operations {
        names.resolve_all("initial_operations", operations, seed_slots)?;
    }
    Ok(names.created)
}

/// `add_image`'s one new node: its UUID, and `created` when a local name was
/// given for it. An id left out is minted and reported as the answer's own
/// `node_id`, which `add_image` has always carried.
pub(crate) fn resolve_single(node_id: Option<String>) -> Result<(String, Created), ToolError> {
    let Some(mut id) = node_id else {
        return Ok((NodeId::new().to_storage(), Created::new()));
    };
    let mut names = Names::default();
    names.define("node_id", "node_id", &mut NewId::Required(&mut id))?;
    Ok((id, names.created))
}

/// `created` as the stored document has it. A first call stores the
/// document it built, so this changes nothing; a replay of an idempotent
/// `create_document` answers the document the first call stored, whose ids
/// were minted then. The same seed lays the same tree, so a node's place in
/// the one built now says which id it has in the one stored.
pub(crate) fn as_stored(
    created: Created,
    built: &WorkspaceDoc,
    stored: &WorkspaceDoc,
) -> Result<Created, ToolError> {
    let built = built.walk_all()?;
    let stored = stored.walk_all()?;
    if built.len() != stored.len() {
        return Ok(created);
    }
    let same_place: HashMap<String, String> = built
        .iter()
        .zip(&stored)
        .map(|((now, _), (then, _))| (now.to_storage(), then.to_storage()))
        .collect();
    Ok(created
        .into_iter()
        .map(|(key, value)| {
            let stored = value.as_str().and_then(|minted| same_place.get(minted));
            match stored {
                Some(id) => (key, Value::String(id.clone())),
                None => (key, value),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests;
