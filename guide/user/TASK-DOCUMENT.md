# Tasks and document

## Composed document views

The experimental composition catalog lets a model arrange Koru rows, columns,
boxes, text, spacers, dividers, buttons and one document editor. It uses named
Koru spacing, surface, radius and typography tokens. It is a bounded native UI
description, with no generated JavaScript or CSS.

Try this in AI chat after creating the four documents in the current workspace:

> Create a mini app with four buttons across the top: Travel, What to do Next,
> Best Restaurants and Trip Notes. Show the selected document in an editable
> area below. Clicking a button should switch that area to its document. Use
> Koru's composition catalog and resolve the actual documents in this workspace.

The model discovers the document references and submits the layout. Subsequent
button clicks select documents locally, without asking a model. Document text
stays in the ordinary workspace documents; the declaration contains their
references and the UI labels.

Keep the mini-app tab selected and use its AI side pane for layout refinement.
The model receives the current layout, including component identities and
document references, so this also works after opening a saved composition.
This request context contains no document text. The provider may retain its
prompt according to its own runtime behavior.

Save and Pin use the same controls described below. Composition saves use a
new mini-app file format; old task/document saves remain readable. An older
build refuses the newer saved format rather than rewriting it. This does not
migrate the workspace database. Use disposable test documents for the first
save/reopen acceptance check.

The A2UI playground is a separate future discussion. This catalog does not
expose every Koru control, arbitrary actions or complete upstream A2UI support.

## The task/document view

The experimental **Tasks and document** host places an existing task collection
beside a document editor. Open **+**, then choose **Tasks and document** in the
New launcher. Use the arrow keys and Enter, a pointer, or accessible activation
to choose the tile. The host remains experimental; native acceptance is still
pending.

Choose Today, Next, Upcoming, Later, Completed, or one of your lists in the
dropdown. **More lists** and **Previous lists** page through longer list
collections. This selection changes which tasks are displayed; it does not
move tasks between lists.

The menus page their choices to fit the window. Search results offer
**Previous results** and **More results** when needed. Refine the search to
find documents outside the first twenty matches.

Complete a task using its checkbox. Open its source through the task's ordinary
open gesture or **Open document**. The document opens inside the right region of
this tab. Search above that region by document title in the current workspace
to replace the displayed document, then use the ordinary document editor,
including its find and replace controls. This search chooses documents;
Find searches text inside the open document.

The task region operates on existing tasks. Create tasks or give an unfiled task
a document in the ordinary Tasks view. The embedded region omits those creation
controls; editing an existing task uses the same task commands as Tasks.

In a narrow window, **Tasks** and **Document** switch the visible region. Both
children remain alive while hidden. Widening the window shows both again.
Use the ordinary F6 chrome rotation to reach the tab's header. Tab moves
between its visible controls and then returns to the child. Enter opens a
selector or activates a region button; Escape dismisses suggestions.

An unsaved arrangement is transient. Closing its tab or quitting discards the
arrangement, and it is not restored as a generated tab after restart. Task
changes and document edits belong to their ordinary documents and save through
the existing workspace. A failed source opening leaves the current editor in
place.
If an accepted task change is still processing, quitting or switching
workspace/profile waits for it to finish or fail. A message explains why the
current workspace remains open; try the action again after processing finishes.

In an AI chat tab or the AI pane, ask for a mini app with a task list and a
document, and say which side each belongs on. For example: “Create a mini app
with my Travel list and Trip notes, with the document on the left.” The model
uses the mini-app creation tool and chooses the list/document references and
arrangement. A named empty list can be found too. The same experimental host
opens after validation; changing its list, completing tasks, searching and
editing then work locally without more model calls.

Managed AI chats use Koru's installed MCP tools to create and refine mini apps:
Codex, Claude, OpenCode, Kimi, Grok, Antigravity and Devin use their registered
managed routes. Antigravity uses its ACP server, rather than a raw `agy` terminal.
The agent's supported runtime and the installed desktop bridge must be available.
Tool-capable direct providers are also supported. If the
required tools are unavailable or opening is interrupted, chat says so;
ordinary chat remains available.
History and reconnect do not recreate tabs. If no app opened, use **Retry** on
a failed or interrupted response. If reconnection recovered a completed
response with no Retry action, send a new request instead. Retrying a creation
already applied in this live chat cannot open
another copy. Switching workspace/profile, hiding the originating chat, or
closing it retires pending delivery. Keep the requesting chat visible until
the mini-app tab opens.

To rearrange an open mini app, keep its tab selected and open the AI side pane.
Ask **“Put the document on the left and tasks on the right.”** The model's
validated update swaps the same two regions. Selection, scroll, search and
native editing state stay with their existing controls. This route currently
uses the side pane beside the selected mini app; a separate chat tab has no
refinement target. Changing or closing the target before the result arrives
refuses the update and keeps the arrangement. A request cannot remove the
editor or its controls. Choosing a different list or document also makes an
older pending arrangement update stale; send a new request for the current view.

A message queued while another reply runs retains its original mini-app target
and revision. If that target changed before dispatch, the queue pauses and keeps
the message. Return to the unchanged target to send it, or use **Edit** and send
a new request for the currently selected view. Historical replies never apply
arrangement updates again. **Retry** also retains the original target and
revision; a changed view needs a new request.

Use **Save mini app** above the regions to save an immutable artifact version.
The artifact stores the arrangement and its linked list/document choices;
tasks and document text stay in their ordinary documents. Wait for an opening
document to settle before saving. A saved arrangement can be opened from the
workspace's **Artifacts** list, with current source data and no model call.
**Artifacts** and **Agents** are shown in the apps rail by default.
It does not automatically reopen at startup. Saving again after changing the
arrangement or its choices creates another version; earlier versions remain.

While saving, closing waits for the result. If saving fails, **Retry mini-app
save** finishes that earlier save with the same choices. Save again afterward
to include newer changes. A failed save never means the arrangement is saved;
source document edits and completed tasks still follow their ordinary save
paths. Closing after a failed save discards the transient arrangement.

Saved apps open only in their original workspace. A missing linked list or
document, an unsupported format, or unavailable artifact data produces a
visible refusal and leaves the saved artifact unchanged. Opening a saved
version preserves any already-open mini app with different unsaved choices.

Use **Pin mini app** above the regions to keep a saved version in this device's
apps rail. Pinning an unsaved arrangement first saves it. The pin appears only
after both steps succeed. If the pin change fails after saving, the artifact
remains in **Artifacts**; **Retry pin change** retries that exact saved version.
Changes made after requesting Pin do not change its capture. A new Pin request
waits if an earlier save still owns different choices or a document is opening.

Pins form their own group after the built-in apps. Click a square to open its
saved arrangement with current source data, without asking a model. Use F6 and
the arrow keys to focus a square, then Enter or Space to open it. Drag squares
within the pin group, or use Ctrl+Shift+Up/Down while focused there. These keys
stop at the pin group's ends. Built-in Alt+number shortcuts retain their targets;
pins have no Alt+number shortcut.

The **Pinned mini apps** square after the pins opens their menu. It includes all
pins across **Previous pins** / **More pins** pages; short windows keep this
menu while folding pin squares. Highlight an app and use
Ctrl+Shift+Up/Down to reorder it while keeping the menu open, including across
pages. The menu also has an **Unpin mini app** action beside each app, so a pin
whose artifact cannot currently open can still be removed. Failed rail changes
offer **Retry pin change** with their original placement. Escape returns focus
to the pin square or its overflow control.

**Unpin mini app** removes the local reference and keeps the saved artifact,
documents and tasks. A pin keeps its exact immutable version; later saves do
not silently retarget it. Unpin and explicitly pin the new saved version to
change it. Pins are separate for each profile/workspace on this device and do
not travel when copying a profile. They are not restored mini-app tabs.

The proposed catalog is experimental and pending maintainer approval. Native
input and accessibility acceptance is still being completed; this is not a
declaration of platform parity.
