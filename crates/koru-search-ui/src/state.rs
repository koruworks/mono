// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use koru_api_client::SearchHit;
use koru_store_wire::search::{LifecycleFilter, SearchFacet, SearchSort};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SearchSelectionContext {
    pub(super) scope: String,
    pub(super) profile: Option<String>,
    pub(super) query: String,
    pub(super) sort: SearchSort,
    pub(super) lifecycle: LifecycleFilter,
    pub(super) archived_only: bool,
    pub(super) include_ai: bool,
    pub(super) excluded_workspaces: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SearchSessionSelection {
    pub(super) context: SearchSelectionContext,
    pub(super) hit_key: String,
    pub(super) index: usize,
}

/// What this surface sorts by until somebody says otherwise.
///
/// **Not `SearchSort::default()`, deliberately.** That one is the *wire's*
/// default — what the store does for a caller that names no sort — and it is
/// relevance, because a query with no opinion about ordering wants its best
/// answer first. This is a product choice on top of it: what people come to
/// global search for is the thing they were just working on, and relevance
/// ranked a three-year-old page above this morning's whenever both said the
/// word. Recency-first, with relevance picking which fragment of a document is
/// the one row shown ([`keep_top_hit_per_document`]), answers both.
pub(super) const DEFAULT_SORT: SearchSort = SearchSort::ObservedModified;

/// Search choices that survive closing and reopening a surface in this app
/// session. Query text and the focused result are deliberately process-local:
/// neither belongs in collaborative content or on disk.
///
/// **Profile and sort outlive the process.** The Agent is quit on shut-down,
/// replaced by an update, restarted by the store supervisor — without a file
/// those two choices reset every time. See [`persist_choices_in`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SearchSessionState {
    pub(super) query: String,
    pub(super) sort: SearchSort,
    pub(super) profile: Option<String>,
    pub(super) include_ai: bool,
    pub(super) selection: Option<SearchSessionSelection>,
}

impl Default for SearchSessionState {
    fn default() -> Self {
        Self {
            query: String::new(),
            sort: DEFAULT_SORT,
            profile: None,
            include_ai: true,
            selection: None,
        }
    }
}

thread_local! {
    static SEARCH_SESSION: RefCell<SearchSessionState> = RefCell::new(SearchSessionState::default());
    /// Whether this thread has already merged the on-disk choices into
    /// [`SEARCH_SESSION`]. Without it, a person who chose Relevance would be
    /// overwritten by the file (or the default) on the next `search_session`
    /// call if we only gated on `profile.is_none()`.
    static DISK_LOADED: RefCell<bool> = const { RefCell::new(false) };
}

/// Where profile and sort are kept between runs of this process, if anywhere.
///
/// **The host says, and until it does nothing is written.** Two reasons it is
/// not a path this crate works out for itself: an agent started with an
/// explicit `--root` must write under that root rather than into the person's
/// real `%LOCALAPPDATA%`, and only the host knows its root; and a host's *unit
/// tests* build surfaces by the dozen, so a location taken by default would
/// mean a `cargo test` run rewriting the choices of whoever ran it. Silence is
/// the safe answer, and it is the default. The same reasoning as
/// `SearchSurface::start_reader`'s.
static STATE_FILE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Keep profile and sort choices in `path` from now on. Call once, before the
/// first surface; a second call is ignored rather than racing the first.
pub fn persist_choices_in(path: Option<PathBuf>) {
    let _ = STATE_FILE.set(path);
}

fn state_file() -> Option<&'static Path> {
    STATE_FILE.get()?.as_deref()
}

pub(super) fn search_session() -> SearchSessionState {
    SEARCH_SESSION.with(|session| {
        let mut state = session.borrow_mut();
        // The first ask in this thread is the one that reads the disk: a
        // process that never opens search never touches the file, and one that
        // opens it repeatedly reads it once.
        let already = DISK_LOADED.with(|loaded| *loaded.borrow());
        if !already {
            let stored = stored_choices();
            if state.profile.is_none() {
                state.profile = stored.profile;
            }
            // Prefer the file over the in-memory default so a restarted Agent
            // reopens on the sort the person last picked.
            if let Some(sort) = stored.sort {
                state.sort = sort;
            }
            if let Some(include_ai) = stored.include_ai {
                state.include_ai = include_ai;
            }
            DISK_LOADED.with(|loaded| *loaded.borrow_mut() = true);
        }
        state.clone()
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StoredChoices {
    profile: Option<String>,
    sort: Option<SearchSort>,
    include_ai: Option<bool>,
}

/// What the last run left, if the host gave us somewhere to keep it.
fn stored_choices() -> StoredChoices {
    let Some(path) = state_file() else {
        return StoredChoices::default();
    };
    stored_choices_at(path)
}

fn stored_choices_at(path: &Path) -> StoredChoices {
    let Ok(text) = std::fs::read_to_string(path) else {
        return StoredChoices::default();
    };
    choices_in(&text)
}

fn choices_in(text: &str) -> StoredChoices {
    let mut choices = StoredChoices::default();
    for (key, value) in text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
    {
        match key {
            "profile" if !value.is_empty() => choices.profile = Some(value.to_owned()),
            "sort" => choices.sort = sort_from_token(value),
            "include_ai" => choices.include_ai = bool_from_token(value),
            _ => {}
        }
    }
    choices
}

fn bool_from_token(token: &str) -> Option<bool> {
    match token {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn sort_token(sort: SearchSort) -> &'static str {
    match sort {
        SearchSort::Relevance => "relevance",
        SearchSort::ObservedModified => "observed_modified",
        SearchSort::LastUsedWorkspace => "last_used_workspace",
    }
}

fn sort_from_token(token: &str) -> Option<SearchSort> {
    match token {
        "relevance" => Some(SearchSort::Relevance),
        "observed_modified" | "last_modified" => Some(SearchSort::ObservedModified),
        "last_used_workspace" | "last_used" => Some(SearchSort::LastUsedWorkspace),
        _ => None,
    }
}

/// **Written on the choice, not on a timer.** Choosing a profile or a sort is
/// a click a person makes seldom, so one small synchronous write costs nothing
/// a person can perceive — and the alternative, a worker thread, would have to
/// outlive an Agent that is quit precisely when this matters most.
fn store_choices(profile: Option<&str>, sort: SearchSort, include_ai: bool) {
    let Some(path) = state_file() else { return };
    store_choices_at(path, profile, sort, include_ai);
}

fn store_choices_at(path: &Path, profile: Option<&str>, sort: SearchSort, include_ai: bool) {
    let current = stored_choices_at(path);
    if current.profile.as_deref() == profile
        && current.sort == Some(sort)
        && current.include_ai == Some(include_ai)
    {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, rendered(profile, sort, include_ai));
}

fn rendered(profile: Option<&str>, sort: SearchSort, include_ai: bool) -> String {
    let mut out = String::from(
        "# What Koru's search reopens on, on THIS machine.\n\
         # 'profile' is a Store profile id, not a folder or an account.\n\
         # 'sort' is relevance | observed_modified | last_used_workspace.\n\
         # 'include_ai' controls whether finalized AI chats join results.\n",
    );
    if let Some(profile) = profile {
        out.push_str(&format!("profile = {profile}\n"));
    }
    // Always write sort so a fresh default is not re-applied after the person
    // picked something else and then the profile line is rewritten alone.
    out.push_str(&format!("sort = {}\n", sort_token(sort)));
    out.push_str(&format!("include_ai = {include_ai}\n"));
    out
}

/// Merge a partial update into the file without dropping the other field.
fn persist_disk(profile: Option<Option<&str>>, sort: Option<SearchSort>, include_ai: Option<bool>) {
    let Some(_) = state_file() else { return };
    let current = stored_choices();
    let profile = match profile {
        Some(value) => value.map(str::to_owned),
        None => current.profile,
    };
    let sort = sort.or(current.sort).unwrap_or(DEFAULT_SORT);
    let include_ai = include_ai.or(current.include_ai).unwrap_or(true);
    store_choices(profile.as_deref(), sort, include_ai);
}

pub(super) fn remember_query(query: &str) {
    SEARCH_SESSION.with(|session| session.borrow_mut().query = query.to_owned());
}

pub(super) fn remember_sort(sort: SearchSort) {
    let changed = SEARCH_SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let changed = session.sort != sort;
        session.sort = sort;
        changed
    });
    if changed {
        persist_disk(None, Some(sort), None);
    }
}

pub(super) fn remember_profile(profile: Option<&str>) {
    let changed = SEARCH_SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let changed = session.profile.as_deref() != profile;
        session.profile = profile.map(str::to_owned);
        changed
    });
    // A resolved profile is worth keeping; *losing* one is not. Search falling
    // back to the catalog's first entry — because the store is still starting,
    // or a profile was removed on another machine — must not overwrite the
    // answer a person gave.
    if changed && profile.is_some() {
        persist_disk(Some(profile), None, None);
    }
}

pub(super) fn remember_include_ai(include_ai: bool) {
    let changed = SEARCH_SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let changed = session.include_ai != include_ai;
        session.include_ai = include_ai;
        changed
    });
    if changed {
        persist_disk(None, None, Some(include_ai));
    }
}

pub(super) fn remember_selection(selection: Option<SearchSessionSelection>) {
    SEARCH_SESSION.with(|session| session.borrow_mut().selection = selection);
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum SearchMenu {
    #[default]
    None,
    Sort,
    /// Built from the Store catalog. Choosing one changes the authorization
    /// scope and refreshes against only the workspaces that profile owns.
    Profiles,
    Workspaces,
    /// The query box's `#tag` completion.
    ///
    /// **A menu rather than a surface of its own**, so it inherits everything
    /// the others already settled: the anchor and clamping through
    /// `PopoverShell`, the row count, and — the load-bearing one —
    /// `menu_selected`, which is kept apart from the results list precisely so
    /// an open popover cannot move the hit that Enter would otherwise open.
    Tags,
    /// The query box's `/key` filter chooser — which attribute, then which
    /// value.
    ///
    /// **One arm for both halves**, for the reason above and one more: the two
    /// halves are one gesture, and a second `SearchMenu` would let a stale
    /// value list survive a key that had already been re-chosen. Which half is
    /// showing is [`SearchSurface::filter_context`](crate::SearchSurface)'s
    /// answer, recomputed from the caret on every keystroke.
    Filters,
}

/// Which workspaces this session has been told to leave out.
///
/// Profile selection is not kept here: it is an authorization scope owned by
/// `SearchSurface`. This state only records explicit workspace exclusions.
#[derive(Clone, Debug, Default)]
pub(super) struct FilterState {
    workspaces: HashSet<String>,
}

impl FilterState {
    pub(super) fn excluded_workspaces(&self) -> Vec<String> {
        self.workspaces.iter().cloned().collect()
    }

    pub(super) fn active(&self) -> bool {
        !self.workspaces.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.workspaces.clear();
    }

    pub(super) fn workspace_excluded(&self, workspace: &str) -> bool {
        self.workspaces.contains(workspace)
    }

    pub(super) fn toggle_workspace(&mut self, facet: &SearchFacet) {
        if !self.workspaces.remove(&facet.workspace) {
            self.workspaces.insert(facet.workspace.clone());
        }
    }

    pub(super) fn allows(&self, hit: &SearchHit) -> bool {
        !self.workspace_excluded(&hit.workspace)
    }
}

/// What identifies a result, for restoring a selection across a refresh.
///
/// **The field is part of the key**, which it was not before. One node emits
/// several fragments — its body, and one per labelled attribute — and without
/// the field they collided, so restoring a selection onto an attribute hit
/// silently landed on its body instead.
pub(super) fn hit_key(hit: &SearchHit) -> String {
    format!(
        "{}/{}/{}/{}",
        hit.workspace,
        hit.document.as_deref().unwrap_or("@workspace"),
        hit.node.as_deref().unwrap_or("@document"),
        hit.field,
    )
}

/// One row per document (or per workspace, when the hit *is* the workspace).
///
/// The index matches fragments, so two paragraphs in the same page produce two
/// hits with the same title. The surface shows only the best one — query order
/// is already best-first — matching how the entity picker collapses.
pub(super) fn keep_top_hit_per_document(hits: &mut Vec<SearchHit>) {
    let mut seen = HashSet::with_capacity(hits.len());
    hits.retain(|hit| {
        let key = if hit.workspace.starts_with("ai:chat:") {
            format!(
                "{}/response/{}",
                hit.workspace,
                hit.node.as_deref().unwrap_or("@block")
            )
        } else {
            match hit.document.as_deref() {
                Some(document) => format!("{}/doc/{document}", hit.workspace),
                None => format!("{}/ws", hit.workspace),
            }
        };
        seen.insert(key)
    });
}

/// The manifest kind of a canvas document — `koru_core::explore::KIND_CANVAS`,
/// spelled out because this crate's dependency charter keeps `koru-core` out.
const CANVAS_KIND: &str = "canvas";

/// **Drops results this build hides** (`koru_features`): while
/// `ENABLE_CANVAS` is off a canvas is not a search result, since nothing
/// would open it (guide/contributing/FEATURE-FLAGS.md, QUESTIONS #7). Run on every
/// page as it lands, so a hidden kind never takes a row, a selection or a
/// landing slot.
pub(super) fn drop_hidden_kinds(hits: &mut Vec<SearchHit>) {
    if !koru_features::canvas() {
        hits.retain(|hit| hit.document_kind != CANVAS_KIND);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QUESTIONS #7 (maintainer 2026-10-10): with `ENABLE_CANVAS` off a canvas is
    /// not a result; with it on, it is.
    #[test]
    fn a_hidden_canvas_is_not_a_result() {
        let page = a_hit();
        let mut canvas = a_hit();
        canvas.document = Some("doc-canvas".into());
        canvas.document_kind = CANVAS_KIND.into();
        let mut hits = vec![page.clone(), canvas.clone()];
        drop_hidden_kinds(&mut hits);
        assert_eq!(hits, vec![page.clone(), canvas.clone()], "canvas on");
        koru_features::testing::with(koru_features::Features::ALL_OFF, || {
            let mut hits = vec![page.clone(), canvas.clone()];
            drop_hidden_kinds(&mut hits);
            assert_eq!(hits, vec![page.clone()], "canvas off");
        });
    }

    /// **Excluding a workspace is what a filter now does**, and it is the whole
    /// of what it does.
    ///
    /// Profile selection is an authorization scope, not a negative filter.
    #[test]
    fn a_workspace_can_be_excluded_and_put_back() {
        let mut filters = FilterState::default();
        let facet = SearchFacet {
            workspace: "workspace-a".into(),
            workspace_name: "Roadmap".into(),
        };
        assert!(!filters.active());

        filters.toggle_workspace(&facet);
        assert!(filters.workspace_excluded("workspace-a"));
        assert!(filters.active());

        filters.toggle_workspace(&facet);
        assert!(!filters.workspace_excluded("workspace-a"));
        assert!(!filters.active());
    }

    /// **Two fragments of one node are two results**, and a key that could not
    /// tell them apart restored the wrong one.
    #[test]
    fn a_key_distinguishes_two_fragments_of_the_same_node() {
        let mut body = a_hit();
        body.field = "body".into();
        let mut due = a_hit();
        due.field = "attr:due".into();
        assert_ne!(hit_key(&body), hit_key(&due));
    }

    /// A workspace's own name is a result with no document and no node, and it
    /// still has to key to something stable.
    #[test]
    fn a_workspace_hit_keys_without_a_document_or_a_node() {
        let mut hit = a_hit();
        hit.document = None;
        hit.node = None;
        hit.field = "workspace".into();
        assert_eq!(hit_key(&hit), "ws-1/@workspace/@document/workspace");
    }

    /// Two body matches in one page are two fragments; the surface keeps the
    /// first (best) and drops the rest so the same title is not listed twice.
    #[test]
    fn only_the_top_hit_per_document_is_kept() {
        let mut first = a_hit();
        first.snippet = "first match".into();
        first.score = 900.0;
        let mut second = a_hit();
        second.node = Some("node-2".into());
        second.snippet = "second match".into();
        second.score = 100.0;
        let mut other = a_hit();
        other.document = Some("doc-2".into());
        other.snippet = "other document".into();
        let mut hits = vec![first.clone(), second, other.clone()];
        keep_top_hit_per_document(&mut hits);
        assert_eq!(hits, vec![first, other]);
    }

    fn a_hit() -> SearchHit {
        SearchHit {
            workspace: "ws-1".into(),
            workspace_name: "Acme".into(),
            document: Some("doc-1".into()),
            node: Some("node-1".into()),
            document_kind: "page".into(),
            mime: None,
            kind: "paragraph".into(),
            field: "body".into(),
            title: "Roadmap".into(),
            breadcrumb: "Acme / Roadmap".into(),
            breadcrumb_spans: Vec::new(),
            snippet: "the quarterly plan".into(),
            spans: Vec::new(),
            lifecycle: koru_store_wire::search::Lifecycle::Live,
            trashed_at: None,
            trashed_by: None,
            score: 1.0,
            observed_modified_at_ms: None,
            workspace_last_used_at_ms: None,
        }
    }

    /// **The file survives the process, and its round trip is the whole
    /// promise.** Written and read as text rather than through the global,
    /// because what a restarted Agent does is read a file this run wrote.
    #[test]
    fn profile_and_sort_round_trip_through_the_file() {
        let body = rendered(
            Some("2f6b4d0e-9a1e-4a5e-8d3c-1b2a3c4d5e6f"),
            SearchSort::LastUsedWorkspace,
            false,
        );
        let choices = choices_in(&body);
        assert_eq!(
            choices.profile.as_deref(),
            Some("2f6b4d0e-9a1e-4a5e-8d3c-1b2a3c4d5e6f")
        );
        assert_eq!(choices.sort, Some(SearchSort::LastUsedWorkspace));
        assert_eq!(choices.include_ai, Some(false));
        // Comments, a byte-order mark and unrelated keys are all survivable;
        // the same tolerance every other Koru `key = value` file has.
        assert_eq!(
            choices_in("\u{feff}# a note\n\nunrelated = x\nprofile =  abc \n")
                .profile
                .as_deref(),
            Some("abc")
        );
        assert_eq!(
            choices_in(&rendered(None, SearchSort::Relevance, true)).sort,
            Some(SearchSort::Relevance)
        );
        assert_eq!(choices_in("profile =\n").profile, None);
        // An older file with only a profile still loads, and leaves sort unset
        // so the surface default applies.
        assert_eq!(
            choices_in("profile = abc\n"),
            StoredChoices {
                profile: Some("abc".into()),
                sort: None,
                include_ai: None,
            }
        );
    }

    #[test]
    fn host_named_file_round_trips_all_choices_without_a_default_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nested").join("search.conf");
        assert!(!path.exists());
        store_choices_at(
            &path,
            Some("2f6b4d0e-9a1e-4a5e-8d3c-1b2a3c4d5e6f"),
            SearchSort::Relevance,
            false,
        );
        let choices = stored_choices_at(&path);
        assert_eq!(
            choices.profile.as_deref(),
            Some("2f6b4d0e-9a1e-4a5e-8d3c-1b2a3c4d5e6f")
        );
        assert_eq!(choices.sort, Some(SearchSort::Relevance));
        assert_eq!(choices.include_ai, Some(false));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    /// **Nothing is written until a host names a file.** A host's unit tests
    /// build surfaces by the dozen; a default location would mean a
    /// `cargo test` run changing which profile that person's search opens in.
    #[test]
    fn no_state_file_means_no_disk_at_all() {
        assert!(
            state_file().is_none(),
            "a test run has been given a state file to write"
        );
        assert_eq!(stored_choices(), StoredChoices::default());
        // Neither of these may panic or write anywhere.
        store_choices(Some("some-profile"), SearchSort::Relevance, true);
        remember_profile(Some("some-profile"));
        remember_sort(SearchSort::Relevance);
        remember_include_ai(false);
        SEARCH_SESSION.with(|session| *session.borrow_mut() = SearchSessionState::default());
        DISK_LOADED.with(|loaded| *loaded.borrow_mut() = false);
    }

    /// A fresh session sorts by last modified, and that is a UI choice rather
    /// than the wire's — which still answers relevance to a caller with no
    /// opinion.
    #[test]
    fn a_fresh_session_sorts_by_last_modified() {
        assert_eq!(SearchSessionState::default().sort, DEFAULT_SORT);
        assert_eq!(DEFAULT_SORT, SearchSort::ObservedModified);
        assert_eq!(SearchSort::default(), SearchSort::Relevance);
    }

    #[test]
    fn query_and_sort_are_remembered_for_the_process_session() {
        SEARCH_SESSION.with(|session| *session.borrow_mut() = SearchSessionState::default());
        DISK_LOADED.with(|loaded| *loaded.borrow_mut() = true);
        remember_query("customer notes");
        // Not the default, or this would pass without remembering anything.
        remember_sort(SearchSort::LastUsedWorkspace);
        remember_profile(Some("profile-id"));
        let selection = SearchSessionSelection {
            context: SearchSelectionContext {
                scope: "global".into(),
                profile: Some("profile-id".into()),
                query: "customer notes".into(),
                sort: SearchSort::LastUsedWorkspace,
                lifecycle: LifecycleFilter::Live,
                archived_only: false,
                include_ai: false,
                excluded_workspaces: Vec::new(),
            },
            hit_key: "workspace/document/node/body".into(),
            index: 12,
        };
        remember_selection(Some(selection.clone()));

        assert_eq!(
            search_session(),
            SearchSessionState {
                query: "customer notes".into(),
                sort: SearchSort::LastUsedWorkspace,
                profile: Some("profile-id".into()),
                include_ai: true,
                selection: Some(selection),
            }
        );

        SEARCH_SESSION.with(|session| *session.borrow_mut() = SearchSessionState::default());
        DISK_LOADED.with(|loaded| *loaded.borrow_mut() = false);
    }
}
