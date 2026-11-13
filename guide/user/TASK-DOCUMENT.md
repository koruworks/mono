# Tasks and document

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
open gesture or **Go to origin**. The document opens inside the right region of
this tab. Search above that region to replace the displayed document, then use
the ordinary document editor, including its find and replace controls.

The task region operates on existing tasks. Create tasks or give an unfiled task
a document in the ordinary Tasks view. The embedded region omits those creation
controls; editing an existing task uses the same task commands as Tasks.

In a narrow window, **Tasks** and **Document** switch the visible region. Both
children remain alive while hidden. Widening the window shows both again.
Use the ordinary F6 chrome rotation to reach the tab's header. Tab moves
between its visible controls and then returns to the child. Enter opens a
selector or activates a region button; Escape dismisses suggestions.

This arrangement is transient. Closing its tab or quitting discards the
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
uses the native creation tool and chooses the list/document references and
arrangement. A named empty list can be found too. The same experimental host
opens after validation; changing its list, completing tasks, searching and
editing then work locally without more model calls.

Generation uses OpenAI, an Ollama model that advertises tools, or hosted Koru
AI when its endpoint accepts tools. Managed agent providers such as Codex and
Claude sessions do not support this creation path yet; ordinary chat remains
available. If tools become unavailable or opening is interrupted, chat says so.
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

Saving and pinning are later stages. The proposed
catalog is experimental and pending maintainer approval. Native input and
accessibility acceptance is still being completed; this is not a declaration
of platform parity.
