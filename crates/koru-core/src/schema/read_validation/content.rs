// SPDX-License-Identifier: MPL-2.0
use super::*;

const METADATA_LIMIT: usize = 8192;
const ATTRIBUTE_LIMIT: usize = 256;
const RUN_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeContentReadError {
    InvalidContent,
    UnsupportedDetail,
    TooLarge,
}
use NodeContentReadError as C;
type ContentResult<T> = std::result::Result<T, C>;

#[derive(Clone, Debug, PartialEq)]
pub struct NodeContentRead {
    pub content: Content,
    pub runs: Vec<TextRun>,
}

fn charge(remaining: &mut usize, bytes: usize) -> ContentResult<()> {
    *remaining = remaining.checked_sub(bytes).ok_or(C::TooLarge)?;
    Ok(())
}

fn string(value: ValueOrContainer, remaining: &mut usize) -> ContentResult<String> {
    let ValueOrContainer::Value(LoroValue::String(raw)) = value else {
        return Err(C::InvalidContent);
    };
    charge(remaining, raw.len())?;
    Ok(raw.to_string())
}

fn optional_string(
    meta: &LoroMap,
    key: &str,
    remaining: &mut usize,
) -> ContentResult<Option<String>> {
    meta.get(key)
        .map(|value| string(value, remaining))
        .transpose()
}

fn attributes(meta: &LoroMap, remaining: &mut usize) -> ContentResult<Vec<Attr>> {
    let Some(raw) = meta.get(K_ATTRS) else {
        return Ok(Vec::new());
    };
    let ValueOrContainer::Container(Container::Map(map)) = raw else {
        return Err(C::InvalidContent);
    };
    if map.len() > ATTRIBUTE_LIMIT {
        return Err(C::TooLarge);
    }
    let mut ids = HashSet::new();
    for key in map.keys() {
        charge(remaining, key.len())?;
        let id = AttrId::parse(&key).ok_or(C::InvalidContent)?;
        if !ids.insert(id) {
            return Err(C::InvalidContent);
        }
        let Some(ValueOrContainer::Container(Container::Map(record))) = map.get(&key) else {
            return Err(C::InvalidContent);
        };
        // Only scalar records owned by the existing typed attribute decoder.
        for field in record.keys() {
            charge(remaining, field.len())?;
            if ![
                K_ATTR_ROLE,
                K_ATTR_TYPE,
                K_ATTR_VALUE,
                K_ATTR_LABEL,
                K_ATTR_HOME,
                K_ATTR_TZ,
                K_ATTR_TARGET,
            ]
            .contains(&field.as_str())
            {
                return Err(C::UnsupportedDetail);
            }
            match record.get(&field) {
                Some(ValueOrContainer::Value(LoroValue::String(value))) => {
                    charge(remaining, value.len())?
                }
                Some(ValueOrContainer::Value(
                    LoroValue::I64(_) | LoroValue::Double(_) | LoroValue::Bool(_),
                )) if field.as_str() == K_ATTR_VALUE => {}
                _ => return Err(C::InvalidContent),
            }
        }
        let role = record
            .get(K_ATTR_ROLE)
            .and_then(as_string)
            .ok_or(C::InvalidContent)?;
        let token = record
            .get(K_ATTR_TYPE)
            .and_then(as_string)
            .ok_or(C::InvalidContent)?;
        let value = read_attr_value(&record, &role).ok_or(C::UnsupportedDetail)?;
        if matches!(value, AttrValue::Number(number) if !number.is_finite()) {
            return Err(C::InvalidContent);
        }
        if token == "ref" {
            if let Some(home) = record.get(K_ATTR_HOME).and_then(as_string) {
                if read_ref_home(&home).is_none() {
                    return Err(C::UnsupportedDetail);
                }
            }
            if matches!(value, AttrValue::Document(_)) {
                if let Some(target) = record.get(K_ATTR_TARGET).and_then(as_string) {
                    NodeId::parse(&target).ok_or(C::InvalidContent)?;
                }
            }
        }
    }
    let attrs = read_attrs(meta);
    if attrs.len() != ids.len() || attrs.iter().any(|a| !ids.remove(&a.id)) || !ids.is_empty() {
        return Err(C::InvalidContent);
    }
    Ok(attrs)
}

fn runs(
    text: &loro::LoroText,
    attrs: &[Attr],
    remaining: &mut usize,
) -> ContentResult<Vec<TextRun>> {
    let deltas = text.to_delta();
    if deltas.len() > RUN_LIMIT {
        return Err(C::TooLarge);
    }
    for delta in &deltas {
        let TextDelta::Insert { insert, attributes } = delta else {
            return Err(C::InvalidContent);
        };
        if let Some(marks) = attributes {
            for (key, value) in marks {
                charge(remaining, key.len())?;
                if let LoroValue::String(value) = value {
                    charge(remaining, value.len())?;
                }
                let valid = if Mark::ALL.iter().any(|mark| mark.key() == key.as_str()) {
                    matches!(value, LoroValue::Bool(_))
                } else {
                    match key.as_str() {
                        SCRIPT_KEY => {
                            matches!(value, LoroValue::String(s) if Script::parse(s).is_some())
                        }
                        SIZE_KEY => match value {
                            LoroValue::I64(number) => (*number as f32).is_finite(),
                            LoroValue::Double(number) => {
                                number.is_finite() && (*number as f32).is_finite()
                            }
                            _ => false,
                        },
                        HIGHLIGHT_KEY => matches!(value, LoroValue::String(_)),
                        PILL_KEY => {
                            matches!(value, LoroValue::String(s) if AttrId::parse(s).is_some_and(|id| attrs.iter().any(|a| a.id == id)) && !insert.is_empty() && insert.chars().all(|c| c == crate::text::ANCHOR))
                        }
                        _ => return Err(C::UnsupportedDetail),
                    }
                };
                if !valid {
                    return Err(C::InvalidContent);
                }
            }
        }
    }
    drop(deltas);
    let mut output = Vec::new();
    for run in runs_from_text(text) {
        if run.pill.is_some() {
            let count = run.text.len() / ANCHOR_LEN;
            if output.len() + count > RUN_LIMIT {
                return Err(C::TooLarge);
            }
            let mut template = run;
            template.text = ANCHOR_STR.to_owned();
            for i in 0..count {
                let mut single = template.clone();
                single.start = Utf8Pos(template.start.0 + i * ANCHOR_LEN);
                single.end = Utf8Pos(single.start.0 + ANCHOR_LEN);
                output.push(single);
            }
        } else {
            if output.len() == RUN_LIMIT {
                return Err(C::TooLarge);
            }
            output.push(run);
        }
    }
    Ok(output)
}

impl WorkspaceDoc {
    /// Strict bounded content projection. No container repair, target fetch, or
    /// compatibility change to ordinary readers. Delta allocation is Loro-owned.
    pub fn read_node_content_strict(
        &self,
        id: NodeId,
        text_budget: usize,
    ) -> ContentResult<NodeContentRead> {
        let tid = self.tree_id(id).map_err(|_| C::InvalidContent)?;
        let meta = self.nodes().get_meta(tid).map_err(|_| C::InvalidContent)?;
        let mut budget = METADATA_LIMIT;
        if let Some(target) = optional_string(&meta, K_EMBED, &mut budget)? {
            let node = NodeId::parse(&target).ok_or(C::InvalidContent)?;
            return Ok(NodeContentRead {
                content: Content::Embed {
                    target: Ref::new(node),
                },
                runs: Vec::new(),
            });
        }
        let attrs = attributes(&meta, &mut budget)?;
        if let Some(hash) = optional_string(&meta, K_BLOB, &mut budget)? {
            let mime = optional_string(&meta, K_MIME, &mut budget)?.unwrap_or_default();
            let dimension = |key| -> ContentResult<Option<u32>> {
                meta.get(key)
                    .map(|value| {
                        as_i64(value)
                            .and_then(|n| u32::try_from(n).ok())
                            .ok_or(C::InvalidContent)
                    })
                    .transpose()
            };
            return Ok(NodeContentRead {
                content: Content::Blob {
                    hash: BlobHash(hash),
                    mime,
                    width: dimension(K_WIDTH)?,
                    height: dimension(K_HEIGHT)?,
                    attrs,
                },
                runs: Vec::new(),
            });
        }
        let text = existing_text(&meta).map_err(|_| C::InvalidContent)?;
        if text.len_utf8() > text_budget {
            return Err(C::TooLarge);
        }
        let runs = runs(&text, &attrs, &mut budget)?;
        Ok(NodeContentRead {
            content: Content::Text {
                text: text.to_string(),
                attrs,
            },
            runs,
        })
    }
}

#[cfg(test)]
mod tests;
