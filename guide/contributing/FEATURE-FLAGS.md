# Product feature flags

**What a build shows.** Decided by the maintainers on 2026-10-08: the next release of
Koru shows documents, files and terminal/shell session tabs — plus Welcome,
Settings, Manage, Trash, Feedback, Search, Updates, What's New, Linked devices
and Voice. Eight larger surfaces are built and tested behind flags — the
first five from 2026-10-08, canvases and Socials from 2026-10-10, agent views
from 2026-10-16. **Cloud sync, agent sessions, Tasks and Koru AI ship on by
default**. Cloud sync defaults on in every build (maintainers, 2026-11-07).
Explore's Agents and Artifacts views, reminders, canvases and Socials are
**hidden** until switched on. Nothing was deleted, and the flags are **UI only**:
every background worker (reminders, sync, the delegate runner, the session
host) starts exactly as it does with the flag on. **The one exception is the
Socials publisher**, which Koru Agent starts only while `ENABLE_SOCIALS` is on
(maintainers, 2026-10-10): nothing publishes from a build that hides Socials. The
decision record is ADR-450.

| Flag (Rust) | Env var | Default | Shows |
| --- | --- | --- | --- |
| `ENABLE_CLOUD_SYNC` | `KORU_ENABLE_CLOUD_SYNC` | **on** | Koru Cloud: account sign-in (email, Google, Apple), the Cloud page, every Cloud pointer |
| `ENABLE_AGENTS` | `KORU_ENABLE_AGENTS` | **on** | Agent sessions as tabs: the `+` menu's *Agents ▸*, agent tabs and their restore, the tray's Agents submenu. **Shell sessions are always shown** |
| `ENABLE_AGENT_VIEWS` | `KORU_ENABLE_AGENT_VIEWS` | off | Explore's *Agents* and *Artifacts* views and every way into them — **and every way to start Delegate** (with `ENABLE_AI` on). Needs `ENABLE_AGENTS` on as well |
| `ENABLE_TASKS` | `KORU_ENABLE_TASKS` | **on** | The Tasks view and every way into it. **`/due` stays** and still creates a task |
| `ENABLE_AI` | `KORU_ENABLE_AI` | **on** | Koru AI chat **and Delegate** (runs, results, schedules) — grouped by the maintainers. *Starting* a delegation also needs `ENABLE_AGENT_VIEWS` |
| `ENABLE_REMINDERS` | `KORU_ENABLE_REMINDERS` | off | The reminders UI. The reminder worker still runs, so an existing reminder can still notify |
| `ENABLE_CANVAS` | `KORU_ENABLE_CANVAS` | off | Canvases: the Canvases view, its `+` flyout, canvas tabs, canvas search results and canvases in the Manage tab's document counts. With agents off as well, a shown canvas is cards and images only |
| `ENABLE_SOCIALS` | `KORU_ENABLE_SOCIALS` | off | Socials: the Socials view, its `+` row, *New social post* and the Socials tab — **and the publisher in Koru Agent**, which starts only with the flag on; off, a scheduled post waits |

## How the answer is reached

All of it is in one leaf crate, [`crates/koru-features`](../../crates/koru-features/src/lib.rs):
`koru_features::features()` (a `&'static Features` latched in a `OnceLock`) and
one accessor per flag — `cloud_sync()`, `agents()`, `agent_views()`,
`tasks()`, `ai()`, `reminders()`, `canvas()`, `socials()`. Two answer for
more than one flag: `agent_views()` is `agents && agent_views`, and
`delegate()` — whether Delegate can be *started* — is `ai() && agent_views()`.

1. **Baked in.** Each `ENABLE_*` constant is `option_env!("KORU_ENABLE_*")`
   read through koru-env's truthiness rule when `koru-features` is compiled.
   Cloud sync, agents, Tasks and Koru AI are **on** when unset; the other four
   flags are **off**. This applies to development and release builds on every
   platform.
2. **Overridden at run time.** A `KORU_ENABLE_*` variable that is **set** in the
   process environment wins in both directions: `KORU_ENABLE_AI=1` shows Koru
   AI in a build that baked it off; `KORU_ENABLE_AI=0` hides it in one that
   baked it on. On is anything except `""`, `0`, `false`, `no`, `off`.
3. **Latched.** The answer is read once per process; a surface never appears or
   vanishes mid-session. Koru and Koru Agent each read their own environment.

**Every read is in `koru-features`.** `option_env!` is evaluated when the crate
that spells it is compiled — the trap `koru-release::package::run_cargo`
documents for `KORU_RELEASE_CHANNEL` — so a second crate reading the same name
could be compiled under a different environment and disagree.

Cloud sync's default exposes its account and sync controls. Connecting still
requires configured Cloud endpoints, a signed-in account with the required
subscription, and the Profile's sync settings. The flag does not change those
settings or automatically enable sync for every Profile or workspace.

## Baking a surface into a release

`koru-release` forwards each of the eight variables that is set to its
`cargo build` invocations, normalised by koru-env's rule (on becomes `1`,
off becomes `0`); an unset one is removed and takes the default above. Set
them in the packager's environment or in `.env.production`, which the release
tool loads before building:

```
KORU_ENABLE_TASKS=1
KORU_ENABLE_REMINDERS=1
KORU_ENABLE_AGENTS=0
```

The last line ships a build without agents. Forwarding the `0` matters: a
removed variable would leave agents at their default, on.

For a dev run, set the variable in the shell — `set KORU_ENABLE_AI=1` before
`koru.bat run` on Windows, `KORU_ENABLE_AI=1 ./koru.sh run` on a Mac — or put
the line in `.env.production` / `.env.local`, which both launchers load.
**It does rebuild:** the flags are read with `option_env!`, so cargo recompiles
everything that links `koru-features` (most of the UI) when one flips; keep the
same flags in both env files to avoid paying that on every switch.
`koru.bat envs` / `./koru.sh envs` list all eight.

An **installed** build reads the run-time override from its own environment,
and an app started from the Start menu, Finder or a login item has no shell's.
On a Mac, `launchctl setenv KORU_ENABLE_AI 1` before Koru and the Agent start
is the way to turn one on without rebuilding.

## What each flag hides

"Hidden" means: absent from menus (not dimmed), its chords do not resolve and
are not printed in any hint, the accessibility tree does not expose it, the F12
inspector does not see it (it is not drawn), and session restore drops it.

### `ENABLE_CLOUD_SYNC` off

- **First run** (`koru-workspace-host::onboarding`): no email/password, Google,
  Apple or *or*. It opens on the local Profile form — where *Use Koru without
  an account* leads — with no *Back*; every later step is unchanged. The Manage
  tab's *New profile* page embeds the same form and its lead drops the account
  half.
- **Manage** (`apps/koru/src/manage/`): no *Koru Cloud* nav row or chip, no
  *Other Profiles*, no *{n} Profiles hidden*; a Profile's page has no
  *Sign-in and sync* section and no *Can't reach Koru — see Koru Cloud*
  pointer; the account switcher does not open; the page's footer offers
  *Delete profile and its workspaces…* and *Remove from this device…* but no
  *Delete from Koru…*. The Cloud page is unreachable: `ManageView::show`
  turns a request for it into the first Profile's page (or *Linked devices*
  when there is none) — which is also where *Manage profiles…* lands before the
  window knows its Profile.
- **Explore**'s empty state: no *Subscribe to Koru Cloud* button, and its
  sentence drops the Cloud clause.
- **Context column**: no *Cloud sync* section; `Ctrl+Alt+K` (*Copy Cloud
  details*) does not resolve.
- *Linked devices* (peer-to-peer) is unaffected.

### `ENABLE_AGENTS` off

- **Explore**: the *Agents* and *Artifacts* views are hidden as well, whatever
  `ENABLE_AGENT_VIEWS` says: they are views of agent sessions.
- **`+` menu**: no *Agents ▸*; the *Shell ▸* flyout and shell session tabs are
  unchanged. `open_session_tab_for_profile` refuses an agent profile from any
  menu.
- **Session restore** drops a tab over a built-in **agent** profile (the
  session itself is untouched); shell and custom-profile tabs come back.
- **Koru Agent's tray**: no *Agents* submenu.
- Welcome's *Terminal agent* and *MCP* rows stay as they were — drawn and
  inert.

### `ENABLE_AGENT_VIEWS` off

The default, also when agents are on (maintainers, 2026-10-16): agent tabs
ship, the views over their runs and results do not.

- **Explore**: no *Agents* or *Artifacts* mode icons. The icons close up; the
  `Alt+digit` chords **skip rather than renumber** — `Alt+5` and `Alt+6` do
  nothing, `Alt+1`/`Alt+2` are unchanged — so a digit never means a different
  view in a different build. A restored or requested hidden mode opens
  Documents.
- **Ways in**: revealing a run or a schedule in the Agents view does nothing,
  the Agents tree's *Delete project* row is not offered, and a result opens
  from Delegate only (with `ENABLE_AI`), not from the Artifacts column.
- **Delegate cannot be started**, with Koru AI on or off — see the next
  section. Runs already made still show.
- The `+` menu's *Agents ▸*, agent tabs and the tray are `ENABLE_AGENTS` and
  unaffected.

### `ENABLE_AGENT_VIEWS` off with `ENABLE_AI` on (starting Delegate)

The default. Starting a delegation is hidden with the agent views (the user,
2026-11-03), so it follows `KORU_ENABLE_AGENT_VIEWS` with agents and Koru AI
on: `koru_features::delegate()` is `ai() && agent_views()`. Koru AI chat is
unaffected.

- No `/delegate` (`koru-editor` `command_menu::product_shows`), no `+`
  *Delegate*, no document-selection *Delegate…* (`apps/koru-document`
  `context_menu`), no Tasks-view *Delegate…* (`MenuAction::TaskDelegate`).
- No *Re-run* on a finished run's Overview, and no note about it: the host
  answers `koru_runs_ui::RunsSurface::set_can_re_run`.
- The modal's `Ctrl+Enter` (*Delegate the work*) is not advertised.
- `begin_delegation` — the one door into the modal — refuses every origin.
- **Runs already made show as they do today**: their pills open the Run tab,
  results open, and the run and schedule menus (*Delete run*, a schedule's
  *Rename*, *Edit…*, *Pause*, *Resume*, *Delete*) are unchanged — those stay
  on `ENABLE_AI` alone. Background workers, as with every flag, still run.

### `ENABLE_TASKS` off

- **Explore**: no *Tasks* mode (and so no rail); `Alt+3` does not resolve.
- **Tabs**: `open_tasks_tab` — the one door — refuses every queue and list;
  session restore drops `tasks:*` tabs.
- **`/` menu** (`koru-editor` `command_menu::product_shows`): no `/task`,
  `/task_list`, `/completed`, `/priority`. **`/due` stays and creates a task.**
  `/checklist` is a document block and stays.
- **The due chip**: pressing it opens its date picker (as the other chips do)
  instead of the Tasks view; `reveal_task_in_tasks_view` is a no-op. Its `×`
  (*Remove from Tasks*, behind its confirm) stays.
- **Document right-click menu** (`apps/koru-document` `context_menu`): no
  *View in Tasks*, *Complete*/*Reopen*, *Priority…*, *Remove from Tasks*; a due
  chip's menu has no *Go to task*, *Complete* or *Remove from Tasks*. The menu
  is index-mapped, so rows are filtered by **identity** — `kept` decides from
  each row's `Choice` which full rows survive, and both the drawn list and
  `choice_at` go through it.
- **Explore node menu** *Attributes ▸*: Due stays, Priority goes.
- **Context column**: no *Tasks* section. **Search**: no `koru.task` or
  `koru.task_state` filter. **Welcome**: no *Task list* row.

### `ENABLE_AI` off (Koru AI chat and Delegate)

- No AI side pane, no ✦ title-row button, `Ctrl+Shift+J` does not resolve, and
  a restored session never reopens the pane.
- No AI chat tab: no *New AI chat* or *Chats ▸* in `+`, no Explore *Chats*
  mode (`Alt+4`); the pane surface is not booted when `+` opens.
- A chat link opens nothing — from inside a document, from the search
  overlay, and from another app (LaunchServices on a Mac, `koru-agent
  koru://chat/…` on Windows), because the Agent itself answers `OpenChatLink`
  with a silent `Ok` and pushes no window a chat when Koru AI is hidden.
- Settings › System has no Koru AI providers card; Welcome has no Koru AI row.
- **Delegate**: no `/delegate`, no `+` *Delegate*, no document-menu *Delegate*,
  no tasks-view *Delegate…*; `begin_delegation` — the one door into the modal —
  refuses. No Run tab and no Result tab can open (a Result tab still can with
  agents on, for the Artifacts column). With `ENABLE_AI` on, starting Delegate
  still needs `ENABLE_AGENT_VIEWS` (above).
- **Run and schedule pills already inside a document still draw** — they are
  document content — but pressing or right-clicking one opens nothing.

### `ENABLE_REMINDERS` off

- No `/remind`, no caption bell (the tab strip takes its room back), no
  *All Reminders* tab, `Ctrl+Shift+R` does not resolve, `F6` has no bell to
  land on.
- Settings has no *Reminders* page; the context column has no *Reminders*
  section; the Tasks rail (with tasks on) has no *Reminders* row; the document
  menu has no *Add reminder*; Welcome has no *Reminder* row; Koru Agent's tray
  has no *All Reminders*.
- Reminder pills are not drawn in documents and nothing about reminders is in
  the accessibility tree.
- **The reminder worker keeps running**: a reminder set while the UI was on can
  still notify.

### `ENABLE_CANVAS` off

- **Explore**: no *Canvases* mode icon; `Alt+7` (`⌃⌘7` on a Mac) does not
  resolve and is not printed in any hint — it skips, so `Alt+8` is still
  Socials. A restored `canvases` mode opens Documents.
- **`+` menu**: no *Canvases ▸* flyout, so no *New canvas* and no recent
  canvases (`MenuAction::Canvases` / `NewCanvas`); the Documents flyout never
  listed canvases. `new_canvas` — the `+` row's and the empty section's
  button's one door — refuses.
- **Canvas tabs**: no door builds one (`apps/koru/src/mainview/canvas.rs`
  `canvas_hidden`). A link, a background open or a landing
  on a canvas opens nothing rather than a page over the canvas's objects. A
  tab opened before the listing named the kind — a cross-workspace landing —
  stays the page it was built as and is not converted.
- **Session restore** drops canvas tabs and pinned canvases
  (`restored_document_view` answers `None`); the documents are untouched and
  come back as canvases in a build that shows them.
- No canvas is drawn, so the accessibility tree and the F12 inspector have
  none. Welcome, the tray and the jump list never had a canvas entry.
- **Search and counts** (maintainers, 2026-10-10; QUESTIONS #7): a canvas is not a
  global search result (`koru-search-ui`'s `drop_hidden_kinds`, on every page
  as it lands), and the Manage tab's per-workspace document count counts pages
  only (`koru-workspace-host`'s `document_count`).

### `ENABLE_AGENTS` off with `ENABLE_CANVAS` on

A canvas's start, agent and output nodes are the agents' surface, so with
agents hidden a canvas is **cards and images**
(QUESTIONS #2). `CanvasView` turns
`koru_canvas_ui::CanvasSurface::set_workflows(false)`:

- The canvas menu has no *Add start / agent / output node*.
- A node has no *Run*, *Stop*, *Respond…* or *Choose agent…* — in its menu, on
  its face, or in the accessibility tree; an agent node shows its agent's name
  as a plain label.
- `canvas_view::perform` refuses those requests again whatever asked, so no
  chain or agent run starts and no session is handed over.
- A workflow node already on the canvas still **draws** — it is document
  content, like a run pill — and can be edited, moved, connected, deleted, and
  its result placed on the canvas.

### `ENABLE_SOCIALS` off

- **Explore**: no *Socials* (megaphone) mode icon; `Alt+8` (`⌃⌘8`) does not
  resolve and is not advertised. A restored `socials` mode opens Documents.
- **`+` menu**: no *Socials* row. **Tab and Explore node menus**: no *New social
  post* (`MenuAction::Socials` / `NewSocialPost`).
- **The Socials tab**: `open_socials_tab` — the one door, which the `+` row,
  the mode, its chord and *New social post* all reach — refuses, and so does
  `social_post_from_document`. Socials tabs were never part of the session
  file, so there is nothing to drop on restore.
- The connect-account (X, LinkedIn, Reddit) and OAuth surfaces live inside the
  Socials surface and are unreachable with it. There is no Settings, Welcome,
  tray or jump-list entry.
- **The publisher does not start** in Koru Agent
  (`koru_socials_publisher::Worker`, gated by `should_start_socials_publisher`
  in `apps/koru-agent/src/run.rs` — the one worker a flag gates, ADR-450's
  2026-10-10 amendment; QUESTIONS #6). Nothing
  publishes from a build that hides Socials. A post already scheduled on this
  device waits in the publisher's journal and goes out when a build that shows
  Socials runs again.

## Hidden for now: constants, not flags

A part of a surface that is shown can be hidden *for now* by a constant in
`koru-features`, beside the flags. It is not baked by a release nor turned on
by a variable: it is one line to flip back to `true`, and every way into the
part comes back with it. The code behind it stays built.

| Constant | Hides | Since |
| --- | --- | --- |
| `HOME_SHOWN` | Home: the house at the top of the left rail, its nav (Inbox, Today, Upcoming, Recent), the Home tab and `Alt+H`. The rail starts at Documents, `Alt+H` resolves to nothing and no hint teaches it (the other views keep their `Alt+digit`), a window that last showed Home reopens on Documents. The recent documents Home listed stay under **+ → Documents** and the Windows Jump List | 2026-11-03 |
| `PRIVATE_CHAT_BUTTON_SHOWN` | The Koru AI composer's private-chat button — the detective mask at the left of its footer. No chat is made private from the composer (the button has no shortcut); a chat already private stays private and keeps its mark in the Chats list. The globe and the workspace chip start where the mask did | 2026-11-03 |
| `CONTEXT_TASKS_SHOWN` | The context column's *Tasks* section — the checklist tab beside a document, listing that document's tasks. The column opens on History; with Reminders and Cloud sync off too, History is its only section and no tab row is drawn. `Ctrl+Alt+J` steps past it; no shortcut opens it on its own. The Tasks view (its rail square, page and chord) is unchanged, under `KORU_ENABLE_TASKS` | 2026-11-03 |

Unlike a flag, a constant is off in the test suites too, so a test that
assumed the part is updated rather than run under an override.

## Not gated, deliberately

- `reference/SHORTCUTS.md` is generated from the whole `koru-keys`
  registry and still lists every chord: it documents Koru, not one build. The
  rows stay in the registry; `ShortcutId::shown_in_product` keeps a hidden one
  out of every hint.
- Background workers, their IPC and their data (above) — except the Socials
  publisher, which does not start with Socials hidden.
- Global search results that are chats or tasks: a hidden chat's result opens
  nothing.

## Tests

The suites run with **every surface on**: crates that read a flag turn on
`koru-features/testing` in their `[dev-dependencies]`, under which the default
is `Features::ALL_ON` and the environment is ignored. A test that means the
simplified product says so with `koru_features::testing::with(Features::ALL_OFF,
|| …)`, which overrides the answer on its own thread only.
`apps/koru/src/mainview/product_flags_tests.rs` runs the container that way
end to end; each gated surface also has an all-off test beside it — canvases in
`mainview/canvas/tests.rs`, a canvas without agents in `canvas_view/tests.rs`
and `koru-canvas-ui`'s own suite.
