// SPDX-License-Identifier: MPL-2.0
//! P1.8 §5's creation-recovery gate, over fake Store and filesystem ports.
//! No test touches the disk, a provider or telemetry; the one journey with a
//! Store (an open window hearing of the Role page) uses an in-memory one.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
mod folders;
mod journeys;

fn root() -> PathBuf {
    PathBuf::from(if cfg!(windows) {
        r"D:\koru\ai"
    } else {
        "/koru/ai"
    })
}

fn selected() -> PathBuf {
    PathBuf::from(if cfg!(windows) {
        r"D:\work\research"
    } else {
        "/work/research"
    })
}

type Key = Vec<String>;
fn key(path: &Path) -> Key {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}
fn path(key: &[String]) -> PathBuf {
    key.iter().collect()
}

/// When a fake port fails relative to its effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fail {
    /// Nothing happens and the call errs.
    Before,
    /// The effect happens and the answer is lost.
    After,
}

#[derive(Default)]
struct FsState {
    dirs: BTreeSet<Key>,
    files: BTreeSet<Key>,
    /// A link and where it resolves.
    links: BTreeMap<Key, Key>,
    /// Every call, in order, by operation and path.
    calls: Vec<(&'static str, Key)>,
    fail_create: Option<(String, Fail)>,
    gate: Option<(String, SyncSender<()>, Receiver<()>)>,
}

/// An in-memory filesystem. The port has no removal at all; this records
/// every call so a test can show nothing but creation and reads happened.
#[derive(Default)]
struct FakeFs {
    state: Mutex<FsState>,
}

impl FakeFs {
    fn with(dirs: &[PathBuf]) -> Arc<Self> {
        let fs = Arc::new(Self::default());
        for dir in dirs {
            fs.mkdirs(dir);
        }
        fs
    }
    fn mkdirs(&self, dir: &Path) {
        let mut state = self.state.lock().unwrap();
        let full = key(dir);
        for end in 1..=full.len() {
            state.dirs.insert(full[..end].to_vec());
        }
    }
    fn file(&self, at: &Path) {
        self.state.lock().unwrap().files.insert(key(at));
    }
    fn link(&self, at: &Path, to: &Path) {
        self.state.lock().unwrap().links.insert(key(at), key(to));
    }
    fn exists(&self, dir: &Path) -> bool {
        self.state.lock().unwrap().dirs.contains(&key(dir))
    }
    fn creates(&self) -> Vec<PathBuf> {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(op, _)| *op == "create_dir")
            .map(|(_, at)| path(at))
            .collect()
    }
    /// Everything present, for "nothing was removed" checks.
    fn present(&self) -> (BTreeSet<Key>, BTreeSet<Key>) {
        let state = self.state.lock().unwrap();
        (state.dirs.clone(), state.files.clone())
    }
    fn fail_create(&self, suffix: &str, fail: Fail) {
        self.state.lock().unwrap().fail_create = Some((suffix.into(), fail));
    }
    fn hold_create(&self, suffix: &str) -> (Receiver<()>, SyncSender<()>) {
        let (entered_tx, entered) = sync_channel(1);
        let (release_tx, release) = sync_channel(1);
        self.state.lock().unwrap().gate = Some((suffix.into(), entered_tx, release));
        (entered, release_tx)
    }
    fn resolve(state: &FsState, at: &Path) -> Key {
        let mut out: Key = Vec::new();
        for part in key(at) {
            out.push(part);
            if let Some(target) = state.links.get(&out) {
                out = target.clone();
            }
        }
        out
    }
}

fn missing() -> std::io::Error {
    std::io::Error::from(std::io::ErrorKind::NotFound)
}

impl BotFolderSystem for FakeFs {
    fn canonical(&self, at: &Path) -> std::io::Result<PathBuf> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(("canonical", key(at)));
        let resolved = Self::resolve(&state, at);
        if state.dirs.contains(&resolved) || state.files.contains(&resolved) {
            Ok(path(&resolved))
        } else {
            Err(missing())
        }
    }
    fn occupied(&self, at: &Path) -> std::io::Result<bool> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(("occupied", key(at)));
        let full = key(at);
        let Some((last, parent)) = full.split_last() else {
            return Ok(true);
        };
        let mut resolved = Self::resolve(&state, &path(parent));
        resolved.push(last.clone());
        Ok(state.dirs.contains(&resolved)
            || state.files.contains(&resolved)
            || state.links.contains_key(&resolved))
    }
    fn create_dir(&self, at: &Path) -> std::io::Result<()> {
        let gate = {
            let mut state = self.state.lock().unwrap();
            state.calls.push(("create_dir", key(at)));
            let text = at.to_string_lossy().into_owned();
            let fail = match &state.fail_create {
                Some((suffix, fail)) if text.ends_with(suffix.as_str()) => Some(*fail),
                _ => None,
            };
            if fail == Some(Fail::Before) {
                state.fail_create = None;
                return Err(std::io::Error::other("injected"));
            }
            let full = key(at);
            let (last, parent) = full.split_last().ok_or_else(missing)?;
            let mut resolved = Self::resolve(&state, &path(parent));
            if !state.dirs.contains(&resolved) {
                return Err(missing());
            }
            resolved.push(last.clone());
            if state.dirs.contains(&resolved)
                || state.files.contains(&resolved)
                || state.links.contains_key(&resolved)
            {
                return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
            }
            state.dirs.insert(resolved);
            if fail == Some(Fail::After) {
                state.fail_create = None;
                return Err(std::io::Error::other("injected after"));
            }
            match state.gate.take() {
                Some((suffix, entered, release)) if text.ends_with(suffix.as_str()) => {
                    Some((entered, release))
                }
                other => {
                    state.gate = other;
                    None
                }
            }
        };
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            let _ = release.recv();
        }
        Ok(())
    }
    fn is_dir(&self, at: &Path) -> std::io::Result<bool> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(("is_dir", key(at)));
        let resolved = Self::resolve(&state, at);
        if state.dirs.contains(&resolved) {
            Ok(true)
        } else if state.files.contains(&resolved) {
            Ok(false)
        } else {
            Err(missing())
        }
    }
    fn is_empty_dir(&self, at: &Path) -> std::io::Result<bool> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(("is_empty_dir", key(at)));
        let resolved = Self::resolve(&state, at);
        if !state.dirs.contains(&resolved) {
            return Err(missing());
        }
        let inside = |entry: &Key| entry.len() > resolved.len() && entry.starts_with(&resolved);
        Ok(!state.dirs.iter().any(inside)
            && !state.files.iter().any(inside)
            && !state.links.keys().any(inside))
    }
}

/// The reserved folders of the fixtures' AI root, with no Store root.
fn reserved() -> ReservedFolders {
    ReservedFolders {
        ai_root: root(),
        store_root: None,
    }
}

/// One page in the fake Store, with its retained creation origin.
#[derive(Clone)]
struct StoredPage {
    workspace: WorkspaceId,
    scope: String,
    key: String,
    hash: String,
    initial: Vec<u8>,
    /// The page's current body: a human may change it.
    current: Vec<u8>,
    /// Where it stands in its workspace: live, in Trash, or removed from the
    /// manifest ("delete forever") while the Store keeps its row and origin.
    placement: BotRolePlacement,
}

#[derive(Default)]
struct StoreState {
    pages: BTreeMap<DocumentId, StoredPage>,
    /// The Store's own seven-day receipts, by (scope, key).
    receipts: BTreeMap<(String, String), DocumentId>,
    calls: usize,
    /// Pages the Store actually created (a known candidate never is again).
    created: usize,
    /// The exact bytes of every request, in order.
    seeds: Vec<(DocumentId, Vec<u8>)>,
    /// Answers that came from the retained origin after receipt expiry.
    replayed_from_origin: usize,
    fail: Option<Fail>,
    /// The next call is refused definitely, before anything is written.
    refuse: bool,
    gate: Option<(SyncSender<()>, Receiver<()>)>,
}

/// The owner's Store authority and the strict creation origin, in memory.
struct FakeOwner {
    owner: BotOwner,
    workspace: WorkspaceId,
    profile: ProfileId,
    /// The Profile the workspace was moved to, if it was.
    moved: Mutex<Option<ProfileId>>,
    allowed: AtomicBool,
    /// Existing ordinary pages of the home workspace.
    pages: Mutex<BTreeSet<DocumentId>>,
    store: Mutex<StoreState>,
    inspections: AtomicUsize,
    /// Inspections answered while access was revoked.
    denied: AtomicUsize,
}

impl FakeOwner {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            owner: BotOwner {
                user_scope: UserScopeId::new(),
                device: DeviceId::from_uuid(Uuid::new_v4()),
            },
            workspace: WorkspaceId::new(),
            profile: ProfileId::new(),
            moved: Mutex::new(None),
            allowed: AtomicBool::new(true),
            pages: Mutex::new(BTreeSet::new()),
            store: Mutex::new(StoreState::default()),
            inspections: AtomicUsize::new(0),
            denied: AtomicUsize::new(0),
        })
    }
    fn existing_page(&self) -> BotPage {
        let document = DocumentId::new();
        self.pages.lock().unwrap().insert(document);
        BotPage {
            workspace: self.workspace,
            document,
        }
    }
    fn fail_store(&self, fail: Fail) {
        self.store.lock().unwrap().fail = Some(fail);
    }
    fn hold_store(&self) -> (Receiver<()>, SyncSender<()>) {
        let (entered_tx, entered) = sync_channel(1);
        let (release_tx, release) = sync_channel(1);
        self.store.lock().unwrap().gate = Some((entered_tx, release));
        (entered, release_tx)
    }
    /// The seven-day Store receipt expires; the origin row does not.
    fn expire_receipts(&self) {
        self.store.lock().unwrap().receipts.clear();
    }
    fn stored(&self) -> BTreeMap<DocumentId, StoredPage> {
        self.store.lock().unwrap().pages.clone()
    }
    fn store_calls(&self) -> usize {
        self.store.lock().unwrap().calls
    }
    fn created(&self) -> usize {
        self.store.lock().unwrap().created
    }
    /// What the person did to the page: Trash, restore, or delete forever.
    fn place(&self, page: DocumentId, placement: BotRolePlacement) {
        self.store
            .lock()
            .unwrap()
            .pages
            .get_mut(&page)
            .expect("a stored page")
            .placement = placement;
    }
}

impl BotAuthority for FakeOwner {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        if self.allowed.load(Ordering::SeqCst) && id == self.workspace {
            Ok(self.moved.lock().unwrap().unwrap_or(self.profile))
        } else {
            Err(BotError::Forbidden)
        }
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.workspace(page.workspace)?;
        let live = self
            .store
            .lock()
            .unwrap()
            .pages
            .get(&page.document)
            .is_some_and(|p| {
                p.placement == BotRolePlacement::Live && p.workspace == page.workspace
            });
        if live || self.pages.lock().unwrap().contains(&page.document) {
            Ok(())
        } else {
            Err(BotError::NotFound)
        }
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        self.inspections.fetch_add(1, Ordering::SeqCst);
        if !self.allowed.load(Ordering::SeqCst) {
            self.denied.fetch_add(1, Ordering::SeqCst);
        }
        Ok(BotAuthoritySnapshot {
            owner: self.owner()?,
            workspaces: workspaces
                .iter()
                .map(|id| (*id, self.workspace(*id)))
                .collect(),
            pages: pages
                .iter()
                .map(|p| (p.clone(), BotAuthority::page(self, p)))
                .collect(),
        })
    }
    /// The adapter's contract over the Store's strict origin: a candidate
    /// with a recorded origin is never created again, and answers with that
    /// origin and where the page stands, whatever the seven-day receipt
    /// says; a candidate with none is created once.
    fn create_role_page(&self, seed: &BotRoleSeed) -> Result<BotRoleProof, BotError> {
        let gate = self.store.lock().unwrap().gate.take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            let _ = release.recv();
        }
        if seed.owner != self.owner || self.workspace(seed.workspace) != Ok(seed.profile) {
            return Err(BotError::Forbidden);
        }
        let mut store = self.store.lock().unwrap();
        store.calls += 1;
        store.seeds.push((seed.candidate, seed.seed.clone()));
        let fail = store.fail.take();
        if fail == Some(Fail::Before) {
            return Err(BotError::Unavailable);
        }
        if std::mem::take(&mut store.refuse) {
            return Err(BotError::Invalid);
        }
        let receipt = (BOT_ROLE_SCOPE.to_owned(), seed.request_key.clone());
        match store.pages.get(&seed.candidate).cloned() {
            Some(page) => {
                if page.scope != BOT_ROLE_SCOPE
                    || page.key != seed.request_key
                    || page.hash != seed.request_hash
                {
                    return Err(BotError::Conflict);
                }
                if !store.receipts.contains_key(&receipt) {
                    store.replayed_from_origin += 1;
                }
            }
            None => {
                if let Some(other) = store.receipts.get(&receipt)
                    && *other != seed.candidate
                {
                    return Err(BotError::Conflict);
                }
                store.pages.insert(
                    seed.candidate,
                    StoredPage {
                        workspace: seed.workspace,
                        scope: BOT_ROLE_SCOPE.into(),
                        key: seed.request_key.clone(),
                        hash: seed.request_hash.clone(),
                        initial: seed.seed.clone(),
                        current: seed.seed.clone(),
                        placement: BotRolePlacement::Live,
                    },
                );
                store.created += 1;
                store.receipts.insert(receipt, seed.candidate);
            }
        }
        if fail == Some(Fail::After) {
            return Err(BotError::Unavailable);
        }
        let page = &store.pages[&seed.candidate];
        Ok(BotRoleProof {
            scope: page.scope.clone(),
            request_key: page.key.clone(),
            request_hash: page.hash.clone(),
            initial_update: page.initial.clone(),
            placement: page.placement,
        })
    }
}

fn owner_context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "caller-controlled".into(),
            scopes: vec![AiScope::Chat, AiScope::ManageBots],
        },
        trace_id: "fixture".into(),
        deadline_millis: None,
    }
}

fn config() -> BotCreationConfig {
    BotCreationConfig {
        name: "Researcher".into(),
        model: koru_ai_core::ModelSelection {
            provider: AiProviderId::parse("agent-codex").unwrap(),
            requested_model: "fixture".into(),
            default_reasoning: ReasoningChoice::Default,
        },
        mode: None,
        fast: None,
        avatar: None,
    }
}

fn create(owner: &FakeOwner, role: BotRoleInput, folder: BotFolderChoice) -> BotCommand {
    BotCommand::CreateBotWithRole {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        home_workspace: owner.workspace,
        role,
        folder,
        config: config(),
        access: BotAccess::Default,
        selected_pages: vec![],
    }
}

fn authored() -> BotRoleInput {
    BotRoleInput::Authored {
        title: "Research role".into(),
        markdown: "Find primary sources and cite them.".into(),
    }
}

fn request_of(command: &BotCommand) -> (BotRequestId, BotId) {
    match command {
        BotCommand::CreateBotWithRole {
            request_id, bot_id, ..
        } => (*request_id, *bot_id),
        _ => panic!("a creation"),
    }
}
