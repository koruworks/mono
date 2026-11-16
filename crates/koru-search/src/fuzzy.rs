// SPDX-License-Identifier: MPL-2.0

//! Typo tolerance: a BK-tree over the normalized vocabulary the index holds.
//!
//! **A copy of the old app search index's `fuzzy.rs`**, for the reason given
//! at the top of `normalize.rs`. Do not fix a bug in one without the other.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Instant;

use strsim::damerau_levenshtein;

use crate::{FuzzyIndexState, FuzzyIndexStats};

const MAX_ALTERNATIVES: usize = 8;

/// Which part of the vocabulary a write changes.
///
/// A task list's name is searchable only by a caller that asked for lists,
/// and **it must not change anybody else's typo tolerance**: the eight
/// alternatives for a mistyped word are chosen from the whole vocabulary
/// before any fragment is filtered, so a list named with a neighbour of a
/// document's word could push that word out of the eight, and a list sharing
/// a word with documents could reorder them. So every writer says which part
/// its words are, and a lists-off query reads the counts as they would be if
/// no list were indexed.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Part {
    /// Words of `task-list` fragments.
    Lists,
    /// Everything else: workspace names, titles and the node walk.
    Rest,
}
const COMPACT_MIN_INACTIVE: usize = 1_000;

#[derive(Clone)]
pub(crate) struct SharedVocabulary(Arc<RwLock<Vocabulary>>);

impl SharedVocabulary {
    pub(crate) fn warming() -> Self {
        Self(Arc::new(RwLock::new(Vocabulary::default())))
    }

    pub(crate) fn ready() -> Self {
        let vocabulary = Vocabulary {
            state: FuzzyIndexState::Ready,
            ..Vocabulary::default()
        };
        Self(Arc::new(RwLock::new(vocabulary)))
    }

    pub(crate) fn read(&self) -> RwLockReadGuard<'_, Vocabulary> {
        self.0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, Vocabulary> {
        self.0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn stats(&self) -> FuzzyIndexStats {
        self.read().stats()
    }

    /// Build a compact replacement without holding the read/write lock, then
    /// publish only if no partition delta changed the active vocabulary while
    /// it was being built. Queries keep using the previous complete tree.
    pub(crate) fn compact_if_needed(&self, built_at_ms: i64) -> bool {
        let (counts, list_counts) = {
            let vocabulary = self.read();
            if !vocabulary.should_compact() {
                return false;
            }
            (vocabulary.counts.clone(), vocabulary.list_counts.clone())
        };
        let mut replacement = Vocabulary {
            state: FuzzyIndexState::Rebuilding,
            ..Vocabulary::default()
        };
        replacement.replace_all(counts.clone(), list_counts.clone(), built_at_ms);
        let mut vocabulary = self.write();
        // Republished only if neither part moved while the tree was rebuilt.
        if vocabulary.counts != counts
            || vocabulary.list_counts != list_counts
            || !vocabulary.should_compact()
        {
            return false;
        }
        *vocabulary = replacement;
        true
    }
}

#[derive(Default)]
pub(crate) struct Vocabulary {
    root: Option<Box<BkNode>>,
    counts: HashMap<String, u64>,
    /// The part of each term's count that `task-list` fragments contribute.
    /// [`counts`](Self::counts) stays the total, so the tree, compaction and
    /// the stats are what they were; this is what a lists-off query takes
    /// away. One more map with the lifetime of the one beside it.
    list_counts: HashMap<String, u64>,
    tree_nodes: usize,
    inactive_nodes: usize,
    state: FuzzyIndexState,
    last_build_ms: Option<u64>,
    last_built_at_ms: Option<i64>,
}

impl Vocabulary {
    /// The alternatives for a mistyped word: every active term within the
    /// allowed distance, nearest first, then most frequent, and **eight
    /// kept**.
    ///
    /// With `include_lists` off, a term's frequency is its count less the
    /// lists' share, and a term nothing but lists use is not a candidate —
    /// both decided **before** the sort and the cut. That is exactly the set
    /// and the frequencies the vocabulary would hold if no list were
    /// indexed, so the eight are the same eight in the same order. The tree
    /// still holds list-only terms as nodes either way: a BK-tree's answer
    /// does not depend on which other terms are in it, only its speed does.
    pub(crate) fn candidates(&self, query: &str, include_lists: bool) -> Vec<FuzzyCandidate> {
        if self.state != FuzzyIndexState::Ready {
            return Vec::new();
        }
        let max_distance = allowed_distance(query);
        if max_distance == 0 {
            return Vec::new();
        }
        let mut found = Vec::new();
        if let Some(root) = &self.root {
            root.search(query, max_distance, &self.counts, &mut found);
        }
        if !include_lists {
            found.retain_mut(|candidate| {
                let lists = self.list_counts.get(&candidate.term).copied().unwrap_or(0);
                candidate.frequency = candidate.frequency.saturating_sub(lists);
                candidate.frequency > 0
            });
        }
        found.retain(|candidate| candidate.term != query);
        found.sort_by(|left, right| {
            left.distance
                .cmp(&right.distance)
                .then_with(|| right.frequency.cmp(&left.frequency))
                .then_with(|| left.term.cmp(&right.term))
        });
        found.truncate(MAX_ALTERNATIVES);
        found
    }

    /// Replace both parts: `counts` is every fragment's words, and
    /// `list_counts` the share of them that `task-list` fragments hold.
    pub(crate) fn replace_all(
        &mut self,
        counts: HashMap<String, u64>,
        list_counts: HashMap<String, u64>,
        built_at_ms: i64,
    ) {
        let started = Instant::now();
        let mut terms: Vec<_> = counts.keys().cloned().collect();
        terms.sort_by_key(|term| std::cmp::Reverse(counts.get(term).copied().unwrap_or(0)));
        self.root = None;
        self.tree_nodes = 0;
        for term in terms {
            self.insert_tree_term(term);
        }
        self.counts = counts;
        self.list_counts = list_counts;
        self.inactive_nodes = 0;
        self.state = FuzzyIndexState::Ready;
        self.last_build_ms = Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        self.last_built_at_ms = Some(built_at_ms);
    }

    /// Apply one writer's change to the counts. `part` says whose words they
    /// are: a [`Part::Lists`] delta moves the total and the lists' share
    /// together, and a [`Part::Rest`] delta the total alone.
    ///
    /// A term is inactive when its **total** is zero, as it always was; a
    /// term only lists use is active and simply not eligible for a lists-off
    /// query.
    pub(crate) fn apply_delta(&mut self, delta: HashMap<String, i64>, part: Part) {
        for (term, change) in delta {
            if change == 0 {
                continue;
            }
            if part == Part::Lists {
                let lists = self.list_counts.get(&term).copied().unwrap_or(0);
                let next = if change.is_negative() {
                    lists.saturating_sub(change.unsigned_abs())
                } else {
                    lists.saturating_add(change as u64)
                };
                if next == 0 {
                    self.list_counts.remove(&term);
                } else {
                    self.list_counts.insert(term.clone(), next);
                }
            }
            let previous = self.counts.get(&term).copied().unwrap_or(0);
            let next = if change.is_negative() {
                previous.saturating_sub(change.unsigned_abs())
            } else {
                previous.saturating_add(change as u64)
            };
            if next == 0 {
                if self.counts.remove(&term).is_some() && previous > 0 {
                    self.inactive_nodes = self.inactive_nodes.saturating_add(1);
                }
            } else {
                if previous == 0 {
                    self.insert_tree_term(term.clone());
                }
                self.counts.insert(term, next);
            }
        }
    }

    pub(crate) fn should_compact(&self) -> bool {
        self.inactive_nodes >= COMPACT_MIN_INACTIVE
            && self.inactive_nodes.saturating_mul(5) >= self.tree_nodes
    }

    fn insert_tree_term(&mut self, term: String) {
        if let Some(root) = &mut self.root {
            if root.insert(term) {
                self.tree_nodes += 1;
            }
        } else {
            self.root = Some(Box::new(BkNode::new(term)));
            self.tree_nodes = 1;
        }
    }

    fn stats(&self) -> FuzzyIndexStats {
        FuzzyIndexStats {
            state: self.state,
            active_terms: self.counts.len() as u64,
            tree_nodes: self.tree_nodes as u64,
            inactive_nodes: self.inactive_nodes as u64,
            last_build_ms: self.last_build_ms,
            last_built_at_ms: self.last_built_at_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FuzzyCandidate {
    pub term: String,
    pub distance: usize,
    pub frequency: u64,
}

struct BkNode {
    term: String,
    children: HashMap<usize, Box<BkNode>>,
}

impl BkNode {
    fn new(term: String) -> Self {
        Self {
            term,
            children: HashMap::new(),
        }
    }

    fn insert(&mut self, term: String) -> bool {
        let distance = damerau_levenshtein(&self.term, &term);
        if distance == 0 {
            return false;
        }
        if let Some(child) = self.children.get_mut(&distance) {
            child.insert(term)
        } else {
            self.children.insert(distance, Box::new(Self::new(term)));
            true
        }
    }

    fn search(
        &self,
        query: &str,
        max_distance: usize,
        active: &HashMap<String, u64>,
        found: &mut Vec<FuzzyCandidate>,
    ) {
        let distance = damerau_levenshtein(&self.term, query);
        if distance <= max_distance
            && let Some(frequency) = active.get(&self.term)
        {
            found.push(FuzzyCandidate {
                term: self.term.clone(),
                distance,
                frequency: *frequency,
            });
        }
        let lower = distance.saturating_sub(max_distance);
        let upper = distance.saturating_add(max_distance);
        for (_, child) in self
            .children
            .iter()
            .filter(|(edge, _)| **edge >= lower && **edge <= upper)
        {
            child.search(query, max_distance, active, found);
        }
    }
}

fn allowed_distance(term: &str) -> usize {
    if !term.chars().all(char::is_alphabetic) {
        return 0;
    }
    match term.chars().count() {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocabulary(words: &[&str]) -> Vocabulary {
        let counts = words.iter().map(|word| ((*word).into(), 1)).collect();
        let mut vocabulary = Vocabulary::default();
        vocabulary.replace_all(counts, HashMap::new(), 1);
        vocabulary
    }

    fn terms(found: &[FuzzyCandidate]) -> Vec<(&str, u64)> {
        found
            .iter()
            .map(|candidate| (candidate.term.as_str(), candidate.frequency))
            .collect()
    }

    /// **Lists off is the vocabulary with no list in it.** Eight lists named
    /// with one-edit neighbours of `book` that sort ahead of `cook` would
    /// fill the eight on their own; a lists-off query still finds `cook`.
    #[test]
    fn list_only_words_do_not_displace_a_documents_word() {
        let mut vocabulary = vocabulary(&["cook"]);
        let before = vocabulary.candidates("book", false);
        assert_eq!(terms(&before), vec![("cook", 1)]);

        let neighbours = [
            "bock", "bonk", "boog", "boom", "boon", "boor", "boos", "boot",
        ];
        vocabulary.apply_delta(
            neighbours.iter().map(|word| ((*word).into(), 1)).collect(),
            Part::Lists,
        );
        assert_eq!(
            vocabulary.candidates("book", false),
            before,
            "the same answer, in the same order"
        );
        let with_lists = vocabulary.candidates("book", true);
        assert_eq!(with_lists.len(), MAX_ALTERNATIVES);
        assert!(
            with_lists.iter().all(|candidate| candidate.term != "cook"),
            "with lists on, the eight are the lists' words: {with_lists:?}"
        );
    }

    /// A list named after a word documents already hold raises that word's
    /// total, which would reorder the eight; lists off reads the documents'
    /// share alone.
    #[test]
    fn a_lists_use_of_a_shared_word_does_not_reorder_the_alternatives() {
        let mut vocabulary = Vocabulary::default();
        vocabulary.replace_all(
            HashMap::from([("cook".into(), 3), ("hook".into(), 2)]),
            HashMap::new(),
            1,
        );
        let before = vocabulary.candidates("book", false);
        assert_eq!(terms(&before), vec![("cook", 3), ("hook", 2)]);

        vocabulary.apply_delta(HashMap::from([("hook".into(), 5)]), Part::Lists);
        assert_eq!(vocabulary.candidates("book", false), before);
        assert_eq!(
            terms(&vocabulary.candidates("book", true)),
            vec![("hook", 7), ("cook", 3)],
            "with lists on the totals rank"
        );

        // The list is renamed away: both parts fall together.
        vocabulary.apply_delta(HashMap::from([("hook".into(), -5)]), Part::Lists);
        assert_eq!(vocabulary.candidates("book", true), before);
    }

    /// A word only a list uses is active — it is in the tree and counted in
    /// the total — and a typo of it finds it when lists are asked for.
    #[test]
    fn a_typo_of_a_list_only_word_finds_it_with_lists_on() {
        let mut vocabulary = vocabulary(&["should"]);
        vocabulary.apply_delta(HashMap::from([("backlog".into(), 1)]), Part::Lists);
        assert!(vocabulary.candidates("backlgo", false).is_empty());
        assert_eq!(
            terms(&vocabulary.candidates("backlgo", true)),
            vec![("backlog", 1)]
        );
        // Deleted: inactive, as any term whose total reaches zero is.
        vocabulary.apply_delta(HashMap::from([("backlog".into(), -1)]), Part::Lists);
        assert!(vocabulary.candidates("backlgo", true).is_empty());
    }

    /// Warming builds both parts, and a compaction carries both across.
    #[test]
    fn warming_and_compaction_keep_the_lists_share() {
        let shared = SharedVocabulary::warming();
        shared.write().replace_all(
            HashMap::from([("cook".into(), 1), ("boot".into(), 1)]),
            HashMap::from([("boot".into(), 1)]),
            1,
        );
        assert_eq!(terms(&shared.read().candidates("book", false)), vec![("cook", 1)]);

        // Enough churn to earn a compaction.
        let churn: Vec<String> = (0..COMPACT_MIN_INACTIVE + 10)
            .map(|i| format!("zz{i:06}"))
            .collect();
        shared.write().apply_delta(
            churn.iter().map(|word| (word.clone(), 1)).collect(),
            Part::Rest,
        );
        shared.write().apply_delta(
            churn.iter().map(|word| (word.clone(), -1)).collect(),
            Part::Rest,
        );
        assert!(shared.compact_if_needed(2));
        assert_eq!(terms(&shared.read().candidates("book", false)), vec![("cook", 1)]);
        assert_eq!(shared.read().candidates("book", true).len(), 2);
    }

    #[test]
    fn deletion_and_adjacent_transposition_cost_one_edit() {
        let vocabulary = vocabulary(&["should", "would", "shoulder"]);
        assert!(
            vocabulary
                .candidates("hould", false)
                .iter()
                .any(|candidate| candidate.term == "should")
        );
        assert!(
            vocabulary
                .candidates("shoudl", false)
                .iter()
                .any(|candidate| candidate.term == "should")
        );
    }

    #[test]
    fn short_and_non_word_terms_are_not_expanded() {
        let vocabulary = vocabulary(&["cat", "1234", "should"]);
        assert!(vocabulary.candidates("cat", false).is_empty());
        assert!(vocabulary.candidates("1234", false).is_empty());
    }

    #[test]
    fn inactive_terms_never_return_from_the_tree() {
        let mut vocabulary = vocabulary(&["should"]);
        vocabulary.apply_delta(HashMap::from([("should".into(), -1)]), Part::Rest);
        assert!(vocabulary.candidates("hould", false).is_empty());
    }
}
