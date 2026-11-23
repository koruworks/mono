// SPDX-License-Identifier: MPL-2.0

//! **The first window a new person sees, driven from outside the process.**
//!
//! The pure suite in `onboarding/accessibility/tests.rs` says what the
//! first-run controller publishes. What it cannot say is the thing Plan U0.1
//! is actually for: *can a UI Automation client — the thing Narrator and
//! Accessibility Insights are — reach the account-free way into Koru, read its
//! name, and press it?* Before this plan the answer was no, and visibly so: the
//! window showed one element with nothing inside it.
//!
//! So this stands where a screen reader stands. It creates a UIA **client** in
//! the COM multi-threaded apartment, hosts a real
//! [`OnboardingView`](koru_workspace_host::onboarding::OnboardingView) in a
//! real window, reaches it through `ElementFromHandle`, finds *Continue
//! offline* by the locator an external tool would build, reads its name and
//! invokes it — and then asks whether the screen that went is gone.
//!
//! ## Why it is `#[ignore]`d
//!
//! It opens a real window on a real desktop and drives it. That is a separate
//! lane, invoked deliberately:
//!
//! ```text
//! cargo test -p koru-workspace-host --test accessibility_onboarding_native -- --ignored --nocapture
//! ```
//!
//! ## What it proves, and what it deliberately does not
//!
//! Proven: the first-run window answers a client at all; the account-free
//! control is findable by `AutomationId` without reading anything out of the
//! tree first; its `Name` is the localized *Continue offline*; `Invoke` on it
//! reaches the form and moves it to the setup card; the card's own controls
//! are then findable and the welcome screen's are **not**, which is
//! screen retirement seen from outside; a field's contents arrive on
//! `ValuePattern`; and a live region's setting arrives on
//! `UIA_LiveSettingPropertyId`.
//!
//! Not proven, and not this plan's gate: what a screen reader *says*, and
//! whether a person can complete setup on a fresh root with Narrator or NVDA.
//! That is Plan U0.2, and no number of assertions here stands in for it.

#![cfg(windows)]

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koru_window::{AccessibilityProvider, WindowBridge};
use koru_workspace_host::accessibility::{PaneAccessibility, Slots, WindowSemantics};
use koru_workspace_host::onboarding::OnboardingView;
use koru_workspace_host::onboarding::accessibility::parts;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::{Window, WindowId};

/// How long the client waits for the provider to answer. Generous: the first
/// `WM_GETOBJECT` starts UIA itself on this machine.
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
enum Wake {
    Accessibility,
    /// The client has read everything it needs from the setup card and wants
    /// a failed creation to look at. Driven from the client rather than on a
    /// timer so the sequence is deterministic: the form is asked for the state
    /// it already knows how to show, through the same `creation_failed` the
    /// creation worker calls.
    StageFailure,
    Finished,
}

/// What the client managed to see, in the order it asked.
#[derive(Debug, Default)]
struct Findings {
    has_native_provider: Option<bool>,
    root_automation_id: Option<String>,
    /// The account-free way in, found by the locator an external tool builds.
    found_continue_offline: bool,
    continue_offline_name: Option<String>,
    continue_offline_bounds: Option<(f64, f64, f64, f64)>,
    invoked: bool,
    /// After the invoke: the welcome screen's control must be **gone**, and the
    /// card's must be there. Screen retirement, from outside.
    welcome_control_after_invoke: bool,
    found_owner_field: bool,
    owner_field_name: Option<String>,
    /// A field's contents on `ValuePattern`, which is how Plan U0.1 publishes
    /// what somebody typed.
    owner_field_value: Option<String>,
    owner_field_is_read_only: Option<bool>,
    /// A live region's setting, read from a client. `2` is `Assertive`, `1` is
    /// `Polite`, `0` is `Off`.
    status_live_setting: Option<i32>,
    status_name: Option<String>,
    error: Option<String>,
}

struct Probe {
    slots: Slots,
    window: Option<Arc<Window>>,
    access: Option<PaneAccessibility>,
    view: OnboardingView,
    hwnd_tx: Option<Sender<isize>>,
    proxy: EventLoopProxy<Wake>,
    /// What the host's own routing resolved, which is the far side of the
    /// client's `Invoke`.
    routed: Arc<Mutex<Vec<(accesskit::Action, String)>>>,
    finished: bool,
}

impl ApplicationHandler<Wake> for Probe {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // Hidden, undecorated and unactivated — the shape the real first-run
        // window is created in, and the shape the adapter requires.
        // `with_active(false)` keeps this off whatever the person running it
        // was doing.
        let attributes = Window::default_attributes()
            .with_title("Koru first-run probe")
            .with_visible(false)
            .with_active(false)
            .with_decorations(false)
            .with_inner_size(winit::dpi::LogicalSize::new(1000.0, 680.0));
        let window = Arc::new(events.create_window(attributes).expect("create window"));

        let proxy = self.proxy.clone();
        let bridge = WindowBridge::before_first_reveal(
            events,
            &window,
            Arc::new(move || {
                let _ = proxy.send_event(Wake::Accessibility);
            }),
        );
        assert!(
            bridge.is_some(),
            "no AccessKit adapter was installed on a hidden window — the rest of this test \
             cannot mean anything"
        );
        let access = PaneAccessibility::new(
            self.slots.window(),
            self.slots.view(),
            bridge.map(|bridge| Box::new(bridge) as Box<dyn AccessibilityProvider>),
        );

        self.view.set_viewport(1000.0, 680.0);
        self.view.set_custom_caption(true);

        // The reveal the adapter had to be installed before.
        window.set_visible(true);

        if let Some(tx) = self.hwnd_tx.take() {
            let _ = tx.send(hwnd_of(&window));
        }
        self.window = Some(window);
        self.access = Some(access);
    }

    fn window_event(&mut self, _events: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let (Some(access), Some(window)) = (self.access.as_mut(), self.window.as_ref()) {
            access.process_event(window, &event);
        }
    }

    fn user_event(&mut self, _events: &ActiveEventLoop, wake: Wake) {
        match wake {
            Wake::Finished => self.finished = true,
            Wake::StageFailure => self.view.creation_failed("The Store refused the write"),
            Wake::Accessibility => {}
        }
    }

    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        let (Some(access), Some(window)) = (self.access.as_mut(), self.window.as_ref()) else {
            return;
        };
        if access.listening() {
            access.observe(self.view.semantics_revision());
        }
        if access.poll().snapshot {
            let scale = window.scale_factor();
            let size = window.inner_size();
            let slot = access.slot();
            let view_slot = access.view_slot();
            let mut children = koru_workspace_host::accessibility::window_commands(
                slot,
                false,
                self.view.language(),
                self.view.window_command_bounds(),
            );
            children.extend(self.view.accessibility_semantics(slot, view_slot));
            let semantics = WindowSemantics {
                window: slot,
                label: "Koru first-run probe".to_string(),
                scale,
                client: koru_draw::geometry::Rect::new(
                    0.0,
                    0.0,
                    size.width as f32 / scale as f32,
                    size.height as f32 / scale as f32,
                ),
                focus: self.view.accessibility_focus(slot, view_slot),
                children,
                // The shipping first-run window publishes whole snapshots too,
                // which is what retires the previous screen's ids — see
                // `App::onboarding_semantics` and ADR-341.
                scope: koru_workspace_host::accessibility::Scope::Whole,
                retired: Vec::new(),
            };
            access.publish(&semantics);
        }
        for resolved in access.resolve() {
            self.routed
                .lock()
                .expect("no test poisons this")
                .push((resolved.action, resolved.occurrence.locator()));
            // The same route the real host takes, so what is proved here is
            // what ships rather than a fixture.
            self.view.accessibility_action(
                resolved.occurrence,
                resolved.action,
                resolved.data.as_ref(),
            );
        }
        if self.finished {
            // `exit()` from `about_to_wait` is not honoured until the loop's next
            // message: winit blocks in `MsgWaitForMultipleObjectsEx` *before* it
            // reads the exit flag, so under `ControlFlow::Wait` this parks until
            // some unrelated message happens to arrive. See guide/contributing/deps/WINIT.md.
            events.set_control_flow(ControlFlow::Poll);
            events.exit();
        }
    }
}

fn hwnd_of(window: &Window) -> isize {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window
        .window_handle()
        .expect("a live window has a handle")
        .as_raw()
    {
        RawWindowHandle::Win32(handle) => handle.hwnd.get(),
        other => panic!("not a Win32 window: {other:?}"),
    }
}

#[test]
#[ignore = "interactive Windows proof: opens a real first-run window and drives it through UI Automation"]
fn a_uia_client_reaches_the_account_free_way_into_koru_and_presses_it() {
    let events = EventLoop::<Wake>::with_user_event()
        // The test harness runs on a worker thread; winit refuses to build an
        // event loop off the main thread on Windows without this.
        .with_any_thread(true)
        .build()
        .expect("an event loop");
    let proxy = events.create_proxy();
    let (hwnd_tx, hwnd_rx) = channel::<isize>();
    let routed = Arc::new(Mutex::new(Vec::new()));
    let findings = Arc::new(Mutex::new(Findings::default()));

    let client = {
        let proxy = proxy.clone();
        let findings = Arc::clone(&findings);
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                client_thread(&hwnd_rx, &findings, &proxy)
            }));
            if let Err(_panic) = outcome {
                findings.lock().expect("no test poisons this").error =
                    Some("the client thread panicked".to_string());
            }
            let _ = proxy.send_event(Wake::Finished);
        })
    };

    let mut probe = Probe {
        slots: Slots::default(),
        window: None,
        access: None,
        view: OnboardingView::default(),
        hwnd_tx: Some(hwnd_tx),
        proxy,
        routed: Arc::clone(&routed),
        finished: false,
    };
    events.run_app(&mut probe).expect("the loop ran");
    client.join().expect("the client thread joined");

    let findings = findings.lock().expect("no test poisons this");
    println!("{findings:#?}");
    assert_eq!(findings.error, None, "the client could not finish");

    let slot = probe
        .access
        .as_ref()
        .expect("the window existed")
        .slot()
        .raw();
    assert_eq!(findings.has_native_provider, Some(true));
    assert_eq!(
        findings.root_automation_id.as_deref(),
        Some(format!("koru:w{slot}/v0/s:window#window").as_str())
    );

    // ---------------------------------------------------- the way into Koru
    assert!(
        findings.found_continue_offline,
        "a client could not find the account-free way into Koru by its AutomationId. This is \
         the control a new person presses to reach their first document without an account."
    );
    assert_eq!(
        findings.continue_offline_name.as_deref(),
        Some("Continue offline")
    );
    let bounds = findings
        .continue_offline_bounds
        .expect("the control has bounds");
    assert!(
        bounds.2 > 0.0 && bounds.3 > 0.0,
        "the way in published an empty rectangle: {bounds:?}"
    );
    assert!(findings.invoked, "the client could not invoke the control");

    // ------------------------------------------- the screen that went, gone
    assert!(
        !findings.welcome_control_after_invoke,
        "a control from the screen that just went is still findable by its old locator: a \
         request queued against it would be routed rather than refused"
    );
    assert!(
        findings.found_owner_field,
        "the card's name field was not findable after the transition"
    );
    // The card's first field is named by its placeholder, *Your name*.
    assert_eq!(findings.owner_field_name.as_deref(), Some("Your name"));
    assert_eq!(
        findings.owner_field_value.as_deref(),
        Some(""),
        "a field's contents did not arrive on UIA's ValuePattern"
    );
    assert_eq!(findings.owner_field_is_read_only, Some(false));

    // -------------------------------------------------------- a live region
    assert_eq!(
        findings.status_live_setting,
        Some(2),
        "a status published as `Live::Assertive` did not arrive on \
         UIA_LiveSettingPropertyId. If this ever starts failing the projection has to find \
         another way to announce a failed creation — see guide/contributing/deps/ACCESSKIT.md"
    );
    assert_eq!(
        findings.status_name.as_deref(),
        Some("The Store refused the write"),
        "a live region announces its **name**, so the message has to be the label"
    );

    let routed = routed.lock().expect("no test poisons this");
    println!("routed: {routed:?}");
    let offline = format!("koru:w{slot}/v1/onb:1#{}", parts::CONTINUE_OFFLINE.name());
    assert_eq!(
        routed
            .iter()
            .filter(|(action, locator)| {
                *action == accesskit::Action::Click && *locator == offline
            })
            .count(),
        1,
        "the invoke did not arrive at the form exactly once as the occurrence the client named"
    );
}

fn client_thread(
    hwnd_rx: &Receiver<isize>,
    findings: &Arc<Mutex<Findings>>,
    proxy: &EventLoopProxy<Wake>,
) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{
        CUIAutomation8, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
        IUIAutomationValuePattern, TreeScope_Descendants, UIA_AutomationIdPropertyId,
        UIA_InvokePatternId, UIA_LiveSettingPropertyId, UIA_ValuePatternId,
        UiaHasServerSideProvider,
    };

    let raw = hwnd_rx
        .recv_timeout(TIMEOUT)
        .expect("the window was created");
    let hwnd = HWND(raw as *mut core::ffi::c_void);

    // The client initialises COM; the provider deliberately does not.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .expect("COM initialised");
    let _com = scopeguard(|| unsafe { CoUninitialize() });

    findings
        .lock()
        .expect("no test poisons this")
        .has_native_provider = Some(unsafe { UiaHasServerSideProvider(hwnd) }.as_bool());

    let uia: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
            .expect("a UI Automation client");

    // **Poll rather than assume.** The first `ElementFromHandle` is what sends
    // `WM_GETOBJECT`; the adapter answers with its own placeholder tree and the
    // real one lands on the next pass of the window's loop.
    let deadline = Instant::now() + TIMEOUT;
    let mut root: Option<IUIAutomationElement> = None;
    let mut automation_id = String::new();
    while Instant::now() < deadline {
        let element = unsafe { uia.ElementFromHandle(hwnd) }.expect("the window has an element");
        automation_id = unsafe { element.CurrentAutomationId() }
            .map(|id| id.to_string())
            .unwrap_or_default();
        if automation_id.starts_with("koru:") {
            root = Some(element);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let root = root.expect("the window root never published a Koru locator");
    findings
        .lock()
        .expect("no test poisons this")
        .root_automation_id = Some(automation_id.clone());

    // The locator, built the way an external tool has to build one: the window
    // number out of the root's own AutomationId, the view slot the allocator
    // hands out first, the screen instance a fresh window opens on, and the
    // control's own keyword. Nothing is read out of the tree to find it.
    let slot = automation_id
        .trim_start_matches("koru:w")
        .split('/')
        .next()
        .expect("a window number")
        .to_string();
    let view = 1;
    let welcome = 1;
    let find_by_id = |locator: &str| -> Option<IUIAutomationElement> {
        let condition = unsafe {
            uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(locator))
        }
        .ok()?;
        unsafe { root.FindFirst(TreeScope_Descendants, &condition) }.ok()
    };

    let offline_locator = format!(
        "koru:w{slot}/v{view}/onb:{welcome}#{}",
        parts::CONTINUE_OFFLINE.name()
    );
    // The first tree may still be settling; the control is what tells us it
    // landed.
    let deadline = Instant::now() + TIMEOUT;
    let mut offline = None;
    while Instant::now() < deadline {
        if let Some(element) = find_by_id(&offline_locator) {
            offline = Some(element);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let offline = offline.expect("a client could not find Continue offline by its locator");
    {
        let mut found = findings.lock().expect("no test poisons this");
        found.found_continue_offline = true;
        found.continue_offline_name = unsafe { offline.CurrentName() }.ok().map(|s| s.to_string());
        if let Ok(rect) = unsafe { offline.CurrentBoundingRectangle() } {
            found.continue_offline_bounds = Some((
                f64::from(rect.left),
                f64::from(rect.top),
                f64::from(rect.right - rect.left),
                f64::from(rect.bottom - rect.top),
            ));
        }
    }

    // Press it. This is the whole point of the plan: the account-free way into
    // Koru, reached and used by a client that has never seen the window.
    let invoke: IUIAutomationInvokePattern =
        unsafe { offline.GetCurrentPatternAs(UIA_InvokePatternId) }
            .expect("the way in offers the Invoke pattern");
    unsafe { invoke.Invoke() }.expect("the control was invoked");
    findings.lock().expect("no test poisons this").invoked = true;

    // ------------------------------------------ the screen that went, gone
    //
    // The card's name field arriving is what says the transition landed;
    // the welcome screen's control still being findable would say a stale
    // request could be routed.
    let owner_locator = format!("koru:w{slot}/v{view}/onb:2#{}", parts::OWNER_NAME.name());
    let deadline = Instant::now() + TIMEOUT;
    let mut owner = None;
    while Instant::now() < deadline {
        if let Some(element) = find_by_id(&owner_locator) {
            owner = Some(element);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let owner = owner.expect("the card never published its name field");
    {
        let mut found = findings.lock().expect("no test poisons this");
        found.found_owner_field = true;
        found.welcome_control_after_invoke = find_by_id(&offline_locator).is_some();
        found.owner_field_name = unsafe { owner.CurrentName() }.ok().map(|s| s.to_string());
        if let Ok(value) =
            unsafe { owner.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
        {
            found.owner_field_value = unsafe { value.CurrentValue() }.ok().map(|s| s.to_string());
            found.owner_field_is_read_only = unsafe { value.CurrentIsReadOnly() }
                .ok()
                .map(|b| b.as_bool());
        }
    }

    // ----------------------------------------------------------- the status
    //
    // Ask the form for the failure it already knows how to show. The window's
    // loop calls the shipping `creation_failed`, so what is read back here is
    // the projection that ships rather than a fixture. A failure on the card
    // that was not finishing is no transition, so the status belongs to the
    // card's own instance, the second screen this window has published.
    let _ = proxy.send_event(Wake::StageFailure);
    let status_locator = format!("koru:w{slot}/v{view}/onb:2#{}", parts::STATUS.name());
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if let Some(status) = find_by_id(&status_locator) {
            let mut found = findings.lock().expect("no test poisons this");
            found.status_name = unsafe { status.CurrentName() }.ok().map(|s| s.to_string());
            found.status_live_setting =
                unsafe { status.GetCurrentPropertyValue(UIA_LiveSettingPropertyId) }
                    .ok()
                    .and_then(|value| i32::try_from(&value).ok());
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Give the window's loop a moment to drain the requests it was handed.
    std::thread::sleep(Duration::from_millis(250));
}

/// `CoUninitialize` on the way out, whichever way the client leaves.
fn scopeguard(f: impl FnOnce()) -> impl Drop {
    struct Guard<F: FnOnce()>(Option<F>);
    impl<F: FnOnce()> Drop for Guard<F> {
        fn drop(&mut self) {
            if let Some(f) = self.0.take() {
                f();
            }
        }
    }
    Guard(Some(f))
}
