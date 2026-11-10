// SPDX-License-Identifier: MPL-2.0

//! Shared operations over a borrowed CRDT; this helper owns no lineage or identity.
use crate::errors::{Code, Error, Result};
use crate::ids::{ActionId, ReminderId};
use crate::personal_attributes::{
    EntityRef, PersonalAttributeValue, PersonalAttributes, validate_attribute_key,
};
use crate::reminders::{
    ActionKind, OccurrenceAction, ReminderReductionInput, ReminderRule, ReminderSettings,
    StoredReminder,
};
use loro::{LoroDoc, LoroValue, ValueOrContainer};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const C_PERSONAL_ATTRIBUTES: &str = "personal_attributes";
pub(crate) const C_REMINDERS: &str = "reminders";
pub(crate) const C_REMINDER_RULES: &str = "rules";
pub(crate) const C_REMINDER_ACTIONS: &str = "actions";
const C_REMINDER_DELETED: &str = "deleted";
pub(crate) const C_REMINDER_CLEARED_ACTIONS: &str = "cleared_actions";
const C_REMINDER_SETTINGS: &str = "settings";
const K_REMINDER_END_OF_DAY: &str = "end_of_day";
const K_REMINDER_DEFAULT_TONE: &str = "default_tone";
const K_REMINDER_WEEK_START: &str = "week_start";
const C_PERSONAL_DICTIONARY: &str = "personal_dictionary";

pub(crate) struct ProfileContent<'a> {
    pub(crate) doc: &'a LoroDoc,
}
impl ProfileContent<'_> {
    // ------------------------------------------------- personal attributes

    /// Set one personal attribute on one entity. The value is validated, then
    /// written as a single atomic register — a concurrent write to the same
    /// key resolves to one whole value, never a splice of both.
    pub fn set_personal_attribute(
        &self,
        entity: &EntityRef,
        key: &str,
        value: &PersonalAttributeValue,
    ) -> Result<()> {
        validate_attribute_key(key)?;
        value.validate()?;
        let root = self.doc.get_map(C_PERSONAL_ATTRIBUTES);
        // `ensure_mergeable_map`, never `get_or_create_container`: two of this
        // person's devices may each file the first attribute for one entity
        // before they meet, and both maps must merge rather than one winning.
        let entry = root
            .ensure_mergeable_map(&entity.to_storage())
            .map_err(loro_failed)?;
        entry.insert(key, value.to_loro()).map_err(loro_failed)?;
        self.doc.commit();
        Ok(())
    }

    /// Clear one personal attribute. Clearing deletes the key — absent is the
    /// stored spelling of "unset", never a stored null.
    ///
    /// Answers whether anything was actually removed, and is guarded on
    /// presence: a delete of an absent key is still an operation, and this
    /// call must not create the entity's map just to delete nothing from it.
    pub fn clear_personal_attribute(&self, entity: &EntityRef, key: &str) -> Result<bool> {
        validate_attribute_key(key)?;
        let root = self.doc.get_map(C_PERSONAL_ATTRIBUTES);
        let Some(ValueOrContainer::Container(container)) = root.get(&entity.to_storage()) else {
            return Ok(false);
        };
        let Ok(entry) = container.into_map() else {
            return Ok(false);
        };
        if entry.get(key).is_none() {
            return Ok(false);
        }
        entry.delete(key).map_err(loro_failed)?;
        self.doc.commit();
        Ok(true)
    }

    /// Every personal attribute one entity carries. A pure read: it creates
    /// no container, and an entity nothing was filed under answers an empty
    /// map. An entry whose stored shape this build does not recognise is
    /// skipped — absent, never an error.
    pub fn personal_attributes(&self, entity: &EntityRef) -> PersonalAttributes {
        let mut out = PersonalAttributes::new();
        let root = self.doc.get_map(C_PERSONAL_ATTRIBUTES);
        let Some(ValueOrContainer::Container(container)) = root.get(&entity.to_storage()) else {
            return out;
        };
        let Ok(entry) = container.into_map() else {
            return out;
        };
        entry.for_each(|key, value| {
            if let ValueOrContainer::Value(value) = value
                && let Some(decoded) = PersonalAttributeValue::from_loro(&value)
            {
                out.insert(key.to_string(), decoded);
            }
        });
        out
    }

    // ---------------------------------------------------------- reminders

    /// Create or replace one whole rule revision. The rule is one register so
    /// concurrent edits resolve to one complete value, never field-by-field.
    /// A deletion tombstone is independent and cannot be cleared here.
    pub fn put_reminder_rule(&self, rule: &ReminderRule) -> Result<()> {
        rule.validate()?;
        let root = self.doc.get_map(C_REMINDERS);
        let rules = root
            .ensure_mergeable_map(C_REMINDER_RULES)
            .map_err(loro_failed)?;
        rules
            .insert(&rule.id.to_storage(), rule.to_loro())
            .map_err(loro_failed)?;
        self.doc.commit();
        Ok(())
    }

    /// Record the grow-only delete fact. Repeating it is idempotent; no method
    /// removes this key, so a concurrent rule edit cannot resurrect the id.
    pub fn delete_reminder(&self, id: ReminderId) -> Result<()> {
        let root = self.doc.get_map(C_REMINDERS);
        let deleted = root
            .ensure_mergeable_map(C_REMINDER_DELETED)
            .map_err(loro_failed)?;
        deleted
            .insert(&id.to_storage(), true)
            .map_err(loro_failed)?;
        self.doc.commit();
        Ok(())
    }

    /// Tombstone every live rule for one authoritatively removed entity.
    /// Callers must establish removal; projection `Missing` is intentionally
    /// insufficient because a cold or stale projection is not deletion truth.
    pub fn delete_reminders_for_target(&self, target: &EntityRef) -> Result<Vec<ReminderId>> {
        let matching: Vec<_> = self
            .reminders()
            .into_iter()
            .filter(|reminder| !reminder.deleted && reminder.rule.target == *target)
            .map(|reminder| reminder.rule.id)
            .collect();
        if matching.is_empty() {
            return Ok(Vec::new());
        }
        let root = self.doc.get_map(C_REMINDERS);
        let deleted = root
            .ensure_mergeable_map(C_REMINDER_DELETED)
            .map_err(loro_failed)?;
        for reminder in &matching {
            deleted
                .insert(&reminder.to_storage(), true)
                .map_err(loro_failed)?;
        }
        self.doc.commit();
        Ok(matching)
    }

    /// Append one action below the reminder identity. An action id names one
    /// atomic value; retrying the same request writes the same map key.
    pub fn append_reminder_action(
        &self,
        reminder: ReminderId,
        action: &OccurrenceAction,
    ) -> Result<()> {
        let root = self.doc.get_map(C_REMINDERS);
        let actions = root
            .ensure_mergeable_map(C_REMINDER_ACTIONS)
            .map_err(loro_failed)?;
        let reminder_actions = actions
            .ensure_mergeable_map(&reminder.to_storage())
            .map_err(loro_failed)?;
        reminder_actions
            .insert(&action.id.to_storage(), action.to_loro())
            .map_err(loro_failed)?;
        self.doc.commit();
        Ok(())
    }

    // ------------------------------------------------- personal dictionary

    /// Add one word to the personal spelling dictionary for one language.
    /// Idempotent: adding a word twice stores it once, and two devices each
    /// adding their first word merge rather than one winning.
    pub fn add_dictionary_word(&self, language: &str, word: &str) -> Result<()> {
        validate_language_tag(language)?;
        validate_dictionary_word(word)?;
        let root = self.doc.get_map(C_PERSONAL_DICTIONARY);
        let entry = root.ensure_mergeable_map(language).map_err(loro_failed)?;
        entry.insert(word, true).map_err(loro_failed)?;
        self.doc.commit();
        Ok(())
    }

    /// Hide every handled action with a grow-only clear fact. Raw actions stay
    /// append-only, so a delayed replica or retry cannot resurrect history;
    /// snoozes and unhandled occurrences remain visible exactly as stored.
    pub fn clear_handled_reminder_history(&self) -> Result<usize> {
        self.clear_handled_reminder_history_matching(|_| true)
    }

    /// Hide handled actions belonging to reminders for exactly one entity.
    ///
    /// This uses the same grow-only facts as profile-wide clearing. Target
    /// scope chooses which existing action ids receive facts; it does not add
    /// a second history model or make a late replica able to resurrect one.
    pub fn clear_handled_reminder_history_for_target(&self, target: &EntityRef) -> Result<usize> {
        self.clear_handled_reminder_history_matching(|reminder_id| {
            ReminderId::parse(reminder_id)
                .and_then(|id| self.reminder(id))
                .is_some_and(|reminder| reminder.rule.target == *target)
        })
    }

    fn clear_handled_reminder_history_matching(
        &self,
        mut includes: impl FnMut(&str) -> bool,
    ) -> Result<usize> {
        let root = self.doc.get_map(C_REMINDERS);
        let Some(ValueOrContainer::Container(actions)) = root.get(C_REMINDER_ACTIONS) else {
            return Ok(0);
        };
        let Ok(actions) = actions.into_map() else {
            return Ok(0);
        };
        let mut pending = Vec::new();
        let ids: Vec<_> = actions.keys().map(|key| key.to_string()).collect();
        for reminder_id in ids {
            // Profile-wide clearing deliberately retains its pre-scope behavior
            // for every action bucket; target scope alone needs a rule lookup.
            if !includes(&reminder_id) {
                continue;
            }
            let Some(ValueOrContainer::Container(entries)) = actions.get(&reminder_id) else {
                continue;
            };
            let Ok(entries) = entries.into_map() else {
                continue;
            };
            let mut handled = Vec::new();
            entries.for_each(|key, value| match value {
                ValueOrContainer::Value(value)
                    if OccurrenceAction::from_loro(&value)
                        .is_some_and(|action| action.kind == ActionKind::Handled) =>
                {
                    handled.push(key.to_string());
                }
                _ => {}
            });
            for action in handled {
                pending.push((reminder_id.clone(), action));
            }
        }
        if pending.is_empty() {
            return Ok(0);
        }
        let cleared = root
            .ensure_mergeable_map(C_REMINDER_CLEARED_ACTIONS)
            .map_err(loro_failed)?;
        let mut removed = 0;
        for (reminder_id, action) in pending {
            let cleared_for_reminder = cleared
                .ensure_mergeable_map(&reminder_id)
                .map_err(loro_failed)?;
            if cleared_for_reminder.get(&action).is_none() {
                cleared_for_reminder
                    .insert(&action, true)
                    .map_err(loro_failed)?;
                removed += 1;
            }
        }
        if removed > 0 {
            self.doc.commit();
        }
        Ok(removed)
    }

    pub fn update_reminder_settings(
        &self,
        end_of_day: Option<Option<crate::reminders::LocalTime>>,
        default_tone: Option<crate::reminders::ToneId>,
        week_start: Option<crate::reminders::WeekStartPreference>,
    ) -> Result<bool> {
        let current = self.reminder_settings();
        let end_of_day = end_of_day.filter(|value| *value != current.end_of_day);
        let default_tone = default_tone.filter(|value| *value != current.default_tone);
        let week_start = week_start.filter(|value| *value != current.week_start);
        if end_of_day.is_none() && default_tone.is_none() && week_start.is_none() {
            return Ok(false);
        }
        let root = self.doc.get_map(C_REMINDERS);
        let stored = root
            .ensure_mergeable_map(C_REMINDER_SETTINGS)
            .map_err(loro_failed)?;
        if let Some(end_of_day) = end_of_day {
            match end_of_day {
                Some(value) => stored
                    .insert(K_REMINDER_END_OF_DAY, value.to_storage())
                    .map_err(loro_failed)?,
                None if stored.get(K_REMINDER_END_OF_DAY).is_some() => {
                    stored.delete(K_REMINDER_END_OF_DAY).map_err(loro_failed)?;
                }
                None => {}
            }
        }
        if let Some(default_tone) = default_tone {
            let value = serde_json::to_value(default_tone)
                .expect("tone is serializable")
                .as_str()
                .expect("tone serializes as a string")
                .to_owned();
            stored
                .insert(K_REMINDER_DEFAULT_TONE, value)
                .map_err(loro_failed)?;
        }
        if let Some(week_start) = week_start {
            let value = serde_json::to_value(week_start)
                .expect("week start is serializable")
                .as_str()
                .expect("week start serializes as a string")
                .to_owned();
            stored
                .insert(K_REMINDER_WEEK_START, value)
                .map_err(loro_failed)?;
        }
        self.doc.commit();
        Ok(true)
    }

    /// Remove one word. Absent is the stored spelling of "not in the
    /// dictionary", so removal deletes the key; answers whether anything was
    /// actually there, and a remove of an absent word creates nothing.
    pub fn remove_dictionary_word(&self, language: &str, word: &str) -> Result<bool> {
        validate_language_tag(language)?;
        let root = self.doc.get_map(C_PERSONAL_DICTIONARY);
        let Some(ValueOrContainer::Container(container)) = root.get(language) else {
            return Ok(false);
        };
        let Ok(entry) = container.into_map() else {
            return Ok(false);
        };
        if entry.get(word).is_none() {
            return Ok(false);
        }
        entry.delete(word).map_err(loro_failed)?;
        self.doc.commit();
        Ok(true)
    }

    /// Remove every word visible at this causal point. Each known word is
    /// deleted individually so a concurrent word first added by another
    /// device remains a normal CRDT merge rather than being hidden by deleting
    /// its whole language container.
    pub fn clear_dictionary(&self) -> Result<usize> {
        let words = self.dictionary();
        if words.is_empty() {
            return Ok(0);
        }
        let root = self.doc.get_map(C_PERSONAL_DICTIONARY);
        let mut removed = 0;
        for (language, words) in words {
            let Some(ValueOrContainer::Container(container)) = root.get(&language) else {
                continue;
            };
            let Ok(entry) = container.into_map() else {
                continue;
            };
            for word in words {
                entry.delete(&word).map_err(loro_failed)?;
                removed += 1;
            }
        }
        if removed > 0 {
            self.doc.commit();
        }
        Ok(removed)
    }

    pub fn reminder_settings(&self) -> ReminderSettings {
        let root = self.doc.get_map(C_REMINDERS);
        let Some(ValueOrContainer::Container(settings)) = root.get(C_REMINDER_SETTINGS) else {
            return ReminderSettings::default();
        };
        let Ok(settings) = settings.into_map() else {
            return ReminderSettings::default();
        };
        let end_of_day = string(&settings.get(K_REMINDER_END_OF_DAY))
            .and_then(|value| crate::reminders::LocalTime::parse(&value));
        let default_tone = string(&settings.get(K_REMINDER_DEFAULT_TONE))
            .and_then(|value| serde_json::from_value(serde_json::Value::String(value)).ok())
            .unwrap_or_default();
        let week_start = string(&settings.get(K_REMINDER_WEEK_START))
            .and_then(|value| serde_json::from_value(serde_json::Value::String(value)).ok())
            .unwrap_or_default();
        ReminderSettings {
            end_of_day,
            default_tone,
            week_start,
        }
    }

    pub fn reminder(&self, id: ReminderId) -> Option<StoredReminder> {
        self.reminders()
            .into_iter()
            .find(|reminder| reminder.rule.id == id)
    }

    /// Lookup for idempotency checks, including actions hidden by Clear all.
    pub fn raw_reminder_action(
        &self,
        reminder: ReminderId,
        action: ActionId,
    ) -> Option<OccurrenceAction> {
        let root = self.doc.get_map(C_REMINDERS);
        let actions = match root.get(C_REMINDER_ACTIONS)? {
            ValueOrContainer::Container(container) => container.into_map().ok()?,
            ValueOrContainer::Value(_) => return None,
        };
        let entries = match actions.get(&reminder.to_storage())? {
            ValueOrContainer::Container(container) => container.into_map().ok()?,
            ValueOrContainer::Value(_) => return None,
        };
        let value = match entries.get(&action.to_storage())? {
            ValueOrContainer::Value(value) => value,
            ValueOrContainer::Container(_) => return None,
        };
        OccurrenceAction::from_loro(&value).filter(|value| value.id == action)
    }

    /// Content-free identity/value view used to classify imported CRDT changes.
    /// Unlike [`Self::reminders`], this includes actions hidden by Clear all.
    pub(crate) fn raw_reminder_actions(
        &self,
    ) -> BTreeMap<(ReminderId, ActionId), OccurrenceAction> {
        let root = self.doc.get_map(C_REMINDERS);
        let Some(ValueOrContainer::Container(actions)) = root.get(C_REMINDER_ACTIONS) else {
            return BTreeMap::new();
        };
        let Ok(actions) = actions.into_map() else {
            return BTreeMap::new();
        };
        let mut out = BTreeMap::new();
        actions.for_each(|reminder_key, value| {
            let Some(reminder) = ReminderId::parse(reminder_key) else {
                return;
            };
            let ValueOrContainer::Container(entries) = value else {
                return;
            };
            let Ok(entries) = entries.into_map() else {
                return;
            };
            entries.for_each(|action_key, value| {
                let Some(action_id) = ActionId::parse(action_key) else {
                    return;
                };
                if let ValueOrContainer::Value(value) = value
                    && let Some(action) =
                        OccurrenceAction::from_loro(&value).filter(|value| value.id == action_id)
                {
                    out.insert((reminder, action_id), action);
                }
            });
        });
        out
    }

    /// Grow-only Clear all facts, separate from raw actions so remote imports
    /// can emit a history wake rather than misreporting an action append.
    pub(crate) fn cleared_reminder_actions(&self) -> BTreeSet<(ReminderId, ActionId)> {
        let root = self.doc.get_map(C_REMINDERS);
        let Some(ValueOrContainer::Container(cleared)) = root.get(C_REMINDER_CLEARED_ACTIONS)
        else {
            return BTreeSet::new();
        };
        let Ok(cleared) = cleared.into_map() else {
            return BTreeSet::new();
        };
        let mut out = BTreeSet::new();
        cleared.for_each(|reminder_key, value| {
            let Some(reminder) = ReminderId::parse(reminder_key) else {
                return;
            };
            let ValueOrContainer::Container(entries) = value else {
                return;
            };
            let Ok(entries) = entries.into_map() else {
                return;
            };
            entries.for_each(|action_key, value| {
                let Some(action) = ActionId::parse(action_key) else {
                    return;
                };
                if matches!(value, ValueOrContainer::Value(LoroValue::Bool(true))) {
                    out.insert((reminder, action));
                }
            });
        });
        out
    }

    /// Every recognized reminder, ordered by its opaque identity. Unknown
    /// future values are skipped without touching their bytes.
    pub fn reminders(&self) -> Vec<StoredReminder> {
        self.reminder_reduction_inputs()
            .into_iter()
            .map(ReminderReductionInput::into_stored)
            .collect()
    }

    /// Visible reminder records plus the content-free cleared handled facts
    /// needed for effective-state reduction. This is an in-process boundary;
    /// the additional facts never enter the public serialized record.
    pub(crate) fn reminder_reduction_inputs(&self) -> Vec<ReminderReductionInput> {
        let root = self.doc.get_map(C_REMINDERS);
        let Some(ValueOrContainer::Container(rules)) = root.get(C_REMINDER_RULES) else {
            return Vec::new();
        };
        let Ok(rules) = rules.into_map() else {
            return Vec::new();
        };
        let deleted = root.get(C_REMINDER_DELETED).and_then(|value| match value {
            ValueOrContainer::Container(container) => container.into_map().ok(),
            ValueOrContainer::Value(_) => None,
        });
        let actions = root.get(C_REMINDER_ACTIONS).and_then(|value| match value {
            ValueOrContainer::Container(container) => container.into_map().ok(),
            ValueOrContainer::Value(_) => None,
        });
        let cleared_actions = root
            .get(C_REMINDER_CLEARED_ACTIONS)
            .and_then(|value| match value {
                ValueOrContainer::Container(container) => container.into_map().ok(),
                ValueOrContainer::Value(_) => None,
            });
        let mut out = Vec::new();
        rules.for_each(|key, value| {
            let Some(key_id) = ReminderId::parse(key) else {
                return;
            };
            let ValueOrContainer::Value(value) = value else {
                return;
            };
            let Some(rule) = ReminderRule::from_loro(&value).filter(|rule| rule.id == key_id)
            else {
                return;
            };
            let is_deleted = deleted
                .as_ref()
                .and_then(|values| values.get(key))
                .is_some_and(|value| {
                    matches!(value, ValueOrContainer::Value(LoroValue::Bool(true)))
                });
            let mut rule_actions = Vec::new();
            let mut cleared_handled_occurrences = BTreeSet::new();
            if let Some(ValueOrContainer::Container(entries)) =
                actions.as_ref().and_then(|values| values.get(key))
                && let Ok(entries) = entries.into_map()
            {
                let cleared = cleared_actions
                    .as_ref()
                    .and_then(|values| values.get(key))
                    .and_then(|value| match value {
                        ValueOrContainer::Container(container) => container.into_map().ok(),
                        ValueOrContainer::Value(_) => None,
                    });
                entries.for_each(|action_key, value| {
                    let Some(key_id) = ActionId::parse(action_key) else {
                        return;
                    };
                    if let ValueOrContainer::Value(value) = value
                        && let Some(action) =
                            OccurrenceAction::from_loro(&value).filter(|action| action.id == key_id)
                    {
                        let is_cleared = cleared.as_ref().is_some_and(|values| {
                            values.get(action_key).is_some_and(|value| {
                                matches!(value, ValueOrContainer::Value(LoroValue::Bool(true)))
                            })
                        });
                        if is_cleared && action.kind == ActionKind::Handled {
                            cleared_handled_occurrences.insert(action.occurrence);
                        } else {
                            rule_actions.push(action);
                        }
                    }
                });
            }
            rule_actions.sort_by_key(|action| action.id);
            out.push(ReminderReductionInput::new(
                StoredReminder {
                    rule,
                    deleted: is_deleted,
                    actions: rule_actions,
                },
                cleared_handled_occurrences,
            ));
        });
        out.sort_by_key(|reminder| reminder.stored().rule.id);
        out
    }

    /// The whole dictionary: language tag → sorted words. A pure read; an
    /// empty dictionary answers an empty map, and an entry whose shape this
    /// build does not recognise is skipped rather than an error.
    pub fn dictionary(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let mut out = std::collections::BTreeMap::new();
        let root = self.doc.get_map(C_PERSONAL_DICTIONARY);
        root.for_each(|language, value| {
            let ValueOrContainer::Container(container) = value else {
                return;
            };
            let Ok(entry) = container.into_map() else {
                return;
            };
            let mut words: Vec<String> = Vec::new();
            entry.for_each(|word, _| words.push(word.to_string()));
            words.sort();
            if !words.is_empty() {
                out.insert(language.to_string(), words);
            }
        });
        out
    }
}
/// A BCP-47-shaped tag: ASCII letters, digits and hyphens, bounded. Not a
/// full BCP-47 parser — the store validates shape, and which tags mean a
/// dictionary is the application's business.
pub(super) fn validate_language_tag(language: &str) -> Result<()> {
    let shaped = (2..=35).contains(&language.len())
        && language
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !language.starts_with('-')
        && !language.ends_with('-');
    if shaped {
        Ok(())
    } else {
        Err(Error::new(
            Code::InvalidRequest,
            "a dictionary language is a BCP 47 tag",
        ))
    }
}

/// A dictionary word: non-empty, bounded, and one word — no whitespace and no
/// control characters. Counted in characters so the limit means the same
/// thing in every language.
pub(super) fn validate_dictionary_word(word: &str) -> Result<()> {
    let chars = word.chars().count();
    let shaped =
        (1..=100).contains(&chars) && !word.chars().any(|c| c.is_whitespace() || c.is_control());
    if shaped {
        Ok(())
    } else {
        Err(Error::new(
            Code::InvalidRequest,
            "a dictionary word is one bounded word",
        ))
    }
}

fn string(value: &Option<ValueOrContainer>) -> Option<String> {
    match value.as_ref()? {
        ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn loro_failed(error: loro::LoroError) -> Error {
    Error::new(Code::StorageFailed, error.to_string())
}
