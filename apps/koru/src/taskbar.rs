// SPDX-License-Identifier: MPL-2.0

//! Windows taskbar integration.
//!
//! A Jump List entry is a shell link back to the executable with an argument;
//! Windows starts another process when it is chosen. Koru must not let that
//! second process open the same default peer, so the first process also owns a
//! tiny named-pipe activation channel. A taskbar launch writes one short line
//! and exits; the running container notices the request on its normal event
//! loop wake-up and acts on it there.
//!
//! **The pipe is scoped to this build** (`Koru.Taskbar.NewDocument.<segment>`,
//! plus the branch for internal builds), because stable, nightly and several
//! dev branches run side by side on one machine and a click must only reach a
//! process of the same build.
//!
//! **The listener never brings the window forward itself.** It only sets a flag
//! and wakes the loop; the shell consumes the flag and focuses its own window —
//! the foreground ask has to come from the thread that owns the window, which
//! is the exact path the tray's *Restore* already takes.
//!
//! ## Two categories
//!
//! **Tasks** is fixed — *Search*, *AI Chat* and *New document*, in that order.
//! *Search* carries the chord the agent actually registered as part of its
//! title, so the taskbar and the tray can never disagree about which
//! combination works. *AI Chat* carries its fixed `Ctrl+Shift+I` chord.
//! **Recent** is the last ten documents you opened, rebuilt whenever that list
//! changes, so the taskbar offers the same thing the welcome tab does.
//!
//! A Recent entry carries **`--profile` as well as the document id**. When
//! Koru is already running the pipe takes the request and the path is never
//! used; the path is for the cold launch, and it is what makes one work. Without
//! it the entry falls back to the *last-selected* profile, which is the last
//! profile opened rather than the one the document is in — so picking a recent
//! document after working in another profile silently did nothing at all. Found by
//! trying it.
//!
//! Baking a path in would normally be a way to freeze it, but not here: the
//! list is rewritten every time the recents change, which is every time a
//! document is opened, so a moved folder corrects itself on the next open.

#[cfg(windows)]
mod windows_impl {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use windows::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, ERROR_UNABLE_TO_REMOVE_REPLACED, HANDLE, PROPERTYKEY,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_GENERIC_WRITE,
        FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile, WIN32_FIND_DATAW, WriteFile,
    };
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize, IPersistFile, STGM_READWRITE,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
        PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
    };
    use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{
        DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW,
        SHCNE_UPDATEITEM, SHCNF_PATHW, SHChangeNotify, SLGP_RAWPATH, ShellLink,
    };
    use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
    use windows::core::{Error, GUID, HRESULT, HSTRING, Interface, PCWSTR, Result, w};

    static NEW_DOCUMENT: AtomicBool = AtomicBool::new(false);
    /// A taskbar *Search* entry was chosen, waiting for the loop to ask the
    /// agent to open its overlay.
    static SEARCH: AtomicBool = AtomicBool::new(false);
    /// A taskbar *AI Chat* entry was chosen, waiting for the loop to ask the
    /// agent to open or focus the standalone chat window.
    static AI_CHAT: AtomicBool = AtomicBool::new(false);
    /// A taskbar activation asked this window to come forward. Consumed by the
    /// shell, which focuses its own window — the listener only signals.
    static FOREGROUND: AtomicBool = AtomicBool::new(false);
    /// Whether this process won the pipe, and is therefore the one Koru of this
    /// build that owns it.
    ///
    /// **The pipe is per build now, so this is per build, not per machine.**
    /// Binding the activation pipe with `FILE_FLAG_FIRST_PIPE_INSTANCE` succeeds
    /// for exactly one process per name — which is what scopes the election to
    /// this segment (and, for internal builds, this branch).
    static ACTIVATION_OWNER: AtomicBool = AtomicBool::new(false);
    /// A document a taskbar entry asked for, waiting for a container frame.
    static OPEN: Mutex<Option<String>> = Mutex::new(None);
    /// A stable document a taskbar entry asked for, waiting for the store to
    /// resolve which workspace it lives in.
    static OPEN_DOCUMENT: Mutex<Option<koru_core::DocumentId>> = Mutex::new(None);
    static UNPINNED: Mutex<Vec<koru_core::DocumentId>> = Mutex::new(Vec::new());
    /// The chord the *Search* task should advertise, once the agent has said
    /// which one it actually registered. `None` both before the answer and when
    /// there is no chord, so the task starts plain and gains a hint in place.
    static SHORTCUT: Mutex<Option<String>> = Mutex::new(None);
    /// What was last handed to Explorer, so an unchanged list costs nothing.
    /// The shortcut is part of the key: a chord arriving is itself a change the
    /// list has to be rebuilt for.
    static INSTALLED: Mutex<Option<JumpList>> = Mutex::new(None);
    /// How many lists have been asked for; each rebuild carries its number.
    static ASKED: AtomicU64 = AtomicU64::new(0);
    /// The number of the list Explorer holds now. Held across a registration,
    /// which is what makes them one at a time. See [`install`].
    static REGISTERED: Mutex<u64> = Mutex::new(0);
    /// A proxy into the winit loop, so the listener thread can *wake* it after
    /// setting a flag — `SetForegroundWindow`'s focus side effect is not a
    /// wake, and a loop asleep would leave the flag sitting until the next
    /// keystroke. Installed by the shell once its loop exists.
    static WAKE: Mutex<Option<winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>>> =
        Mutex::new(None);

    /// The two things a Jump List is built from. Kept together so the dedup
    /// compares the whole shape, not either of its parts.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct JumpList {
        recents: Vec<(String, koru_core::DocumentId)>,
        pinned: Vec<(String, koru_core::DocumentId)>,
        shortcut: Option<String>,
    }

    /// The longest activation line the listener will read. A verb and a uuid fit
    /// in a fraction of it; the cap is there so a stuck writer cannot make the
    /// listener wait on an unbounded read.
    const ACTIVATION_MAX: usize = 128;

    /// The document mark's index among the exe's icons — resource `2` in
    /// `koru.rc`, which is index 1. Resource 1 stays the application icon.
    const DOCUMENT_ICON: i32 = 1;

    /// The activation pipe's name, scoped to this build.
    ///
    /// **One pipe per build, not per machine.** Every channel (stable, beta,
    /// nightly) and every internal build from a different branch is a different
    /// binary tested side by side, and a click must be handed only to a process
    /// of the same build. Released channels are already unique by segment;
    /// internal builds add the stamped branch, because several branches are
    /// tested on one machine at once. A non-`[A-Za-z0-9._-]` character in the
    /// branch is flattened to `-`, because branch names like `feature/foo` are
    /// not all pipe-name-safe.
    fn pipe_name() -> Vec<u16> {
        let mut name = format!(
            r"\\.\pipe\Koru.Taskbar.NewDocument.{}",
            koru_channel::segment()
        );
        if let Some(branch) = crate::build_stamp::branch() {
            name.push('.');
            for ch in branch.chars() {
                name.push(if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '-'
                });
            }
        }
        name.encode_utf16().chain(Some(0)).collect()
    }

    // System.Title (PKEY_Title). A shell link uses this property as the visible
    // Jump List task label instead of falling back to the executable name.
    const PKEY_TITLE: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9),
        pid: 2,
    };

    pub(super) fn prepare(request: Option<&str>) -> bool {
        if let Some(line) = request {
            tracing::info!(
                target: "koru::taskbar",
                line,
                "taskbar activation requested"
            );
        }
        if let Some(line) = request
            && signal_existing(line, 0)
        {
            return true;
        }

        match first_pipe() {
            Ok(pipe) => {
                // **Winning the pipe is winning the election**, and anything
                // machine-wide reads it from here. See `ACTIVATION_OWNER`.
                ACTIVATION_OWNER.store(true, Ordering::Release);
                tracing::info!(
                    target: "koru::taskbar",
                    "won the activation pipe; this process is the activation owner"
                );
                // windows-rs keeps HANDLE conservative (`!Send`) because an
                // arbitrary handle may have thread affinity. A named pipe has
                // none; pass its integer value and reconstruct it in the
                // listener so the closure itself contains only a Send value.
                let raw_pipe = pipe.0 as usize;
                std::thread::Builder::new()
                    .name("koru-taskbar-activation".into())
                    .spawn(move || serve(HANDLE(raw_pipe as *mut std::ffi::c_void)))
                    .expect("start Koru taskbar activation listener");
                false
            }
            // Another process won the startup race after the first connect
            // attempt. Give its listener a moment to reach ConnectNamedPipe,
            // then hand off instead of opening a second default peer.
            Err(_) => match request {
                Some(line) => {
                    tracing::info!(
                        target: "koru::taskbar",
                        "another process holds the pipe; retrying the hand-off"
                    );
                    signal_existing(line, 1_000)
                }
                None => false,
            },
        }
    }

    pub(super) fn pending() -> bool {
        NEW_DOCUMENT.load(Ordering::Acquire)
    }

    pub(super) fn take() -> bool {
        NEW_DOCUMENT.swap(false, Ordering::AcqRel)
    }

    pub(super) fn open_pending() -> bool {
        OPEN.lock().is_ok_and(|o| o.is_some())
    }

    pub(super) fn take_open() -> Option<String> {
        OPEN.lock().ok()?.take()
    }

    pub(super) fn open_document_pending() -> bool {
        OPEN_DOCUMENT.lock().is_ok_and(|o| o.is_some())
    }

    pub(super) fn take_open_document() -> Option<koru_core::DocumentId> {
        OPEN_DOCUMENT.lock().ok()?.take()
    }

    pub(super) fn take_search() -> bool {
        SEARCH.swap(false, Ordering::AcqRel)
    }

    pub(super) fn take_ai_chat() -> bool {
        AI_CHAT.swap(false, Ordering::AcqRel)
    }

    pub(super) fn take_foreground() -> bool {
        FOREGROUND.swap(false, Ordering::AcqRel)
    }

    /// Hand Explorer a new Recent category, if it would differ from the last
    /// one. Rebuilt on a thread of its own: this is COM, and opening a document
    /// must not wait for the shell.
    pub(super) fn set_recents(entries: Vec<(String, koru_core::DocumentId)>) {
        let shortcut = SHORTCUT.lock().ok().and_then(|guard| guard.clone());
        let pinned = match INSTALLED.lock() {
            Ok(installed) => installed
                .as_ref()
                .map(|j| j.pinned.clone())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        install(entries, pinned, shortcut);
    }

    pub(super) fn set_pinned(entries: Vec<(String, koru_core::DocumentId)>) {
        let shortcut = SHORTCUT.lock().ok().and_then(|guard| guard.clone());
        let recents = match INSTALLED.lock() {
            Ok(installed) => installed
                .as_ref()
                .map(|j| j.recents.clone())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        install(recents, entries, shortcut);
    }

    /// Say which chord the *Search* task should advertise, and rebuild the list
    /// if that changes it. `None` is a valid answer: it means there is no chord
    /// (or none yet), and the task is drawn plain.
    pub(super) fn set_shortcut(shortcut: Option<String>) {
        *SHORTCUT.lock().expect("taskbar shortcut") = shortcut.clone();
        let (recents, pinned) = match INSTALLED.lock() {
            Ok(installed) => installed
                .as_ref()
                .map(|j| (j.recents.clone(), j.pinned.clone()))
                .unwrap_or_default(),
            Err(_) => return,
        };
        install(recents, pinned, shortcut);
    }

    /// Give the listener thread a way to wake the loop after it sets a flag.
    /// Installed once the shell's event loop exists; until then the flag is
    /// simply drained on the loop's first pass.
    pub(super) fn install_wake_proxy(
        proxy: winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>,
    ) {
        *WAKE.lock().expect("taskbar wake proxy") = Some(proxy);
    }

    /// Rebuild the Jump List on its own thread, if the shape changed. The one
    /// path the initial list, `set_recents`, `set_pinned` and `set_shortcut`
    /// all meet in.
    fn install(
        recents: Vec<(String, koru_core::DocumentId)>,
        pinned: Vec<(String, koru_core::DocumentId)>,
        shortcut: Option<String>,
    ) {
        let generation = {
            let mut installed = match INSTALLED.lock() {
                Ok(i) => i,
                Err(_) => return,
            };
            let now = JumpList {
                recents: recents.clone(),
                pinned: pinned.clone(),
                shortcut: shortcut.clone(),
            };
            if installed.as_ref() == Some(&now) {
                return;
            }
            *installed = Some(now);
            // Numbered under the same lock that decided the shape, so the
            // numbers are in the order the shapes were decided.
            ASKED.fetch_add(1, Ordering::Relaxed) + 1
        };
        tracing::info!(
            target: "koru::taskbar",
            recents = recents.len(),
            pinned = pinned.len(),
            shortcut,
            "rebuilding the Jump List"
        );
        let _ = std::thread::Builder::new()
            .name("koru-jump-list".into())
            .spawn(move || {
                // **One registration at a time, and never an older list over a
                // newer one.** Each rebuild is its own thread, and two are
                // routinely in flight together — the startup list and the
                // shortcut's, or `set_recents` and `set_pinned`, which are
                // called back to back. Unordered, whichever thread finished last
                // won, and the list Explorer kept could be the stale one, with
                // the dedup above then refusing to correct it.
                let Ok(mut registered) = REGISTERED.lock() else {
                    return;
                };
                if *registered > generation {
                    return;
                }
                match unsafe { register_jump_list(&recents, &pinned, shortcut.as_deref()) } {
                    Ok(()) => tracing::info!(target: "koru::taskbar", "Jump List registered"),
                    Err(error) => tracing::error!(
                        target: "koru::taskbar",
                        "could not update the taskbar list: {error}"
                    ),
                }
                *registered = generation;
            });
    }

    /// The tasks, before anything else is known. **On its own thread like
    /// every other rebuild**: this is about 90 ms of COM and shell links, and it
    /// used to be paid on the main thread before the window or the GPU had
    /// started, a quarter of a cold launch. Nothing on the way to the first
    /// frame reads the list.
    pub(super) fn install_jump_list() {
        install(Vec::new(), Vec::new(), None);
    }

    fn first_pipe() -> Result<HANDLE> {
        let name = pipe_name();
        unsafe {
            let handle = CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                0,
                1,
                0,
                None,
            );
            if handle.is_invalid() {
                Err(Error::from_thread())
            } else {
                Ok(handle)
            }
        }
    }

    fn signal_existing(line: &str, wait_ms: u32) -> bool {
        let name = pipe_name();
        unsafe {
            if !WaitNamedPipeW(PCWSTR(name.as_ptr()), wait_ms).as_bool() {
                tracing::info!(
                    target: "koru::taskbar",
                    wait_ms,
                    "no listener waiting on the activation pipe"
                );
                return false;
            }
            let Ok(pipe) = CreateFileW(
                PCWSTR(name.as_ptr()),
                FILE_GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            ) else {
                tracing::info!(
                    target: "koru::taskbar",
                    "the activation pipe would not open for writing"
                );
                return false;
            };
            // A process started by the taskbar is allowed to foreground a
            // window. Delegate that permission to the process receiving the
            // activation so its prompt is visible instead of appearing in a
            // minimized or background window.
            let mut server = 0;
            let allowed = GetNamedPipeServerProcessId(pipe, &mut server)
                .is_ok_and(|()| AllowSetForegroundWindow(server).is_ok());
            tracing::info!(
                target: "koru::taskbar",
                server,
                allowed,
                "delegated foreground permission to the running process"
            );
            let bytes = line.as_bytes();
            let mut written = 0;
            let sent = bytes.len() <= ACTIVATION_MAX
                && WriteFile(pipe, Some(bytes), Some(&mut written), None).is_ok()
                && written as usize == bytes.len();
            let _ = CloseHandle(pipe);
            tracing::info!(
                target: "koru::taskbar",
                line,
                sent,
                "hand-off to the running process"
            );
            sent
        }
    }

    fn serve(pipe: HANDLE) {
        loop {
            let connected = unsafe {
                match ConnectNamedPipe(pipe, None) {
                    Ok(()) => true,
                    Err(error) => error.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0),
                }
            };
            if !connected {
                break;
            }

            let mut buffer = [0_u8; ACTIVATION_MAX];
            let mut read = 0;
            let received = unsafe {
                ReadFile(pipe, Some(&mut buffer), Some(&mut read), None).is_ok() && read > 0
            };
            // A line that is not understood is **dropped, not guessed at**. It
            // can only come from a differently-versioned Koru, and acting on a
            // half-recognised request is how the wrong document opens.
            if received && let Ok(line) = std::str::from_utf8(&buffer[..read as usize]) {
                tracing::info!(target: "koru::taskbar", line, "activation received");
                let mut acted = false;
                // **`search` and `ai-chat` never foreground Koru.** Their
                // windows are agent-owned and come forward on their own; Koru's
                // window stays wherever it was. `new` and `open` bring Koru forward.
                let mut foreground_owed = false;
                if line == "new" {
                    NEW_DOCUMENT.store(true, Ordering::Release);
                    acted = true;
                    foreground_owed = true;
                } else if line == "search" {
                    SEARCH.store(true, Ordering::Release);
                    acted = true;
                } else if line == "ai-chat" {
                    AI_CHAT.store(true, Ordering::Release);
                    acted = true;
                } else if let Some(id) = line.strip_prefix("open-document ") {
                    *OPEN_DOCUMENT.lock().expect("taskbar open-document request") =
                        koru_core::DocumentId::parse(id);
                    acted = true;
                    foreground_owed = true;
                } else if let Some(id) = line.strip_prefix("open ") {
                    *OPEN.lock().expect("taskbar open request") = Some(id.to_string());
                    acted = true;
                    foreground_owed = true;
                }
                if acted {
                    // **Signal, don't foreground.** The window's own thread is
                    // the only one that may bring it forward, so this just
                    // records that one is owed and wakes the loop; the shell
                    // consumes it and focuses its own window (the tray's
                    // `Restore` path). `AllowSetForegroundWindow` above already
                    // granted the permission that focus needs.
                    if foreground_owed {
                        FOREGROUND.store(true, Ordering::Release);
                    }
                    // **Wake, don't hope.** The flag above is only drained by
                    // the loop's `about_to_wait`, and a loop asleep sees nothing
                    // to run it.
                    let woken =
                        if let Some(proxy) = WAKE.lock().expect("taskbar wake proxy").as_ref() {
                            proxy
                                .send_event(koru_workspace_host::app::Wake::Taskbar)
                                .is_ok()
                        } else {
                            // The loop does not exist yet; the flag is drained on
                            // its first pass instead.
                            false
                        };
                    tracing::info!(
                        target: "koru::taskbar",
                        woken,
                        "activation acted on and the loop woken"
                    );
                }
            }
            let _ = unsafe { DisconnectNamedPipe(pipe) };
        }
        let _ = unsafe { CloseHandle(pipe) };
    }

    unsafe fn removed_open_documents(removed: &IObjectArray) -> Vec<koru_core::DocumentId> {
        let Ok(count) = (unsafe { removed.GetCount() }) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..count {
            let Ok(unknown) = (unsafe { removed.GetAt::<windows::core::IUnknown>(i) }) else {
                continue;
            };
            let Ok(link) = unknown.cast::<IShellLinkW>() else {
                continue;
            };
            let mut buf = [0u16; 1024];
            if unsafe { link.GetArguments(&mut buf) }.is_err() {
                continue;
            }
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let args = String::from_utf16_lossy(&buf[..end]);
            let Some(id) = args
                .split_whitespace()
                .skip_while(|part| *part != "--open-document")
                .nth(1)
                .and_then(koru_core::DocumentId::parse)
            else {
                continue;
            };
            out.push(id);
        }
        out
    }

    pub(super) fn take_unpinned() -> Vec<koru_core::DocumentId> {
        UNPINNED
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }

    /// One Recent entry: a link back to the exe with `--open-document <id>`,
    /// labelled with the document's name. The store resolves which workspace the
    /// document lives in, so a recent (or a pinned copy of one) opens in the
    /// right place however it is reached — cold or warm, from any workspace.
    unsafe fn recent_link(
        executable: &HSTRING,
        name: &str,
        id: &koru_core::DocumentId,
    ) -> Result<IShellLinkW> {
        let link: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
        let arguments = HSTRING::from(format!("--open-document {}", id.to_storage()));
        let description = HSTRING::from(format!("Open {name} in Koru"));
        unsafe {
            link.SetPath(executable)?;
            link.SetArguments(&arguments)?;
            link.SetDescription(&description)?;
            // **Resource 1, not 0** — the second icon in the exe, which is the
            // Phosphor document mark rather than the app logo. A row of ten
            // identical app icons says nothing about what any of them is; a
            // document icon says these are documents. Windows takes a file and
            // an index here, never a glyph, which is why the mark is rasterised
            // into `assets/document.ico` and embedded rather than drawn.
            link.SetIconLocation(executable, DOCUMENT_ICON)?;
        }
        // Without System.Title the shell falls back to the executable's name,
        // so every row would read "koru".
        let properties: IPropertyStore = link.cast()?;
        let title = PROPVARIANT::from(name);
        unsafe {
            properties.SetValue(&PKEY_TITLE, &title)?;
            properties.Commit()?;
        }
        Ok(link)
    }

    unsafe fn register_jump_list(
        recents: &[(String, koru_core::DocumentId)],
        pinned: &[(String, koru_core::DocumentId)],
        shortcut: Option<&str>,
    ) -> Result<()> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        let _com = Com;

        let executable_path = std::env::current_exe()?;
        let executable = HSTRING::from(executable_path.to_string_lossy().as_ref());

        let destinations: ICustomDestinationList =
            unsafe { CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)? };
        let mut slots = 0;
        let removed: IObjectArray = unsafe { destinations.BeginList(&mut slots)? };
        let unpinned = unsafe { removed_open_documents(&removed) };
        if !unpinned.is_empty() {
            if let Ok(mut queue) = UNPINNED.lock() {
                queue.extend(unpinned);
            }
            if let Some(proxy) = WAKE.lock().ok().and_then(|g| g.clone()) {
                let _ = proxy.send_event(koru_workspace_host::app::Wake::Taskbar);
            }
        }

        // **Search first**, above *New document*: it is the primary task, and
        // its title carries the chord the agent actually registered — see
        // [`search_task_title`]. The chord is the one thing here that changes
        // after the list is first installed, which is why `shortcut` is part of
        // the dedup key.
        let search: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
        unsafe {
            search.SetPath(&executable)?;
            search.SetArguments(w!("--search"))?;
            search.SetDescription(w!("Open Koru Search"))?;
            search.SetIconLocation(&executable, 0)?;
        }
        let search_properties: IPropertyStore = search.cast()?;
        let search_title = super::search_task_title(shortcut);
        unsafe {
            search_properties.SetValue(&PKEY_TITLE, &PROPVARIANT::from(search_title.as_str()))?;
            search_properties.Commit()?;
        }

        let ai_chat: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
        unsafe {
            ai_chat.SetPath(&executable)?;
            ai_chat.SetArguments(w!("--ai-chat"))?;
            ai_chat.SetDescription(w!("Open Koru AI Chat"))?;
            ai_chat.SetIconLocation(&executable, 0)?;
        }
        let ai_chat_properties: IPropertyStore = ai_chat.cast()?;
        unsafe {
            ai_chat_properties
                .SetValue(&PKEY_TITLE, &PROPVARIANT::from("AI Chat (Ctrl+Shift+I)"))?;
            ai_chat_properties.Commit()?;
        }

        let link: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
        unsafe {
            link.SetPath(&executable)?;
            link.SetArguments(w!("--new-document"))?;
            link.SetDescription(w!("Create a new Koru document"))?;
            link.SetIconLocation(&executable, 0)?;
        }

        let properties: IPropertyStore = link.cast()?;
        let title = PROPVARIANT::from("New document");
        unsafe {
            properties.SetValue(&PKEY_TITLE, &title)?;
            properties.Commit()?;
        }

        let collection: IObjectCollection =
            unsafe { CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)? };
        unsafe {
            collection.AddObject(&search)?;
            // No *AI Chat* task in a build that hides Koru AI (koru-features).
            if koru_features::ai() {
                collection.AddObject(&ai_chat)?;
            }
            collection.AddObject(&link)?;
        }
        let tasks: IObjectArray = collection.cast()?;

        if !pinned.is_empty() {
            let pinned_items: IObjectCollection = unsafe {
                CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?
            };
            for (name, id) in pinned.iter() {
                let entry = unsafe { recent_link(&executable, name, id)? };
                unsafe { pinned_items.AddObject(&entry)? };
            }
            let items: IObjectArray = pinned_items.cast()?;
            if let Err(error) = unsafe { destinations.AppendCategory(w!("App Pinned"), &items) } {
                tracing::warn!(
                    target: "koru::taskbar",
                    "the taskbar declined the pinned list: {error}"
                );
            }
        }

        // **Recent goes on before Tasks**, because `AppendCategory` places a
        // category above the fixed tasks — which is the order every other app
        // uses, and the order the taskbar's own "Recent" heading implies.
        //
        // A category with nothing in it is an error from the shell rather than
        // an empty heading, so an empty list simply does not add one.
        if !recents.is_empty() {
            let recent: IObjectCollection = unsafe {
                CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?
            };
            for (name, id) in recents.iter().take(super::RECENT_MAX) {
                let entry = unsafe { recent_link(&executable, name, id)? };
                unsafe { recent.AddObject(&entry)? };
            }
            let items: IObjectArray = recent.cast()?;
            // Not fatal: the shell refuses a category the user has turned off
            // ("Show recently opened items" in taskbar settings), and losing
            // the whole list — including *New document* — over that would be
            // worse than losing the half they asked not to see.
            if let Err(error) = unsafe { destinations.AppendCategory(w!("Recent"), &items) } {
                tracing::warn!(
                    target: "koru::taskbar",
                    "the taskbar declined the recent list: {error}"
                );
            }
        }

        unsafe {
            destinations.AddUserTasks(&tasks)?;
            destinations.CommitList()?;
        }
        refresh_pinned_shortcuts(&executable_path, &executable);
        Ok(())
    }

    /// Explorer keeps the icon captured when an app was pinned, even after the
    /// executable's icon resource changes. Give only shortcuts that target this
    /// exact executable an explicit current icon location, then notify the shell
    /// that their cached image is stale.
    fn refresh_pinned_shortcuts(executable_path: &Path, executable: &HSTRING) {
        let Some(appdata) = std::env::var_os("APPDATA") else {
            return;
        };
        let directory = PathBuf::from(appdata)
            .join("Microsoft")
            .join("Internet Explorer")
            .join("Quick Launch")
            .join("User Pinned")
            .join("TaskBar");
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };

        notify_icon_changed(executable_path);
        for entry in entries.flatten() {
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
            {
                continue;
            }
            let _ = refresh_pinned_shortcut(&path, executable_path, executable);
        }
    }

    fn refresh_pinned_shortcut(
        path: &Path,
        executable_path: &Path,
        executable: &HSTRING,
    ) -> Result<()> {
        let link: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
        let persist: IPersistFile = link.cast()?;
        let shortcut = wide(path);
        unsafe { persist.Load(PCWSTR(shortcut.as_ptr()), STGM_READWRITE)? };

        let mut target = vec![0_u16; 32_768];
        let mut metadata = WIN32_FIND_DATAW::default();
        unsafe { link.GetPath(&mut target, &mut metadata, SLGP_RAWPATH.0 as u32)? };
        let end = target
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(target.len());
        let target = PathBuf::from(OsString::from_wide(&target[..end]));
        if !same_path(&target, executable_path) {
            return Ok(());
        }

        unsafe {
            link.SetIconLocation(executable, 0)?;
            if let Err(error) = persist.Save(PCWSTR(shortcut.as_ptr()), true) {
                // The pinned shortcut is locked by Explorer while the app is
                // pinned and running; `ERROR_UNABLE_TO_REMOVE_REPLACED` is that
                // transient lock, not a failure worth surfacing on every launch.
                if error.code() == HRESULT::from_win32(ERROR_UNABLE_TO_REMOVE_REPLACED.0) {
                    return Ok(());
                }
                return Err(error);
            }
        }
        notify_icon_changed(path);
        Ok(())
    }

    fn same_path(left: &Path, right: &Path) -> bool {
        let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_owned());
        let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_owned());
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn notify_icon_changed(path: &Path) {
        let path = wide(path);
        unsafe {
            SHChangeNotify(
                SHCNE_UPDATEITEM,
                SHCNF_PATHW,
                Some(path.as_ptr().cast()),
                None,
            );
        }
    }
}

/// ## macOS has no Dock menu here, and it is blocked rather than skipped
///
/// The counterpart would be recent documents under a right-click on the Dock
/// icon. **AppKit only offers that through `applicationDockMenu:` on the
/// `NSApplicationDelegate`** — there is no `NSApp.setDockMenu`, and `muda`
/// (which builds the menu bar) exposes no Dock API for the same reason.
///
/// **winit owns the application delegate** and aborts if anything replaces it
/// (`koru-workspace-host`'s `macos_urls`, which learned that the hard way). The
/// shell adds `application:openURLs:` to winit's delegate class rather than
/// installing one of its own, and `applicationDockMenu:` would be added the
/// same way; the callback and its menu model are simply not implemented yet.
///
/// **The activation channel below should not be ported regardless.** Most of
/// `windows_impl` exists because a Jump List entry *starts a second process*
/// which must hand its request to the running one and exit. Choosing a Dock row
/// calls back into the app that is already running, so there is nothing to
/// marshal.

/// enough to scroll is a file browser, which is a different feature.
pub const RECENT_MAX: usize = 10;

/// The title the *Search* task carries.
///
/// A Jump List task has one text field, so the chord is folded into the title
/// rather than drawn as a right-aligned accelerator the way the tray does it.
/// `None` — before the agent answers, when it is absent, or when there is no
/// agent — is plain `Search`, which is honest: the task still works, it just
/// does not advertise a chord.
// Only the cfg(windows) task builder calls it outside tests; the tests pin
// the string on every platform.
#[cfg_attr(not(windows), allow(dead_code))]
fn search_task_title(shortcut: Option<&str>) -> String {
    match shortcut {
        Some(label) => format!("Search ({label})"),
        None => "Search".to_string(),
    }
}

/// What a taskbar launch is asking for, as the one line that crosses the pipe.
///
/// A string rather than an enum because it *is* the wire format — keeping the
/// two the same thing is what stops a new verb being added to one and not the
/// other.
// Only `line()` reads the payload, and only Windows has the jump list that
// sends one.
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Activation {
    NewDocument,
    /// Open or focus the machine-wide standalone AI chat window.
    AiChat,
    /// A stable document id, resolved through the store (the Jump List's Recent
    /// and pinned entries). Not [`Open`], which is the legacy node-id spelling
    /// older pinned entries still carry.
    OpenDocument(koru_core::DocumentId),
    /// Legacy: a workspace node id. Kept so a pinned entry from before the
    /// store-scoped recents still resolves (or quietly does nothing).
    Open(koru_core::NodeId),
    /// Open the global search overlay, as though the agent's hotkey had been
    /// pressed. The taskbar's *Search* task asks for this.
    Search,
}

impl Activation {
    // Portable although only Windows sends one: it is the wire format the
    // test below pins, and pinning it on every platform is the point.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn line(&self) -> String {
        match self {
            Self::NewDocument => "new".to_string(),
            Self::AiChat => "ai-chat".to_string(),
            Self::OpenDocument(id) => format!("open-document {}", id.to_storage()),
            Self::Open(id) => format!("open {}", id.to_storage()),
            Self::Search => "search".to_string(),
        }
    }
}

/// Start (or contact) the activation channel. `true` means the request reached
/// a running Koru process and this short-lived taskbar process should exit.
pub fn prepare(request: Option<Activation>) -> bool {
    #[cfg(windows)]
    {
        let line = request.map(|r| r.line());
        windows_impl::prepare(line.as_deref())
    }
    #[cfg(not(windows))]
    {
        let _ = request;
        false
    }
}

// **The election is per build now.** This used to answer "is this process the
// one Koru that owns machine-wide things", and its only reader was the global
// hotkey — an election borrowed from the Jump List's activation pipe because
// there was nowhere better to put it. The Koru Agent is that owner now, by
// construction: binding its endpoint is the exclusion, and a second agent
// stands down before it ever calls `RegisterHotKey`. `ACTIVATION_OWNER` is
// still set below, because winning the pipe is still what decides which
// process serves a Recent entry — and the pipe name now carries the segment
// (and branch), so the election is scoped to this build rather than to the
// whole machine.

/// Install/update the native Jump List task for this executable.
pub fn install_jump_list() {
    #[cfg(windows)]
    windows_impl::install_jump_list();
}

/// Offer these documents under **Recent**, most recent first. Cheap and
/// idempotent: an unchanged list does nothing at all. Entries carry a stable
/// document id and its title; the store resolves the workspace on click, so no
/// profile folder is baked in.
pub fn set_recents(entries: Vec<(String, koru_core::DocumentId)>) {
    #[cfg(windows)]
    windows_impl::set_recents(entries);
    #[cfg(not(windows))]
    {
        let _ = entries;
    }
}

pub fn set_pinned(entries: Vec<(String, koru_core::DocumentId)>) {
    #[cfg(windows)]
    windows_impl::set_pinned(entries);
    #[cfg(not(windows))]
    {
        let _ = entries;
    }
}

/// Whether a taskbar entry asked for a document that still needs a frame.
pub fn open_pending() -> bool {
    #[cfg(windows)]
    {
        windows_impl::open_pending()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Consume one request to open a document.
pub fn take_open() -> Option<koru_core::NodeId> {
    #[cfg(windows)]
    {
        windows_impl::take_open().and_then(|id| koru_core::NodeId::parse(&id))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Whether a taskbar entry asked for a stable document that still needs the
/// store to resolve and open it.
pub fn open_document_pending() -> bool {
    #[cfg(windows)]
    {
        windows_impl::open_document_pending()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Consume one request to open a stable document.
pub fn take_unpinned() -> Vec<koru_core::DocumentId> {
    #[cfg(windows)]
    {
        windows_impl::take_unpinned()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

pub fn take_open_document() -> Option<koru_core::DocumentId> {
    #[cfg(windows)]
    {
        windows_impl::take_open_document()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Whether a running process has received a taskbar activation that still
/// needs a container frame.
pub fn new_document_pending() -> bool {
    #[cfg(windows)]
    {
        windows_impl::pending()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Consume one activation request.
pub fn take_new_document() -> bool {
    #[cfg(windows)]
    {
        windows_impl::take()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Consume one *Search* activation.
pub fn take_search() -> bool {
    #[cfg(windows)]
    {
        windows_impl::take_search()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Consume one *AI Chat* activation.
pub fn take_ai_chat() -> bool {
    #[cfg(windows)]
    {
        windows_impl::take_ai_chat()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Consume one "come forward" request, so the shell can focus its own window.
pub fn take_foreground() -> bool {
    #[cfg(windows)]
    {
        windows_impl::take_foreground()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Say which chord the *Search* task should advertise, and rebuild the Jump
/// List if it changed. `None` means no chord (or none yet).
pub fn set_shortcut(shortcut: Option<String>) {
    #[cfg(windows)]
    windows_impl::set_shortcut(shortcut);
    #[cfg(not(windows))]
    {
        let _ = shortcut;
    }
}

/// Hand the activation listener a proxy with which to wake the winit loop.
pub fn install_wake_proxy(
    proxy: winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>,
) {
    #[cfg(windows)]
    windows_impl::install_wake_proxy(proxy);
    #[cfg(not(windows))]
    {
        let _ = proxy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The title carries the chord because a Jump List task has one text field —
    /// no right-aligned accelerator the way the tray draws it. Plain `Search`
    /// when there is no chord to advertise: the task still works.
    #[test]
    fn the_search_task_folds_the_chord_into_its_title() {
        assert_eq!(
            search_task_title(Some("Ctrl+Shift+K")),
            "Search (Ctrl+Shift+K)"
        );
        assert_eq!(
            search_task_title(Some("Ctrl+Shift+J")),
            "Search (Ctrl+Shift+J)"
        );
        assert_eq!(search_task_title(None), "Search");
    }

    /// The activation line is the wire format (ADR-42): a verb added to one end
    /// and not the other is the failure mode this pins.
    #[test]
    fn the_search_activation_writes_its_own_line() {
        assert_eq!(Activation::Search.line(), "search");
        assert_eq!(Activation::AiChat.line(), "ai-chat");
        assert_eq!(Activation::NewDocument.line(), "new");
    }
}
