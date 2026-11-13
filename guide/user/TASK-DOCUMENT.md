# Tasks and document

The experimental **Tasks and document** host places an existing task collection
beside a document editor. This development checkpoint is unfinished: its entry
is present only in a legacy menu, not the live **+** launcher. It is not yet
available through the normal UI. The behavior below describes the host under
development; native acceptance is still pending.

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

This is a development fixture. Chat-generated arrangements and saving or pinning
an arrangement are later stages. Native input and accessibility acceptance is
still being completed; the fixture is not a declaration of platform parity.
