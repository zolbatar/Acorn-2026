# Luna instructions: minimal desktop, slice 1

Implement slice 1 of the Ricochet desktop: **explore and launch**. Deliver working code, appropriate tests, documentation, and evidence of the end-to-end experience. Do not stop at a plan or a visual mock-up.

## Product goal and roadmap

The minimum useful desktop lets someone find a program, run it, edit its source, save it, and recover when it goes wrong.

Build that experience in three slices:

1. **Explore and launch — implement now:** enter the desktop, use the icon bar to open the mounted volume, browse folders, double-click a BASIC program to launch it, and see its running task on the icon bar.
2. **Create and save — later:** a small text/BASIC editor with Open, Save, Save As, Run, and unsaved-change handling; shared writable controls and clipboard; Filer create-folder, rename, copy, move, and delete operations with appropriate confirmation.
3. **Manage and recover — later:** a desktop MOS/BASIC command window, a task list with stop controls, fuller error reporting, and orderly desktop exit with unsaved-work checks.

Across those slices, grow common Wimp facilities for icons, menus, keyboard focus, writable fields, clipboard, errors, file opening/saving, and redraw. Implement only the subset needed for the current slice, documenting its boundaries.

The eventual acceptance journey is: open Examples, run a program, edit a copy, save it in a work folder, run the copy, and close everything cleanly from the desktop. Slice 1 establishes the first part of that journey.

## Read and inspect first

- Read `AGENTS.md` and `docs/ricochet-design.md` before implementation. The brief is the current direction; distinguish requirements from unresolved proposals.
- Inspect Git status and preserve unrelated work. Do not reset, discard, or include unrelated changes in a commit. This handoff does not request a commit or push.
- Inspect the current implementation rather than assuming the brief describes every latest change.
- Start with `src/wimp.rs`, `src/window.rs`, `src/renderer.rs`, `src/runtime.rs`, `src/filesystem.rs`, `src/swi.rs`, `src/configure.rs`, `tests/wimp_desktop.rs`, and the editable BASIC programs in `examples/wimp/two-windows`.
- Read the README for current launch commands, desktop input conventions, and execution preferences.

## Architecture and compatibility requirements

- Keep this a hosted environment in the existing host window and shared Wimp service.
- Implement Filer behaviour, launch policy, and desktop policy in inspectable **BASIC64**. Rust supplies mechanisms: task lifecycle, checked filesystem access, rendering, input routing, and messaging. Do not implement the whole Filer in Rust for convenience.
- Keep guest addresses logical and caller-scoped. Validate guest buffers and task/window ownership at service boundaries; do not expose host pointers.
- Reuse the existing HostFS/FileSwitch path model and mounted volume. Do not introduce a second filesystem abstraction or let the Filer browse arbitrary host paths outside that model.
- Reuse the shared BASIC loading/execution path and saved execution preferences. Preserve source directives and source/tokenised compatibility behaviour; do not create a desktop-only interpreter or force a different engine.
- Preserve documented public SWI contracts. Before implementing a standard Wimp call, consult its authoritative documentation for register/block shapes and semantics. Reject unsupported forms explicitly. Clearly distinguish any necessary project extension from a historical SWI.
- Keep the existing two-task demo working. Reuse the desktop's current visual resources and coordinate/input conventions so painting and hit-testing agree.
- Update the design brief when resolving an architectural question, recording the decision, compatibility implications, and remaining limits. Do not treat this handoff as approval for a broad application-manifest or scheduling redesign.

## Slice 1 required behaviour

### 1. Desktop startup

- `DESKTOP` from the normal MOS prompt starts the usable desktop in the existing host surface.
- Start the necessary desktop components through the real task/service machinery. Do not automatically launch the sample applications.
- Avoid duplicate desktop components if the startup path is invoked again where supported.
- Keep `--desktop-demo` available as its existing separate demonstration path.

### 2. Icon bar

- Show the mounted volume on the left and running user applications on the right.
- Clicking the volume opens its root in the Filer; repeated activation must have a consistent, documented window reuse policy.
- Each running user task has a stable entry with a readable name. Clicking it brings its existing windows forward where applicable.
- Add and remove entries on actual task startup and exit, including failed execution. Do not infer task lifetime from whether it has an open window.
- Reserve icon-bar space consistently and keep icons clickable as the host window changes size.
- A full system menu and task-control panel belong to later slices. Only add menu behaviour here if needed for this slice's interactions.

### 3. Filer browsing

- Open a directory window showing the current guest path and distinguishable directory/program/file entries with readable names.
- Support selection, double-click to open directories, scrolling when needed, and a discoverable way to navigate to the parent directory.
- Populate entries from the real mounted volume. Do not hard-code a listing of examples.
- Keep directory windows responsive and preserve correct content when moved, resized, covered, exposed, or scrolled.
- Handle empty directories, long names, and listing/open failures without crashing the desktop.
- File editing and mutations are out of scope. Unsupported file types should produce a clear, nonfatal explanation when opened.

### 4. Launch BASIC programs

- Double-click a supported BASIC program to launch it as a separate desktop task through the existing loader/runtime.
- Support source and tokenised program forms already supported by the runtime. Reuse existing classification and metadata where available; do not invent conflicting extension rules.
- Preserve the caller-scoped guest filesystem context and document how the launched task's working directory is chosen.
- Keep the desktop usable while the program runs. Its exit must not terminate the desktop or other tasks.
- Support a Wimp program opening its own windows on the shared desktop.
- Account explicitly for ordinary BASIC programs that print, draw, or request input without opening Wimp windows. Provide the smallest usable task-owned output/input window using existing display/input mechanisms, or reuse an equivalent existing facility. Do not silently run them invisibly or take over the desktop surface. A full command window is still a later slice.
- Report loading and execution errors visibly with a dismissible message, without leaving stale windows or icon-bar entries.
- Full application-directory registration and file-type associations are later work unless a narrow existing application convention can be reused directly. Do not expand this slice into a package manager.

### 5. Shared Wimp foundations

- Add the icon, hit-testing, event, lifecycle, and redraw support required by the icon bar and Filer as reusable facilities.
- Maintain ownership boundaries when multiple tasks create windows/icons or receive input.
- Implement a coherent minimal redraw path for new content. If retaining an existing snapshot adapter for program output, document its limitations separately from any standard redraw services added.
- Do not build a second host-only widget tree containing Filer policy. Desktop BASIC64 components must drive their visible content and respond through published services.

## Suggested implementation order

1. Establish the smallest service contracts for icons, directory enumeration, task launch/lifecycle, and required redraw/input; reuse existing contracts wherever possible.
2. Implement the necessary shared runtime/Wimp mechanisms with focused tests.
3. Add BASIC64 desktop startup and icon-bar components, including the real mounted-volume entry.
4. Add BASIC64 Filer directory enumeration, navigation, selection, and scrolling.
5. Connect file activation to separate task launch and icon-bar lifecycle updates.
6. Complete output/input presentation and visible error handling.
7. Exercise the full journey, fix integration issues, and update documentation.

Prefer a small functioning vertical path early, then complete the edge cases. Continue autonomously through routine implementation decisions. Ask only when a genuine unresolved product or compatibility choice prevents progress.

## Acceptance checks

Demonstrate the following through the normal frontend:

1. Start at the MOS prompt and enter `DESKTOP`.
2. See an icon bar with the mounted volume, without automatically opened sample windows.
3. Click the volume, browse to a directory containing examples, and return to its parent.
4. Open a directory large enough to require scrolling and activate an entry after scrolling.
5. Double-click a BASIC Wimp example; its window opens on the same desktop and its task appears on the icon bar.
6. Launch a second program and verify independent windows, input routing, and task entries.
7. Click a task entry to bring that task's windows forward.
8. Launch a simple non-Wimp BASIC program and verify visible output and usable input when requested.
9. Let a task finish normally and verify its resources and icon-bar entry are removed without disturbing the desktop.
10. Try a malformed program, an unsupported file, and a file removed since listing; each failure remains visible and recoverable.
11. Move, resize, cover, and expose Filer windows; verify correct painting and hit targets. Resize the host window and check icon-bar placement.
12. Verify the existing two-window demo still runs.

Add meaningful automated tests for directory navigation/path boundaries, task launch and cleanup, ownership/input routing, and relevant new SWI contracts. Use existing test infrastructure and run appropriate regression checks. Do not substitute screenshot-only tests for behaviour.

Perform live visual/interaction checks if the environment permits. If a check cannot be run, state exactly what is unverified; do not claim that unit tests establish a working desktop experience.

## Deferred scope

Do not implement the editor, file mutations, a full desktop command window, task-manager UI, forced-stop controls, general application registration, session restoration, pinboard, wallpaper preferences, clock, search, thumbnails, widgets, or a full settings application in this slice.

## Completion report

Report what works, where the BASIC64 desktop sources live, how to launch and exercise the slice, which tests and manual checks passed, and any remaining limitations. Update the README and design brief so they accurately describe the delivered behaviour and supported service subset.
