// SPDX-License-Identifier: MPL-2.0

//! The P7.1 qualification program (contract section 2).
//!
//! **This program starts a real browser.** It is run by a person, or inside
//! Windows Sandbox. No test runs it, and it is never run by an agent on a
//! person's desktop: a driven window can take focus and receive keystrokes.
//!
//! ```text
//! qualify <browser executable> <output folder> [options]
//!
//!   --part a|b|c         which part of the runbook to run (default a)
//!   --scratch <folder>   where the test profiles go (default <output>\profiles)
//!   --unattended         nobody is at the keyboard: steps that need a hand
//!                        are recorded "not run"
//!   --address <address>  part b: the issue list's address
//!   --expect <text>      part b: text the pages must hold (repeatable)
//! ```
//!
//! Part A needs no account. It serves the fixture site on two loopback
//! ports, runs steps A0 to A12 in order, and stops to say what to do by hand
//! where a step needs one. Part B is the real site; part C is A0 to A2 on a
//! managed computer.
//!
//! It writes, into the output folder only: `M7.P7.1.QUALIFICATION.md`, a
//! skeleton with each step's result and numbers, and `transcripts/<step>.txt`,
//! the messages sent and received with every address reduced to its site. It
//! never writes page text, a full address, a cookie or a raw frame. It reads
//! nothing but the profiles it made.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use koru_browser_control::fixture::{
    BIG_PAGE_BYTES, FixtureRequest, FixtureSite, ISSUES, MARK_FIELD, MARK_TEXT, SIGN_IN_CODE, Site,
};
use koru_browser_control::installed::{self, Probe};
use koru_browser_control::launch::{
    self, Attachment, BrowserStop, DiskFolders, LaunchPlan, ProcessTree, ProfileFolderName,
    WindowShow, plain_path, prepare_profile_folder,
};
use koru_browser_control::origin::{AddressSite, BrowserOrigin, PageAddress, StartingAddress};
use koru_browser_control::protocol::{
    self, Command, CommandId, CommandKind, ConstantScript, DecodeFailure, DecodeScope, Evaluation,
    Event, FrameHead, FrameId, LifecycleStage, NavigationStart, Refusal, Reply, SessionId,
    TargetKind,
};
use koru_browser_control::transport::{FrameSink, PipeTransport, Transport, TransportFailure};

// The provisional budgets of contract section 4.5.
const LAUNCH_REPLY: Duration = Duration::from_secs(15);
const ONE_COMMAND: Duration = Duration::from_secs(10);
const ONE_NAVIGATION: Duration = Duration::from_secs(15);
const ONE_READ: Duration = Duration::from_secs(5);

fn locked<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------ the records

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Observed,
    Failed,
    NotRun,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Failed => "failed",
            Self::NotRun => "not run",
        }
    }
}

struct StepRecord {
    id: &'static str,
    confirms: &'static str,
    status: Status,
    notes: Vec<String>,
}

/// What one step writes down: its notes, and its frame transcript.
#[derive(Default)]
struct Notes {
    lines: Vec<String>,
    transcript: Vec<String>,
}

impl Notes {
    fn note(&mut self, text: impl Into<String>) {
        let text = text.into();
        println!("    {text}");
        self.lines.push(text);
    }
}

// ------------------------------------------------- the fixture, on two ports

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        302 => "Found",
        303 => "See Other",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Status",
    }
}

fn position(bytes: &[u8], wanted: &[u8]) -> Option<usize> {
    bytes
        .windows(wanted.len())
        .position(|window| window == wanted)
}

/// Answer one connection with the fixture site's own answer.
fn answer(
    site: &FixtureSite,
    which: Site,
    mut stream: TcpStream,
    stopping: &AtomicBool,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut received = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(at) = position(&received, b"\r\n\r\n") {
            break at + 4;
        }
        if received.len() > 64 * 1024 {
            return Ok(());
        }
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Ok(());
        }
        received.extend_from_slice(&chunk[..count]);
    };
    let head = String::from_utf8_lossy(&received[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("");
    let target = first.next().unwrap_or("/");
    let mut cookies = String::new();
    let mut length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            match name.to_ascii_lowercase().as_str() {
                "cookie" => cookies = value.trim().to_owned(),
                "content-length" => length = value.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let length = length.min(1024 * 1024);
    let mut body = received[end..].to_vec();
    while body.len() < length {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..count]);
    }
    let response = site.respond(&FixtureRequest {
        site: which,
        method,
        target,
        cookies: &cookies,
        body: &body,
    });
    let mut out = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason(response.status)
    );
    for (name, value) in &response.headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    if !response.never_finishes {
        out.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    }
    out.push_str("Connection: close\r\n\r\n");
    stream.write_all(out.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()?;
    // A response that never finishes: the connection stays open, unfinished,
    // until the program ends.
    while response.never_finishes && !stopping.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

fn serve(site: Arc<FixtureSite>, which: Site, listener: TcpListener, stopping: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stopping.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { continue };
            let site = site.clone();
            let stopping = stopping.clone();
            std::thread::spawn(move || {
                let _ = answer(&site, which, stream, &stopping);
            });
        }
    });
}

// ------------------------------------------------------------- one browser

enum Arrival {
    Frame(Vec<u8>),
    Closed,
    Failed(TransportFailure),
}

/// What the program does with a paused top-level document request.
enum Policy {
    /// Interception is off.
    Off,
    /// Only this site may be opened at top level.
    Only(BrowserOrigin),
    /// Everything is let through and its site recorded (part B).
    Any,
}

#[derive(Default)]
struct Tally {
    refused: u32,
    continued: u32,
    tabs_closed: u32,
    dialogs: u32,
    decode_failures: u32,
    /// Every site a top-level document was requested from.
    top_sites: BTreeSet<String>,
    /// Events Koru does not decode, by name.
    unlisted: BTreeMap<String, u32>,
    /// Decoded events, by name.
    listed: BTreeMap<&'static str, u32>,
}

struct Navigation {
    start: NavigationStart,
    loaded: bool,
    took: Duration,
}

/// What a read returned. Held in memory for the checks below and never
/// written anywhere.
struct Reading {
    title: String,
    text: String,
    total: u64,
    links: Vec<(String, String)>,
    more_links: bool,
}

struct Browser {
    process_id: u32,
    tree: Arc<dyn ProcessTree>,
    stop: BrowserStop,
    sink: Arc<Mutex<Box<dyn FrameSink>>>,
    arrivals: Receiver<(Instant, Arrival)>,
    next_id: u64,
    pending: HashMap<u64, CommandKind>,
    session: Option<SessionId>,
    top_frame: Option<FrameId>,
    top_site: Option<AddressSite>,
    page: Option<PageAddress>,
    policy: Policy,
    lines: Vec<String>,
    events: Vec<(Instant, Event)>,
    tally: Tally,
    pipe_closed: Option<Instant>,
    ready: bool,
}

impl Browser {
    fn open(executable: &Path, profile: &Path, show: WindowShow) -> Result<Self, String> {
        let plan = LaunchPlan::new(executable, profile)
            .map_err(|failure| format!("the launch plan was refused ({:?})", failure.stage))?
            .showing(show);
        let launched = launch::launch(&plan).map_err(|failure| {
            format!(
                "the launch failed at {:?}, system code {:?}",
                failure.stage, failure.code
            )
        })?;
        let pipe = launched.pipe.ok_or("the launch gave no pipe")?;
        let transport: Box<dyn Transport> =
            Box::new(PipeTransport::new(pipe.from_browser, pipe.to_browser));
        let (mut source, sink) = transport.split();
        let (sender, arrivals) = mpsc::channel();
        std::thread::Builder::new()
            .name("qualify-reader".into())
            .spawn(move || {
                loop {
                    let arrival = match source.receive() {
                        Ok(frame) => Arrival::Frame(frame),
                        Err(TransportFailure::Closed) => Arrival::Closed,
                        Err(other) => Arrival::Failed(other),
                    };
                    let goes_on = matches!(
                        arrival,
                        Arrival::Frame(_) | Arrival::Failed(TransportFailure::Oversize { .. })
                    );
                    if sender.send((Instant::now(), arrival)).is_err() || !goes_on {
                        break;
                    }
                }
            })
            .map_err(|_| "the reader thread could not be started")?;
        let sink = Arc::new(Mutex::new(sink));
        let closer = sink.clone();
        launched.stop.set_orderly_close(move || {
            let _ = locked(&closer).send(&Command::Close.encode(CommandId(0), None));
        });
        Ok(Self {
            process_id: launched.process_id,
            tree: launched.tree,
            stop: launched.stop,
            sink,
            arrivals,
            next_id: 1,
            pending: HashMap::new(),
            session: None,
            top_frame: None,
            top_site: None,
            page: None,
            policy: Policy::Off,
            lines: Vec::new(),
            events: Vec::new(),
            tally: Tally::default(),
            pipe_closed: None,
            ready: false,
        })
    }

    /// Send a command without waiting for its reply.
    fn send(&mut self, command: &Command, on_page: bool) -> Result<u64, String> {
        let session = if on_page {
            Some(self.session.clone().ok_or("no tab is attached")?)
        } else {
            None
        };
        let id = self.next_id;
        self.next_id += 1;
        self.lines
            .push(command.record(CommandId(id), session.as_ref()));
        locked(&self.sink)
            .send(&command.encode(CommandId(id), session.as_ref()))
            .map_err(|failure| format!("{}: not sent ({failure:?})", command.kind().name()))?;
        self.pending.insert(id, command.kind());
        Ok(id)
    }

    fn failure(&mut self, failure: DecodeFailure) {
        self.tally.decode_failures += 1;
        self.lines.push(format!(
            "? not decoded: message={} length={} reason={:?}",
            failure.message, failure.length, failure.reason
        ));
    }

    /// Wait at most `limit` for one arrival and act on it. Answers the reply
    /// it was, when it was one.
    fn step(&mut self, limit: Duration) -> Option<(u64, Result<Reply, Refusal>)> {
        let (at, arrival) = match self.arrivals.recv_timeout(limit) {
            Ok(arrival) => arrival,
            Err(RecvTimeoutError::Timeout) => return None,
            Err(RecvTimeoutError::Disconnected) => {
                self.pipe_closed.get_or_insert_with(Instant::now);
                std::thread::sleep(limit.min(Duration::from_millis(20)));
                return None;
            }
        };
        match arrival {
            Arrival::Closed => {
                self.pipe_closed.get_or_insert(at);
                self.lines.push("# the pipe closed".to_owned());
                None
            }
            Arrival::Failed(failure) => {
                self.lines.push(format!("# transport: {failure:?}"));
                None
            }
            Arrival::Frame(frame) => self.frame(&frame, at),
        }
    }

    fn frame(&mut self, frame: &[u8], at: Instant) -> Option<(u64, Result<Reply, Refusal>)> {
        match protocol::classify(frame) {
            Err(failure) => {
                self.failure(failure);
                None
            }
            Ok(FrameHead::Unlisted { name }) => {
                let name = name.unwrap_or_else(|| "(not a plain name)".to_owned());
                *self.tally.unlisted.entry(name).or_default() += 1;
                None
            }
            Ok(FrameHead::Reply { id, .. }) => {
                let Some(kind) = self.pending.remove(&id.0) else {
                    self.lines.push(format!("< {} (no such command)", id.0));
                    return None;
                };
                match protocol::decode_reply(frame, kind) {
                    Err(failure) => {
                        self.failure(failure);
                        Some((id.0, Err(Refusal { code: i64::MIN })))
                    }
                    Ok(Err(refusal)) => {
                        self.lines
                            .push(format!("< {} refused code={}", id.0, refusal.code));
                        Some((id.0, Err(refusal)))
                    }
                    Ok(Ok(reply)) => {
                        self.lines.push(reply.record(id));
                        if let Reply::Navigated { frame, .. } = &reply {
                            // The frame a navigation of the tab starts in is
                            // its top frame.
                            self.top_frame.get_or_insert_with(|| frame.clone());
                        }
                        Some((id.0, Ok(reply)))
                    }
                }
            }
            Ok(FrameHead::Event { name, .. }) => {
                let scope = DecodeScope {
                    top_frame: self.top_frame.as_ref(),
                };
                match protocol::decode_event(frame, name, scope) {
                    Err(failure) => self.failure(failure),
                    Ok(event) => {
                        self.lines.push(event.record());
                        *self.tally.listed.entry(name.name()).or_default() += 1;
                        self.react(&event);
                        self.events.push((at, event));
                    }
                }
                None
            }
        }
    }

    /// What the program does by itself when an event arrives.
    fn react(&mut self, event: &Event) {
        let follow_up = match event {
            Event::FrameNavigated {
                frame,
                top: true,
                site,
                address,
            } => {
                self.top_frame = Some(frame.clone());
                self.top_site = Some(site.clone());
                self.page = address.clone();
                self.tally.top_sites.insert(site.record().to_owned());
                None
            }
            Event::NavigatedWithinDocument {
                site,
                address: Some(address),
                ..
            } => {
                self.top_site = Some(site.clone());
                self.page = Some(address.clone());
                None
            }
            Event::RequestPaused {
                request,
                frame,
                site,
                document,
                ..
            } => {
                // Until the top frame is known, every document is treated as
                // top-level: the stricter reading.
                let top = *document && self.top_frame.as_ref().is_none_or(|known| known == frame);
                let allowed = match &self.policy {
                    Policy::Off | Policy::Any => true,
                    Policy::Only(origin) => site.origin() == Some(origin),
                };
                if top {
                    self.tally.top_sites.insert(site.record().to_owned());
                }
                if top && !allowed {
                    self.tally.refused += 1;
                    Some((
                        Command::FailRequest {
                            request: request.clone(),
                        },
                        true,
                    ))
                } else {
                    self.tally.continued += 1;
                    Some((
                        Command::ContinueRequest {
                            request: request.clone(),
                        },
                        true,
                    ))
                }
            }
            Event::TargetCreated(info) if info.opened_by_page => {
                self.tally.tabs_closed += 1;
                Some((
                    Command::CloseTarget {
                        target: info.target.clone(),
                    },
                    false,
                ))
            }
            Event::DialogOpening { .. } => {
                self.tally.dialogs += 1;
                Some((Command::DismissDialog, true))
            }
            _ => None,
        };
        if let Some((command, on_page)) = follow_up
            && let Err(problem) = self.send(&command, on_page)
        {
            self.lines.push(format!("# {problem}"));
        }
    }

    /// Send a command and wait for its reply, acting on events meanwhile.
    fn call(&mut self, command: Command, on_page: bool, limit: Duration) -> Result<Reply, String> {
        let name = command.kind().name();
        let id = self.send(&command, on_page)?;
        let until = Instant::now() + limit;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.lines
                    .push(format!("# {id} no reply in {} ms", limit.as_millis()));
                return Err(format!("{name}: no reply in {} ms", limit.as_millis()));
            }
            if self.pipe_closed.is_some() {
                return Err(format!("{name}: the pipe closed"));
            }
            match self.step(left.min(Duration::from_millis(100))) {
                Some((got, Ok(reply))) if got == id => return Ok(reply),
                Some((got, Err(refusal))) if got == id => {
                    return Err(format!("{name}: refused, code {}", refusal.code));
                }
                _ => {}
            }
        }
    }

    /// Act on arrivals until `done` or the limit. `true` when done.
    fn wait(&mut self, limit: Duration, mut done: impl FnMut(&Self) -> bool) -> bool {
        let until = Instant::now() + limit;
        loop {
            if done(self) {
                return true;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            self.step(left.min(Duration::from_millis(50)));
        }
    }

    fn settle(&mut self, time: Duration) {
        self.wait(time, |_| false);
    }

    /// The version reply: the first thing asked of a new browser.
    fn version(&mut self) -> Result<protocol::VersionInfo, String> {
        match self.call(Command::GetVersion, false, LAUNCH_REPLY)? {
            Reply::Version(info) => Ok(info),
            _ => Err("the version reply was not a version".to_owned()),
        }
    }

    /// Attach to the window's tab and turn on what every later step needs.
    /// Nothing is navigated before interception is acknowledged.
    fn prepare(&mut self, policy: Policy) -> Result<(), String> {
        if self.ready {
            return Ok(());
        }
        let Reply::Targets(targets) = self.call(Command::GetTargets, false, ONE_COMMAND)? else {
            return Err("the tab list was not a tab list".to_owned());
        };
        let tab = targets
            .iter()
            .find(|info| info.kind == TargetKind::Page)
            .ok_or("the browser lists no tab")?
            .target
            .clone();
        let Reply::Attached { session } =
            self.call(Command::AttachToTarget { target: tab }, false, ONE_COMMAND)?
        else {
            return Err("attaching gave no session".to_owned());
        };
        self.session = Some(session);
        self.call(Command::EnablePage, true, ONE_COMMAND)?;
        self.call(
            Command::SetLifecycleEvents { enabled: true },
            true,
            ONE_COMMAND,
        )?;
        self.call(
            Command::SetDiscoverTargets { discover: true },
            false,
            ONE_COMMAND,
        )?;
        self.call(Command::DenyDownloads, false, ONE_COMMAND)?;
        self.policy = policy;
        if !matches!(self.policy, Policy::Off) {
            self.call(Command::EnableInterception, true, ONE_COMMAND)?;
        }
        self.ready = true;
        Ok(())
    }

    fn navigate(&mut self, address: PageAddress, limit: Duration) -> Result<Navigation, String> {
        let started = Instant::now();
        let mark = self.events.len();
        let reply = self.call(Command::Navigate { address }, true, limit.max(ONE_COMMAND))?;
        let Reply::Navigated { start, .. } = reply else {
            return Err("the navigation reply was not one".to_owned());
        };
        let loaded = start == NavigationStart::Started
            && self.wait(limit.saturating_sub(started.elapsed()), |browser| {
                browser.events[mark..].iter().any(|(_, event)| {
                    matches!(
                        event,
                        Event::Lifecycle { frame, stage: LifecycleStage::Load }
                            if Some(frame) == browser.top_frame.as_ref()
                    )
                })
            });
        Ok(Navigation {
            start,
            loaded,
            took: started.elapsed(),
        })
    }

    /// Read the page with the constant script, in an isolated world.
    fn read(&mut self) -> Result<Reading, String> {
        let frame = self.top_frame.clone().ok_or("no top frame is known yet")?;
        let Reply::World { context } =
            self.call(Command::CreateIsolatedWorld { frame }, true, ONE_READ)?
        else {
            return Err("no isolated world".to_owned());
        };
        let evaluate = Command::Evaluate {
            script: ConstantScript::READ,
            context,
        };
        let Reply::Evaluated(Evaluation::Value(value)) = self.call(evaluate, true, ONE_READ)?
        else {
            return Err("the read script threw".to_owned());
        };
        let json = value.json();
        let text = |value: &serde_json::Value| value.as_str().unwrap_or("").to_owned();
        Ok(Reading {
            title: text(&json["title"]),
            text: text(&json["text"]),
            total: json["total"].as_u64().unwrap_or(0),
            links: json["links"]
                .as_array()
                .map(|links| {
                    links
                        .iter()
                        .map(|link| (text(&link["text"]), text(&link["address"])))
                        .collect()
                })
                .unwrap_or_default(),
            more_links: json["more_links"].as_bool().unwrap_or(false),
        })
    }

    /// The current page's site and path, for a note. Never its query.
    fn place(&self) -> String {
        let site = self
            .top_site
            .as_ref()
            .map_or("(unknown)", |site| site.record());
        let path = self
            .page
            .as_ref()
            .map(PageAddress::path)
            .unwrap_or_default();
        format!("{site}{path}")
    }

    fn path(&self) -> String {
        self.page
            .as_ref()
            .map(PageAddress::path)
            .unwrap_or_default()
    }

    /// Move this browser's transcript into the step's, with the counts of
    /// the events that were not decoded.
    fn hand_over(&mut self, notes: &mut Notes) {
        notes.transcript.append(&mut self.lines);
        for (name, count) in std::mem::take(&mut self.tally.unlisted) {
            notes
                .transcript
                .push(format!("! {name} (not decoded) x{count}"));
        }
        self.events.clear();
    }

    /// Close through the stop handle and wait for the tree to be gone.
    fn shut(mut self, notes: &mut Notes) {
        self.stop.close();
        self.stop.wait_closed(Duration::from_secs(10));
        self.hand_over(notes);
    }
}

// ------------------------------------------------------------ the keyboard

#[cfg(windows)]
fn foreground_process() -> Option<u32> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return None;
    }
    let mut process = 0u32;
    unsafe { GetWindowThreadProcessId(window, &mut process) };
    (process != 0).then_some(process)
}

#[cfg(not(windows))]
fn foreground_process() -> Option<u32> {
    None
}

/// Whether the foreground window belonged to the browser at any moment of
/// `time`. `None` when the foreground window cannot be told.
fn took_foreground(browser: &mut Browser, time: Duration) -> Option<bool> {
    foreground_process()?;
    let until = Instant::now() + time;
    let mut took = false;
    while Instant::now() < until {
        browser.step(Duration::from_millis(50));
        if let Some(process) = foreground_process() {
            took |= browser.tree.process_ids().contains(&process);
        }
    }
    Some(took)
}

// ----------------------------------------------------------------- the run

struct Run {
    executable: PathBuf,
    out: PathBuf,
    scratch: PathBuf,
    unattended: bool,
    address: Option<String>,
    expect: Vec<String>,
    site: Arc<FixtureSite>,
    typed: Receiver<String>,
    main: Option<Browser>,
    signed_in: bool,
    records: Vec<StepRecord>,
}

impl Run {
    fn a(&self, path: &str) -> Result<PageAddress, String> {
        let address = format!("{}{path}", self.site.address(Site::A));
        PageAddress::reported(&address).ok_or_else(|| "not an address".to_owned())
    }

    fn b(&self, path: &str) -> Result<PageAddress, String> {
        let address = format!("{}{path}", self.site.address(Site::B));
        PageAddress::reported(&address).ok_or_else(|| "not an address".to_owned())
    }

    fn only_site_a(&self) -> Result<Policy, String> {
        BrowserOrigin::parse(self.site.address(Site::A))
            .map(Policy::Only)
            .map_err(|error| format!("site A is not a site ({error:?})"))
    }

    /// The profile folder of this name under the scratch folder, made by
    /// the crate's own folder rule.
    fn profile(&self, name: &str) -> Result<PathBuf, String> {
        let name = ProfileFolderName::new(name).ok_or("not a profile folder name")?;
        prepare_profile_folder(&DiskFolders, &self.scratch, &name)
            .map(|folder| plain_path(&folder))
            .map_err(|failure| format!("the profile folder was refused ({:?})", failure.stage))
    }

    /// Say what to do by hand and wait for Enter, acting on the browser's
    /// events meanwhile so that the window keeps working. `None` when nobody
    /// is at the keyboard.
    fn ask(&self, prompt: &str, mut browser: Option<&mut Browser>) -> Option<String> {
        if self.unattended {
            return None;
        }
        println!("\n  >> {prompt}");
        print!("  >> then press Enter (you may type a note first): ");
        let _ = std::io::stdout().flush();
        while self.typed.try_recv().is_ok() {}
        loop {
            match browser.as_deref_mut() {
                Some(browser) => {
                    browser.step(Duration::from_millis(50));
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
            match self.typed.try_recv() {
                Ok(line) => return Some(line.trim().to_owned()),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => return None,
            }
        }
    }

    /// Ask, and write down the answer or that nobody answered.
    fn observe(&self, notes: &mut Notes, question: &str, browser: Option<&mut Browser>) -> bool {
        match self.ask(question, browser) {
            Some(answer) => {
                notes.note(format!("person: {question} -> {answer:?}"));
                true
            }
            None => {
                notes.note(format!("not run, needs a person: {question}"));
                false
            }
        }
    }

    fn open_main(&mut self) -> Result<Browser, String> {
        let profile = self.profile("main")?;
        Browser::open(&self.executable, &profile, WindowShow::NoActivate)
    }

    /// Run `body` with the main browser, starting it if there is none. The
    /// browser is kept for the next step only if the step succeeded and the
    /// pipe is still open.
    fn with_main(
        &mut self,
        notes: &mut Notes,
        body: impl FnOnce(&mut Self, &mut Browser, &mut Notes) -> Result<Status, String>,
    ) -> Result<Status, String> {
        let mut browser = match self.main.take() {
            Some(browser) => browser,
            None => self.open_main()?,
        };
        let result = body(self, &mut browser, notes);
        if result.is_ok() && browser.pipe_closed.is_none() {
            browser.hand_over(notes);
            self.main = Some(browser);
        } else {
            browser.shut(notes);
        }
        result
    }

    fn close_main(&mut self, notes: &mut Notes) {
        if let Some(browser) = self.main.take() {
            browser.shut(notes);
        }
    }

    fn step(
        &mut self,
        id: &'static str,
        confirms: &'static str,
        body: fn(&mut Self, &mut Notes) -> Result<Status, String>,
    ) -> Status {
        println!("\n== {id} ==");
        let mut notes = Notes::default();
        let status = match body(self, &mut notes) {
            Ok(status) => status,
            Err(problem) => {
                notes.note(format!("failed: {problem}"));
                Status::Failed
            }
        };
        println!("   {id}: {}", status.word());
        let folder = self.out.join("transcripts");
        let written = std::fs::create_dir_all(&folder).and_then(|()| {
            std::fs::write(
                folder.join(format!("{id}.txt")),
                notes.transcript.join("\n"),
            )
        });
        if written.is_err() {
            notes.note("the transcript could not be written");
        }
        self.records.push(StepRecord {
            id,
            confirms,
            status,
            notes: notes.lines,
        });
        status
    }

    fn skip(&mut self, id: &'static str, confirms: &'static str, why: &str) {
        println!("\n== {id} ==\n   {id}: not run ({why})");
        self.records.push(StepRecord {
            id,
            confirms,
            status: Status::NotRun,
            notes: vec![why.to_owned()],
        });
    }
}

fn milliseconds(time: Duration) -> u128 {
    time.as_millis()
}

fn system_version() -> String {
    std::process::Command::new("cmd")
        .args(["/c", "ver"])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "(could not be read)".to_owned())
}

/// Bytes on disk, the longest full path in characters, and whether a file
/// of this name is anywhere under the folder.
fn survey(folder: &Path, wanted: &str) -> (u64, usize, bool) {
    let (mut bytes, mut deepest, mut found) = (0u64, 0usize, false);
    let mut left = vec![folder.to_path_buf()];
    while let Some(next) = left.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            deepest = deepest.max(path.as_os_str().len());
            found |= entry.file_name().to_string_lossy() == wanted;
            match entry.metadata() {
                Ok(data) if data.is_dir() => left.push(path),
                Ok(data) => bytes += data.len(),
                Err(_) => {}
            }
        }
    }
    (bytes, deepest, found)
}

// --------------------------------------------------------------- the steps

const NAMED_ARGUMENTS: &str = "--user-data-dir=<profile> --remote-debugging-pipe \
     --remote-debugging-io-pipes=<read handle>,<write handle> --no-first-run \
     --no-default-browser-check --new-window about:blank";

fn a0(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    notes.note(format!(
        "the program started; this build can start a browser: {}",
        launch::CAN_LAUNCH
    ));
    notes.note(format!("Windows: {}", system_version()));
    notes.note(format!(
        "executable path given: {}",
        run.executable.display()
    ));
    let present = run.executable.is_file();
    notes.note(format!("an executable is present at that path: {present}"));
    match installed::probe(Some(&run.executable)) {
        Probe::Found(found) => notes.note(format!(
            "version read from the file, without starting it: {}",
            found.version
        )),
        other => notes.note(format!("version probe of the file: {other:?}")),
    }
    match installed::probe(None) {
        Probe::Found(found) => notes.note(format!(
            "the registered location names an executable, version {}; same file as given: {}",
            found.version,
            found.executable == run.executable
        )),
        other => notes.note(format!("probe of the registered location: {other:?}")),
    }
    if !launch::CAN_LAUNCH {
        return Err("this build cannot start a browser".to_owned());
    }
    if !present {
        return Err("no executable at the given path".to_owned());
    }
    Ok(Status::Observed)
}

fn a1(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    notes.note(format!("arguments: {NAMED_ARGUMENTS}"));
    notes.note("the window is asked to show without becoming active");
    run.with_main(notes, |run, browser, notes| {
        browser.settle(Duration::from_secs(3));
        let active = browser.tree.active_processes();
        notes.note(format!(
            "process {} started; processes in its tree after 3 s: {active:?}",
            browser.process_id
        ));
        if active == Some(0) {
            return Err(
                "every process had exited within 3 s: refused, or handed to a running browser"
                    .to_owned(),
            );
        }
        for question in [
            "A1: Did a browser window open on an empty page? (yes/no)",
            "A1: Did any first-run, sync or 'sign in to the browser' screen appear? Describe it.",
            "A1: Does the window show a banner or any other sign that it is being driven? Describe it.",
            "A1: In the window's settings, which services that send addresses to the browser's maker are switched on in this fresh profile?",
        ] {
            run.observe(notes, question, Some(&mut *browser));
        }
        Ok(Status::Observed)
    })
}

fn a1b(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    if !run.observe(
        notes,
        "A1b: Open your everyday browser of the same kind now, and leave it open.",
        None,
    ) {
        return Ok(Status::NotRun);
    }
    let profile = run.profile("a1b")?;
    let mut browser = Browser::open(&run.executable, &profile, WindowShow::NoActivate)?;
    browser.settle(Duration::from_secs(3));
    let active = browser.tree.active_processes();
    let version = browser.version();
    notes.note(format!(
        "with the everyday browser running: processes in the new tree after 3 s: {active:?}; the pipe answered: {}",
        version.is_ok()
    ));
    let separate = run.observe(
        notes,
        "A1b: Did a second, separate window open (not a tab in your everyday browser)? (yes/no)",
        Some(&mut browser),
    );
    browser.shut(notes);
    version?;
    Ok(if separate && active != Some(0) {
        Status::Observed
    } else {
        Status::Failed
    })
}

fn a2(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    run.with_main(notes, |_, browser, notes| {
        let asked = Instant::now();
        let info = browser.version()?;
        notes.note(format!(
            "the version message was answered over the pipe in {} ms",
            milliseconds(asked.elapsed())
        ));
        notes.note(
            "switches: --remote-debugging-pipe and --remote-debugging-io-pipes=<read>,<write>; \
             the two handles as decimal numbers, inherited through an explicit handle list",
        );
        notes.note("framing: one JSON message followed by one zero byte, both ways");
        notes.note(format!(
            "protocol version {}; product {:?}; product version {}",
            info.protocol,
            info.product,
            info.product_version
                .map_or("(not parsed)".to_owned(), |version| version.to_string())
        ));
        Ok(Status::Observed)
    })
}

fn a2b(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let profile = run.profile("a2b")?;
    let plan = LaunchPlan::new(&run.executable, &profile)
        .map_err(|failure| format!("the launch plan was refused ({:?})", failure.stage))?
        .attaching(Attachment::LoopbackPort);
    let launched = launch::launch(&plan)
        .map_err(|failure| format!("the launch failed at {:?}", failure.stage))?;
    let outcome = (|| {
        // The browser writes its port, then the path to attach at.
        let port_file = profile.join("DevToolsActivePort");
        let until = Instant::now() + LAUNCH_REPLY;
        let (port, path) = loop {
            let written = std::fs::read_to_string(&port_file).unwrap_or_default();
            let mut lines = written.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next())
                && let Ok(port) = port.trim().parse::<u16>()
            {
                break (port, path.trim().to_owned());
            }
            if Instant::now() >= until {
                return Err("the browser wrote no port file".to_owned());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        notes.note("the port file was written");
        let stream = TcpStream::connect(("127.0.0.1", port))
            .map_err(|error| format!("no connection to the port ({:?})", error.kind()))?;
        let _ = stream.set_read_timeout(Some(ONE_COMMAND));
        let _ = stream.set_write_timeout(Some(ONE_COMMAND));
        let (mut socket, _) = tungstenite::client(format!("ws://127.0.0.1:{port}{path}"), stream)
            .map_err(|_| "the handshake failed".to_owned())?;
        let frame = String::from_utf8(Command::GetVersion.encode(CommandId(1), None))
            .map_err(|_| "the frame is not text".to_owned())?;
        notes
            .transcript
            .push(Command::GetVersion.record(CommandId(1), None));
        socket
            .send(tungstenite::Message::text(frame))
            .map_err(|_| "the version message could not be sent".to_owned())?;
        for _ in 0..50 {
            let message = socket.read().map_err(|_| "no reply".to_owned())?;
            let bytes = message.into_data();
            if let Ok(FrameHead::Reply { id, .. }) = protocol::classify(&bytes)
                && id == CommandId(1)
                && let Ok(Ok(reply)) = protocol::decode_reply(&bytes, CommandKind::GetVersion)
            {
                notes.transcript.push(reply.record(id));
                notes.note("connected over the loopback port; the version message was answered");
                return Ok(Status::Observed);
            }
        }
        Err("fifty messages and no version reply".to_owned())
    })();
    launched.stop.close();
    launched.stop.wait_closed(Duration::from_secs(10));
    outcome
}

fn a3(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    run.close_main(notes);
    let person = run.observe(
        notes,
        "A3: After Enter you have 5 seconds to click into another window (a text editor) and start typing. Keep typing until this program asks again.",
        None,
    );
    if person {
        std::thread::sleep(Duration::from_secs(5));
    }
    for (name, show) in [
        ("a3n", WindowShow::NoActivate),
        ("a3d", WindowShow::BrowserDefault),
    ] {
        let profile = run.profile(name)?;
        let mut browser = Browser::open(&run.executable, &profile, show)?;
        let at_launch = took_foreground(&mut browser, Duration::from_secs(5));
        let navigated = (|| {
            browser.version()?;
            browser.prepare(Policy::Off)?;
            let mut moved = Vec::new();
            for path in ["/", "/links", "/out"] {
                browser.navigate(run.a(path)?, ONE_NAVIGATION)?;
                moved.push(took_foreground(&mut browser, Duration::from_millis(1500)));
            }
            Ok::<_, String>(moved)
        })();
        notes.note(format!(
            "{show:?}: the foreground window became the browser's at launch: {at_launch:?}; after each of three navigations: {navigated:?}"
        ));
        browser.shut(notes);
    }
    let answered = person
        && run.observe(
            notes,
            "A3: You can stop typing. Was your typing interrupted, or did any keystroke land in a browser window? When: at a launch, during a navigation, never?",
            None,
        );
    Ok(if answered {
        Status::Observed
    } else {
        Status::NotRun
    })
}

fn a4(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let policy = run.only_site_a()?;
    run.with_main(notes, |run, browser, notes| {
        browser.version()?;
        browser.prepare(policy)?;
        notes.note("interception was acknowledged before the first navigation");
        let home = browser.navigate(run.a("/")?, ONE_NAVIGATION)?;
        notes.note(format!(
            "site A opened: start {:?}, loaded {}, {} ms",
            home.start,
            home.loaded,
            milliseconds(home.took)
        ));
        let mut leaked = Vec::new();
        for (name, path, follow, wait) in [
            ("a link to B", "/out", Some("/?from=link"), 3),
            ("a redirect to B", "/redirect-out", None, 3),
            ("a script navigation to B", "/script-out", None, 3),
            ("a reload to B after two seconds", "/refresh-out", None, 5),
            ("a form sent to B by script", "/form-out", None, 3),
            ("a tab opened to B by script", "/popup", None, 4),
        ] {
            let before = (
                run.site.requests(Site::B),
                browser.tally.refused,
                browser.tally.tabs_closed,
            );
            let first = browser.navigate(run.a(path)?, Duration::from_secs(8));
            let second = match follow {
                Some(target) => Some(browser.navigate(run.b(target)?, Duration::from_secs(5))),
                None => None,
            };
            browser.settle(Duration::from_secs(wait));
            let reached = run.site.requests(Site::B) - before.0;
            let start = |result: &Result<Navigation, String>| match result {
                Ok(navigation) => format!("{:?}", navigation.start),
                Err(problem) => format!("({problem})"),
            };
            notes.note(format!(
                "{name}: top-level requests paused and refused: {}; tabs opened by the page and closed: {}; site B received {reached} request(s); the page's navigation: {}{}; the window is now on {}",
                browser.tally.refused - before.1,
                browser.tally.tabs_closed - before.2,
                start(&first),
                second
                    .as_ref()
                    .map_or(String::new(), |result| format!(", then to B: {}", start(result))),
                browser.place()
            ));
            if reached > 0 {
                leaked.push(name);
            }
        }
        run.observe(
            notes,
            "A4: What does the window show for a page that was blocked? Describe it.",
            Some(&mut *browser),
        );
        // A tab the page opens is not attached to, so it is recorded
        // (assumption 24) and does not decide the step.
        let decided: Vec<_> = leaked
            .iter()
            .filter(|name| !name.starts_with("a tab"))
            .collect();
        if decided.is_empty() {
            Ok(Status::Observed)
        } else {
            Err(format!("a request reached site B for: {decided:?}"))
        }
    })
}

fn a4b(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let policy = run.only_site_a()?;
    run.with_main(notes, |run, browser, notes| {
        browser.version()?;
        browser.prepare(policy)?;
        let before = (run.site.requests(Site::B), browser.tally.continued);
        browser.navigate(run.a("/embeds")?, ONE_NAVIGATION)?;
        browser.settle(Duration::from_secs(3));
        let embedded = run.site.requests(Site::B) - before.0;
        notes.note(format!(
            "a page on A with an image and a frame from B: site B received {embedded} request(s) (2 means both loaded); document requests let through meanwhile: {}",
            browser.tally.continued - before.1
        ));
        let before = (run.site.requests(Site::B), browser.tally.refused);
        let top = browser.navigate(run.b("/?from=top")?, Duration::from_secs(5))?;
        browser.settle(Duration::from_secs(2));
        let reached = run.site.requests(Site::B) - before.0;
        notes.note(format!(
            "then B at top level: start {:?}; refused {}; site B received {reached} request(s)",
            top.start,
            browser.tally.refused - before.1
        ));
        if reached == 0 {
            Ok(Status::Observed)
        } else {
            Err("a top-level request reached site B".to_owned())
        }
    })
}

fn a5(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let policy = run.only_site_a()?;
    run.with_main(notes, |run, browser, notes| {
        browser.version()?;
        browser.prepare(policy)?;
        browser.navigate(run.a("/login")?, ONE_NAVIGATION)?;
        let question = format!(
            "A5: In the browser window: type any name, press Continue, enter the code {SIGN_IN_CODE}, press Sign in. Wait for the Issues page."
        );
        run.signed_in = run.observe(notes, &question, Some(&mut *browser));
        let list = browser.navigate(run.a("/issues")?, ONE_NAVIGATION)?;
        let loaded = Instant::now();
        notes.note(format!(
            "the issue list: start {:?}, loaded {}, the window is on {}",
            list.start,
            list.loaded,
            browser.place()
        ));
        let on_list = browser.path() == "/issues";
        let mut complete_at = None;
        for delay in [0u64, 250, 1000] {
            browser.wait(Duration::from_millis(delay).saturating_sub(loaded.elapsed()), |_| false);
            let at = loaded.elapsed();
            let reading = browser.read()?;
            let titles = ISSUES
                .iter()
                .filter(|(title, _)| reading.text.contains(*title))
                .count();
            let links = reading
                .links
                .iter()
                .filter(|(_, address)| address.contains("/issues/"))
                .count();
            notes.note(format!(
                "read {} ms after load: title {} bytes, text {} of {} bytes, {} link(s) (more: {}), {titles} of 3 issue titles, {links} issue link(s), marked text present: {}",
                milliseconds(at),
                reading.title.len(),
                reading.text.len(),
                reading.total,
                reading.links.len(),
                reading.more_links,
                reading.text.contains(MARK_TEXT)
            ));
            if titles == 3 && links == 3 && complete_at.is_none() {
                complete_at = Some(at);
            }
        }
        match complete_at {
            Some(at) => notes.note(format!(
                "the list was complete at the read {} ms after load",
                milliseconds(at)
            )),
            None if on_list => notes.note("the list was never read complete"),
            None => notes.note(
                "the issue list was not reached: it needs the sign-in, which needs a person",
            ),
        }
        // A field's value is not page text.
        browser.navigate(run.a("/form")?, ONE_NAVIGATION)?;
        let form = browser.read()?;
        notes.note(format!(
            "a page with a filled field: text {} bytes; the field's marked value is in the text: {}",
            form.text.len(),
            form.text.contains(MARK_FIELD)
        ));
        // Two megabytes of text come back bounded.
        browser.navigate(run.a("/big")?, ONE_NAVIGATION)?;
        let big = browser.read()?;
        notes.note(format!(
            "a page of {BIG_PAGE_BYTES} bytes: the script returned {} bytes and reported {} in all",
            big.text.len(),
            big.total
        ));
        if form.text.contains(MARK_FIELD) {
            return Err("the read returned a field's value".to_owned());
        }
        Ok(if complete_at.is_some() {
            Status::Observed
        } else if on_list {
            Status::Failed
        } else {
            Status::NotRun
        })
    })
}

fn counts(browser: &Browser) -> BTreeMap<String, u32> {
    let mut all: BTreeMap<String, u32> = browser
        .tally
        .listed
        .iter()
        .map(|(name, count)| ((*name).to_owned(), *count))
        .collect();
    for (name, count) in &browser.tally.unlisted {
        *all.entry(name.clone()).or_default() += count;
    }
    all
}

fn gained(before: &BTreeMap<String, u32>, after: &BTreeMap<String, u32>) -> String {
    let gained: Vec<String> = after
        .iter()
        .filter_map(|(name, count)| {
            let more = count - before.get(name).copied().unwrap_or(0);
            (more > 0).then(|| format!("{name} x{more}"))
        })
        .collect();
    if gained.is_empty() {
        "(none)".to_owned()
    } else {
        gained.join(", ")
    }
}

fn a6(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let policy = run.only_site_a()?;
    run.with_main(notes, |run, browser, notes| {
        browser.version()?;
        browser.prepare(policy)?;
        browser.navigate(run.a("/")?, ONE_NAVIGATION)?;
        notes.note("evidence only: no input listener and no binding is installed");
        let mut person = true;
        for action in [
            "A6: Click somewhere in the page, type a few keys, and scroll.",
            "A6: Type the fixture site's /links address into the address bar and press Enter.",
            "A6: Open a new tab in that window (Ctrl+T).",
        ] {
            let before = counts(browser);
            if run.ask(action, Some(&mut *browser)).is_none() {
                notes.note(format!("not run, needs a person: {action}"));
                person = false;
                continue;
            }
            browser.settle(Duration::from_millis(500));
            notes.note(format!(
                "{action} -> messages that arrived: {}",
                gained(&before, &counts(browser))
            ));
        }
        let before = counts(browser);
        browser.navigate(run.a("/forge")?, ONE_NAVIGATION)?;
        browser.settle(Duration::from_secs(1));
        let forged = browser.read()?;
        notes.note(format!(
            "a page that fires a synthetic key and click and calls a binding by name -> messages that arrived: {}; the page reports the binding existed: {}",
            gained(&before, &counts(browser)),
            forged.text.contains("binding called")
        ));
        Ok(if person { Status::Observed } else { Status::NotRun })
    })
}

/// After a close: start the main profile again and see where the issue
/// list lands.
fn relaunch_and_check(run: &mut Run, notes: &mut Notes, after: &str) -> Result<(), String> {
    let policy = run.only_site_a()?;
    let after = after.to_owned();
    run.with_main(notes, move |run, browser, notes| {
        let info = browser.version();
        notes.note(format!(
            "after {after}: the same profile started again and answered: {}",
            info.is_ok()
        ));
        info?;
        browser.prepare(policy)?;
        browser.navigate(run.a("/issues")?, ONE_NAVIGATION)?;
        let kept = browser.path() == "/issues";
        notes.note(format!(
            "after {after}: the issue list landed on {}; signed in before: {}; the fixture sign-in survived: {}",
            browser.place(),
            run.signed_in,
            if run.signed_in {
                kept.to_string()
            } else {
                "(not signed in, nothing to survive)".to_owned()
            }
        ));
        Ok(Status::Observed)
    })
    .map(|_| ())
}

fn a7(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let mut under_two_seconds = true;
    for (name, slow, by) in [
        ("an orderly close, idle", false, "ask"),
        ("an orderly close while a slow page loads", true, "ask"),
        ("ending the process tree", false, "end"),
        ("the stop handle's close", true, "stop"),
    ] {
        let policy = run.only_site_a()?;
        let mut browser = match run.main.take() {
            Some(browser) => browser,
            None => run.open_main()?,
        };
        let ready = (|| {
            browser.version()?;
            browser.prepare(policy)?;
            if slow {
                browser.send(
                    &Command::Navigate {
                        address: run.a("/slow")?,
                    },
                    true,
                )?;
                browser.settle(Duration::from_secs(1));
            }
            Ok::<_, String>(())
        })();
        if let Err(problem) = ready {
            browser.shut(notes);
            return Err(problem);
        }
        let asked = Instant::now();
        match by {
            "ask" => {
                let _ = browser.send(&Command::Close, false);
            }
            "end" => browser.tree.end(),
            _ => browser.stop.close(),
        }
        let gone = browser.tree.wait_empty(Duration::from_secs(10));
        let took = asked.elapsed();
        under_two_seconds &= gone && took < Duration::from_secs(2);
        notes.note(format!(
            "{name}: from the request to the last process exit: {} ms; every process gone: {gone}",
            milliseconds(took)
        ));
        browser.shut(notes);
        relaunch_and_check(run, notes, name)?;
    }
    if under_two_seconds {
        Ok(Status::Observed)
    } else {
        Err("a close took two seconds or more, or left a process".to_owned())
    }
}

fn a8(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    if run.unattended {
        notes.note(
            "not run, needs a person: closing the window by hand, and ending it from Task Manager",
        );
        return Ok(Status::NotRun);
    }
    for action in [
        "A8: Close the browser window by hand now (its X button). Do NOT press Enter: the program is watching.",
        "A8: Open Task Manager and end the browser that this program started (End task). Do NOT press Enter.",
    ] {
        let policy = run.only_site_a()?;
        let mut browser = match run.main.take() {
            Some(browser) => browser,
            None => run.open_main()?,
        };
        let ready = browser
            .version()
            .and_then(|_| browser.prepare(policy))
            .and_then(|()| Ok(browser.navigate(run.a("/")?, ONE_NAVIGATION)?));
        if let Err(problem) = ready {
            browser.shut(notes);
            return Err(problem);
        }
        println!("\n  >> {action}");
        let mark = browser.events.len();
        let watching = Instant::now();
        let mut tree_empty = None;
        let seen = browser.wait(Duration::from_secs(180), |browser| {
            browser.pipe_closed.is_some()
                || browser.events[mark..].iter().any(|(_, event)| {
                    matches!(
                        event,
                        Event::TargetDestroyed { .. }
                            | Event::TargetCrashed { .. }
                            | Event::InspectorCrashed
                    )
                })
        });
        if !seen {
            notes.note(format!("{action} -> nothing was seen in 180 s"));
            browser.shut(notes);
            return Err("neither a closed pipe nor a lost tab was seen".to_owned());
        }
        let first = Instant::now();
        // What else arrives, and how long after the first sign.
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            browser.step(Duration::from_millis(10));
            if tree_empty.is_none() && browser.tree.active_processes() == Some(0) {
                tree_empty = Some(Instant::now());
            }
            if tree_empty.is_some() && browser.pipe_closed.is_some() {
                break;
            }
        }
        let event = browser.events[mark..]
            .iter()
            .find(|(_, event)| {
                matches!(
                    event,
                    Event::TargetDestroyed { .. }
                        | Event::TargetCrashed { .. }
                        | Event::InspectorCrashed
                )
            })
            .map(|(at, event)| (event.name().name(), *at));
        let since = |at: Option<Instant>| match at {
            Some(at) => format!(
                "{} ms",
                at.saturating_duration_since(first.min(at)).as_millis()
            ),
            None => "not seen within 5 s".to_owned(),
        };
        notes.note(format!(
            "{action} -> first sign after {} s of watching; lost-tab message: {}; the pipe closed: {} after the first sign; no process left: {} after the first sign",
            watching.elapsed().as_secs(),
            event.map_or("none".to_owned(), |(name, _)| name.to_owned()),
            since(browser.pipe_closed),
            since(tree_empty)
        ));
        browser.shut(notes);
    }
    notes.note("the time from the hand's action to the first sign cannot be measured by the program; every later sign is measured from the first");
    Ok(Status::Observed)
}

fn a9(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let policy = run.only_site_a()?;
    let profile = run.profile("main")?;
    run.with_main(notes, |run, browser, notes| {
        browser.version()?;
        browser.prepare(policy)?;
        browser.navigate(run.a("/")?, ONE_NAVIGATION)?;
        let before = run.site.requests_for(Site::A, "/download");
        let download = browser.navigate(run.a("/download")?, Duration::from_secs(5))?;
        browser.settle(Duration::from_secs(2));
        let (_, _, in_profile) = survey(&profile, "fixture-download.bin");
        notes.note(format!(
            "the download link: start {:?}; site A received {} request(s) for it; a file of its name is in the profile folder: {in_profile}",
            download.start,
            run.site.requests_for(Site::A, "/download") - before
        ));
        let by_hand = run.observe(
            notes,
            "A9: Is there a file named fixture-download.bin in your Downloads folder, and did the window show a download? (the program does not look there)",
            Some(&mut *browser),
        );
        let dialogs = browser.tally.dialogs;
        let dialog = browser.navigate(run.a("/dialog")?, Duration::from_secs(8))?;
        notes.note(format!(
            "a page that opens a dialog: dialogs seen and dismissed: {}; the page then loaded: {}",
            browser.tally.dialogs - dialogs,
            dialog.loaded
        ));
        if in_profile {
            return Err("a download was saved".to_owned());
        }
        Ok(if by_hand { Status::Observed } else { Status::NotRun })
    })
}

fn a10(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let mut browsers = Vec::new();
    let mut problem = None;
    for name in ["a10a", "a10b"] {
        let opened = run.profile(name).and_then(|profile| {
            let mut browser = Browser::open(&run.executable, &profile, WindowShow::NoActivate)?;
            let ready = (|| {
                browser.version()?;
                browser.prepare(Policy::Off)?;
                browser.navigate(run.a("/")?, ONE_NAVIGATION)?;
                Ok::<_, String>(())
            })();
            browsers.push((name, profile, browser));
            ready
        });
        if let Err(failed) = opened {
            problem = Some(failed);
            break;
        }
    }
    if problem.is_none() {
        for (_, _, browser) in &mut browsers {
            browser.settle(Duration::from_secs(3));
        }
        for (name, profile, browser) in &browsers {
            let (bytes, deepest, _) = survey(profile, "");
            notes.note(format!(
                "{name}: {:?} process(es); most memory committed by the tree at once: {}; fresh profile on disk: {bytes} bytes; its deepest path: {deepest} characters (the profile folder itself: {})",
                browser.tree.active_processes(),
                browser
                    .tree
                    .peak_memory_bytes()
                    .map_or("(the system kept no number)".to_owned(), |peak| format!("{peak} bytes")),
                profile.as_os_str().len()
            ));
        }
        notes.note(format!(
            "two browsers ran at once on two profiles: {}",
            browsers
                .iter()
                .all(|(_, _, browser)| browser.pipe_closed.is_none())
        ));
    }
    for (_, _, browser) in browsers {
        browser.shut(notes);
    }
    match problem {
        Some(problem) => Err(problem),
        None => Ok(Status::Observed),
    }
}

fn a11(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let policy = run.only_site_a()?;
    run.with_main(notes, |run, browser, notes| {
        browser.version()?;
        browser.prepare(policy)?;
        browser.navigate(run.a("/")?, ONE_NAVIGATION)?;
        let person = run.observe(
            notes,
            "A11: After Enter you have 5 seconds to click on another application's window.",
            Some(&mut *browser),
        );
        if person {
            browser.settle(Duration::from_secs(5));
        }
        let before = foreground_process().map(|process| browser.tree.process_ids().contains(&process));
        let asked = browser.call(Command::BringToFront, true, ONE_COMMAND);
        let after = took_foreground(browser, Duration::from_secs(2));
        notes.note(format!(
            "the browser had the foreground before: {before:?}; bring-to-front was answered: {}; it had the foreground within 2 s after: {after:?}",
            asked.is_ok()
        ));
        asked?;
        let seen = person
            && run.observe(
                notes,
                "A11: Did the browser window come to the front? What was needed (nothing, a click on a flashing taskbar button)?",
                Some(&mut *browser),
            );
        Ok(if seen { Status::Observed } else { Status::NotRun })
    })
}

fn a12(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let profile = run.profile("main")?;
    let listed = std::process::Command::new("icacls")
        .arg(&profile)
        .output()
        .map_err(|error| {
            format!(
                "the access entries could not be listed ({:?})",
                error.kind()
            )
        })?;
    notes.note("the profile folder's access entries, as the system lists them:");
    for line in String::from_utf8_lossy(&listed.stdout).lines() {
        let line = line.trim();
        if !line.is_empty() {
            notes.note(format!("  {line}"));
        }
    }
    // A file the browser wrote, read by another process of this user: its
    // state file at the top of the profile if there is one (it is not held
    // locked), otherwise the first file found.
    let state = profile.join("Local State");
    let mut file = state.is_file().then_some(state);
    let mut left = vec![profile.clone()];
    while file.is_none() {
        let Some(next) = left.pop() else { break };
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            match entry.metadata() {
                Ok(data) if data.is_dir() => left.push(entry.path()),
                Ok(data) if data.len() > 0 => file = Some(entry.path()),
                _ => {}
            }
        }
    }
    let Some(file) = file else {
        return Err("the profile folder holds no file to read".to_owned());
    };
    let read = std::process::Command::new("cmd")
        .arg("/c")
        .arg("type")
        .arg(&file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("no other process could be started ({:?})", error.kind()))?;
    notes.note(format!(
        "another process of the same user read a file in the profile folder: {}",
        read.success()
    ));
    Ok(Status::Observed)
}

// ------------------------------------------------------------------ part B

fn journey(run: &mut Run) -> Result<Browser, String> {
    let mut browser = match run.main.take() {
        Some(browser) => browser,
        None => {
            let profile = run.profile("journey")?;
            Browser::open(&run.executable, &profile, WindowShow::NoActivate)?
        }
    };
    browser.version()?;
    browser.prepare(Policy::Any)?;
    Ok(browser)
}

fn journey_address(run: &Run) -> Result<PageAddress, String> {
    let text = run
        .address
        .as_deref()
        .ok_or("part b needs --address <the issue list's address>")?;
    StartingAddress::parse(text)
        .map(|address| PageAddress::from(&address))
        .map_err(|error| format!("the address was refused ({error:?})"))
}

fn expected(run: &Run, reading: &Reading) -> String {
    if run.expect.is_empty() {
        return "no --expect text was given".to_owned();
    }
    let held = run
        .expect
        .iter()
        .filter(|text| reading.text.contains(text.as_str()))
        .count();
    format!("{held} of {} expected texts are in it", run.expect.len())
}

fn b1(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let address = journey_address(run)?;
    let mut browser = journey(run)?;
    let opened = browser.navigate(address, ONE_NAVIGATION);
    notes.note(format!(
        "the issue list's address was opened; the window is on {}",
        browser.place()
    ));
    let person = run.observe(
        notes,
        "B1: Sign in to the journey site in the browser window, with two-factor. Note any challenge, warning or refusal. Did the sign-in complete?",
        Some(&mut browser),
    );
    browser.hand_over(notes);
    run.main = Some(browser);
    opened?;
    Ok(if person {
        Status::Observed
    } else {
        Status::NotRun
    })
}

fn b2(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let address = journey_address(run)?;
    let mut browser = journey(run)?;
    let outcome = (|| {
        let list = browser.navigate(address, ONE_NAVIGATION)?;
        let reading = browser.read()?;
        notes.note(format!(
            "the issue list: loaded {}, on {}; title {} bytes, text {} of {} bytes, {} link(s); {}",
            list.loaded,
            browser.place(),
            reading.title.len(),
            reading.text.len(),
            reading.total,
            reading.links.len(),
            expected(run, &reading)
        ));
        let Some(number) = run.ask(
            "B2: Type the number of one seeded issue (as in its address).",
            Some(&mut browser),
        ) else {
            notes.note("not run, needs a person: which issue to follow");
            return Ok(Status::NotRun);
        };
        let ending = format!("/{}", number.trim());
        let link = reading
            .links
            .iter()
            .position(|(_, address)| {
                PageAddress::reported(address).is_some_and(|link| link.path().ends_with(&ending))
            })
            .ok_or("no link of the reading leads to that issue")?;
        let target = PageAddress::reported(&reading.links[link].1).ok_or("not an address")?;
        notes.note(format!("following link number {link} of the reading"));
        let issue = browser.navigate(target, ONE_NAVIGATION)?;
        let reading = browser.read()?;
        notes.note(format!(
            "the issue: loaded {}, on {}; text {} of {} bytes; {}",
            issue.loaded,
            browser.place(),
            reading.text.len(),
            reading.total,
            expected(run, &reading)
        ));
        Ok(Status::Observed)
    })();
    browser.hand_over(notes);
    run.main = Some(browser);
    outcome
}

fn b3(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let browser = run.main.as_ref().ok_or("no browser from B1 and B2")?;
    notes.note("every site a top-level document was requested from, in B1 and B2:");
    for site in &browser.tally.top_sites {
        notes.note(format!("  {site}"));
    }
    Ok(Status::Observed)
}

fn b4(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let address = journey_address(run)?;
    if let Some(mut browser) = run.main.take() {
        let asked = Instant::now();
        let _ = browser.send(&Command::Close, false);
        let gone = browser.tree.wait_empty(Duration::from_secs(10));
        notes.note(format!(
            "an orderly close: {} ms, every process gone: {gone}",
            milliseconds(asked.elapsed())
        ));
        browser.shut(notes);
    }
    let mut browser = journey(run)?;
    let opened = browser.navigate(address.clone(), ONE_NAVIGATION);
    notes.note(format!(
        "after the relaunch the issue list landed on {}; that is the list's own path: {}",
        browser.place(),
        browser.path() == address.path()
    ));
    let person = run.observe(
        notes,
        "B4: Is the window still signed in? Did 'remember this device' matter?",
        Some(&mut browser),
    );
    browser.hand_over(notes);
    run.main = Some(browser);
    opened?;
    Ok(if person {
        Status::Observed
    } else {
        Status::NotRun
    })
}

fn b5(run: &mut Run, notes: &mut Notes) -> Result<Status, String> {
    let address = journey_address(run)?;
    let mut browser = journey(run)?;
    let person = run.observe(
        notes,
        "B5: Sign out of the journey site from ANOTHER browser (end all sessions).",
        Some(&mut browser),
    );
    let outcome = (|| {
        if !person {
            return Ok(Status::NotRun);
        }
        let reload = browser.navigate(address, ONE_NAVIGATION)?;
        let reading = browser.read()?;
        notes.note(format!(
            "after the sign-out, the issue list's address: loaded {}, landed on {}; title {} bytes, text {} bytes, {} link(s); {}",
            reload.loaded,
            browser.place(),
            reading.title.len(),
            reading.text.len(),
            reading.links.len(),
            expected(run, &reading)
        ));
        run.observe(
            notes,
            "B5: What does the window show now?",
            Some(&mut browser),
        );
        Ok(Status::Observed)
    })();
    browser.hand_over(notes);
    run.main = Some(browser);
    outcome
}

// ---------------------------------------------------------------- evidence

fn evidence(run: &Run, part: &str, started: u64) -> String {
    let mut text = String::new();
    text.push_str("# M7 P7.1 qualification\n\n");
    text.push_str(
        "Written by `examples/qualify.rs`. **A skeleton**: each step records what the program \
         measured and what the person typed. The person completes it, and corrects any \
         judgement the program could not make.\n\n",
    );
    text.push_str(&format!("- Part: {part}\n"));
    text.push_str(&format!("- Started, in seconds since 1970: {started}\n"));
    text.push_str(&format!("- Attended by a person: {}\n", !run.unattended));
    if part == "a" || part == "c" {
        text.push_str(&format!(
            "- Fixture site A: {} ; site B: {}\n",
            run.site.address(Site::A),
            run.site.address(Site::B)
        ));
        text.push_str(&format!(
            "- Requests received in all: site A {}, site B {}\n",
            run.site.requests(Site::A),
            run.site.requests(Site::B)
        ));
    }
    text.push_str(
        "- Transcripts: `transcripts/<step>.txt`, one per step, addresses reduced to their site\n\n",
    );
    text.push_str("| Step | Result | Confirms |\n| --- | --- | --- |\n");
    for record in &run.records {
        text.push_str(&format!(
            "| {} | {} | {} |\n",
            record.id,
            record.status.word(),
            record.confirms
        ));
    }
    for record in &run.records {
        text.push_str(&format!(
            "\n## {} — {}\n\nConfirms: {}\n\n",
            record.id,
            record.status.word(),
            record.confirms
        ));
        for note in &record.notes {
            text.push_str(&format!("- {}\n", note.replace('\n', " ")));
        }
    }
    text.push_str(
        "\n## To be completed by the person\n\n- Who ran this, on which computer, and whether \
         inside Windows Sandbox.\n- For every step marked \"not run\": why.\n- Anything seen \
         that no step asked about.\n",
    );
    text
}

// -------------------------------------------------------------------- main

const USAGE: &str = "usage: qualify <browser executable> <output folder> [--part a|b|c] \
     [--scratch <folder>] [--unattended] [--address <address>] [--expect <text>]...";

fn main() -> ExitCode {
    let mut positional = Vec::new();
    let mut part = "a".to_owned();
    let mut scratch = None;
    let mut unattended = false;
    let mut address = None;
    let mut expect = Vec::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| {
            arguments.next().unwrap_or_else(|| {
                eprintln!("{name} needs a value\n{USAGE}");
                std::process::exit(2)
            })
        };
        match argument.as_str() {
            "--part" => part = value("--part"),
            "--scratch" => scratch = Some(PathBuf::from(value("--scratch"))),
            "--address" => address = Some(value("--address")),
            "--expect" => expect.push(value("--expect")),
            "--unattended" => unattended = true,
            other if other.starts_with("--") => {
                eprintln!("{other} is not an option\n{USAGE}");
                return ExitCode::from(2);
            }
            _ => positional.push(argument),
        }
    }
    let [executable, out] = positional.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if !matches!(part.as_str(), "a" | "b" | "c") {
        eprintln!("--part is a, b or c\n{USAGE}");
        return ExitCode::from(2);
    }
    // Full paths: a launch takes no other kind.
    let full = |path: &String| std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    let executable = full(executable);
    let out = full(out);
    let scratch = scratch.map_or_else(
        || out.join("profiles"),
        |folder| std::path::absolute(&folder).unwrap_or(folder),
    );
    if let Err(error) =
        std::fs::create_dir_all(&out).and_then(|()| std::fs::create_dir_all(&scratch))
    {
        eprintln!("the output or scratch folder could not be made: {error}");
        return ExitCode::from(2);
    }

    // The fixture site on two loopback ports. Two host names, so the two
    // sites share no cookie.
    let stopping = Arc::new(AtomicBool::new(false));
    let listeners = TcpListener::bind(("127.0.0.1", 0)).and_then(|a| {
        let b = TcpListener::bind(("127.0.0.1", 0))?;
        Ok((a.local_addr()?.port(), a, b.local_addr()?.port(), b))
    });
    let (port_a, listener_a, port_b, listener_b) = match listeners {
        Ok(listeners) => listeners,
        Err(error) => {
            eprintln!("the fixture site could not listen: {error}");
            return ExitCode::from(2);
        }
    };
    let site = Arc::new(FixtureSite::new(
        &format!("http://127.0.0.1:{port_a}"),
        &format!("http://localhost:{port_b}"),
    ));
    serve(site.clone(), Site::A, listener_a, stopping.clone());
    serve(site.clone(), Site::B, listener_b, stopping.clone());

    let (typist, typed) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if typist.send(line).is_err() {
                break;
            }
        }
    });

    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    let mut run = Run {
        executable,
        out,
        scratch,
        unattended,
        address,
        expect,
        site,
        typed,
        main: None,
        signed_in: false,
        records: Vec::new(),
    };
    println!("M7 P7.1 qualification, part {part}");
    println!(
        "fixture site A {} , site B {}",
        run.site.address(Site::A),
        run.site.address(Site::B)
    );

    let steps_a: [(
        &'static str,
        &'static str,
        fn(&mut Run, &mut Notes) -> Result<Status, String>,
    ); 14] = [
        ("A1", "1, 4, 19, 26", a1),
        ("A1b", "22", a1b),
        ("A2", "2, 17", a2),
        ("A3", "5", a3),
        ("A4", "9, 24", a4),
        ("A4b", "23", a4b),
        ("A5", "10", a5),
        ("A6", "11", a6),
        ("A7", "8, 12, 16", a7),
        ("A8", "13", a8),
        ("A9", "14", a9),
        ("A10", "15, 25", a10),
        ("A11", "5", a11),
        ("A12", "18", a12),
    ];
    let instrument = run.step("A0", "the instrument itself", a0);
    if instrument != Status::Observed {
        println!("\nA0 did not pass: nothing else is run.");
        if part == "a" {
            for (id, confirms, _) in steps_a {
                run.skip(
                    id,
                    confirms,
                    "A0 failed: the program cannot start the browser here",
                );
            }
        }
    } else if part == "a" {
        let mut pipe = true;
        for (id, confirms, body) in steps_a {
            if !pipe {
                run.skip(
                    id,
                    confirms,
                    "the pipe failed (A2); this program drives the pipe only, and the fallback \
                     transport is written only after this result",
                );
                continue;
            }
            let status = run.step(id, confirms, body);
            if id != "A2" {
                continue;
            }
            if status == Status::Failed {
                let mut notes = Notes::default();
                run.close_main(&mut notes);
                pipe = false;
                run.step("A2b", "21", a2b);
            } else {
                run.skip("A2b", "21", "A2 passed: the fallback is not needed");
            }
        }
    } else if part == "c" {
        run.step("A1", "1, 4, 19, 26 (C1: 3)", a1);
        run.step("A2", "2, 17 (C1: 3)", a2);
    } else {
        run.step("B1", "6", b1);
        run.step("B2", "10", b2);
        run.step("B3", "D7.7", b3);
        run.step("B4", "8", b4);
        run.step("B5", "section 11", b5);
    }
    let mut last = Notes::default();
    run.close_main(&mut last);
    stopping.store(true, Ordering::SeqCst);

    let file = run.out.join("M7.P7.1.QUALIFICATION.md");
    match std::fs::write(&file, evidence(&run, &part, started)) {
        Ok(()) => println!("\nwritten: {}", file.display()),
        Err(error) => {
            eprintln!("the evidence file could not be written: {error}");
            return ExitCode::from(1);
        }
    }
    let failed = run
        .records
        .iter()
        .filter(|record| record.status == Status::Failed)
        .count();
    println!("{} step(s), {failed} failed", run.records.len());
    ExitCode::from(u8::from(failed > 0))
}
