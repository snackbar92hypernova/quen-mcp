# quen: what it is for, and how it works

This document explains why quen exists, the principles it is built on, what it deliberately doesn't do, and how its parts work, at the level of ideas. It is written for people: testers, contributors, and anyone deciding whether quen fits their work.

Other documents cover the rest:
- [INSTALL.md](../INSTALL.md): installing and configuring quen, what it changes and how it restores, crash recovery;
- [SECURITY.md](../SECURITY.md): what quen may do on your PC, and what it trusts;
- [docs/agents.md](agents.md): the agent playbook, tool by tool, which quen serves to agents as `quen_guide`;
- [schema/README.md](../schema/README.md): every ledger table and view;
- [docs/clients.md](clients.md): MCP clients other than Claude Code.

Everything up to "Status" describes what quen does today, as the code in this repository does it. "Direction" at the end lists plans, and says so.

## Why quen exists

Testing a render or performance mod is the same chores, boot after boot: launch the game with the right settings and the right mods moved aside, get to the same place, toggle something, measure, take a screenshot, and put everything back. Done by hand it is slow. Done by an AI coding agent with only a shell, it is fragile: the agent patches settings files, moves mods, starts the game, sends keys, and relies on its own cleanup. When the agent is stopped, times out or loses its session, the cleanup doesn't run, and the game folder stays half-patched. The results end up in notes, and a later agent can't tell which number came from which boot, with which mods, under which conditions.

quen turns those chores into a service with guard rails. It is an MCP server and command-line tool. An agent asks quen for a test session; quen checks the request, records each change before it makes it, launches the game, and undoes every change on every way out. During the session the agent gets typed tools: control the game, capture frames, measure frame times, read and patch memory, fly the camera. Every boot, capture and measurement goes into a local SQLite ledger, beside the results of the other tools a modder uses, so a question asked next week is answered from recorded evidence rather than from memory.

The same tools serve more than testing:
- **Development:** an agent can drive a mod's build-and-try loop beside the modder's other tools: stage a new build, relaunch at the same spot, switch the mod through its own exec functions, and read what the game does.
- **Troubleshooting:** it can narrow down a mod list's conflicts, or a sudden loss of frame rate, boot by boot, parking a different set of mods each time, with every loadout and its measurements recorded.

quen supports one game: The Witcher 3 5.01 (DX12) on Windows. Its core logic is game-neutral.

## Principles

### Controlled boots

A test boot is defined by its inputs: the game build, the mods that loaded, the settings, the save, the tools attached. quen launches the game itself, writes the requested settings overrides, parks (moves aside) the mods the request names, and records all of it on the boot's ledger row. The game runs inside a Windows Job Object that quen holds, so quen always knows which process is its game, and closes only that one, never a copy the user started. Measurements check that the machine is quiet: a compiler, a build or a Java process running beside the game makes them warn, or refuse when asked to be strict.

### Always restore

Whatever a session changes in the game folder and the game's documents folder is put back when the session ends, whichever way it ends: a normal end, a failed launch, the agent stopped, quen killed, the PC losing power. The session journal (below) is how. This guarantee is the core of the tool: an agent can run test after test, and the user's install is never left worse than quen found it.

### Refuse before acting

Every tool checks what it can before it changes anything, and a refusal always means nothing was changed. A refusal gives its reason. When something is missing, such as a tool, a file or a newer version of quen's in-game package, it says what is needed and where to get it. Once a tool has made its first change, a failure unwinds through the journal instead of leaving a partial state.

### The user decides

The game runs on the user's PC, with the user's saves. Four things happen only with the user's agreement: a launch, a new save, a cleanup of captures, and the agent taking the controls from the user.
- The agent passes `user_ok`, its word that it asked the user in the conversation.
- These tools are marked so that Claude Code shows its permission prompt on every call, in every permission mode, with no "always allow". In other clients the user keeps them behind the client's own prompt.
- quen can't verify that a person agreed: `user_ok` is the agent's word, and the client's prompt is the real consent. The documentation says so plainly.

The same goes for the user's machine: quen never downloads or installs third-party tools, and the paths it finds go into its config only after the user confirms them.

### Evidence over opinion

A number means something only together with its conditions. quen records each boot's build, mod set, settings and conditions, and ties every measurement and capture to its boot. A claim, such as "this change lowers frame time here, outside drift", is a ledger row linked to the measurements that support or contradict it. It counts as proven only when measurements from at least two independent methods support it and no contradiction stands. "Proven" is computed from the evidence each time it is read, never stored, so it can't be set by hand or go stale. A claim found wrong is withdrawn, never deleted, and keeps its reason.

### Privacy

quen sends nothing off the PC: it has no network code, no telemetry and no update check. (In a Tracy launch, the Tracy client inside the game talks to Tracy's capture tool over loopback.) Screenshots are of the game's window; the agent can't ask quen for the screen. Everything quen produces stays under its data root, outside the source tree, and quen refuses a data root inside a git work tree, the game folder or Pictures. Results show paths under the user's folder as `%USERPROFILE%\...`. The repository holds source only, and every commit is checked for personal data.

## Scope and non-goals

quen is for developing, testing and troubleshooting mods on a single-player game installed on the user's own PC: launching controlled test sessions, driving the game into a state, capturing and measuring there, and keeping the evidence.

It is deliberately not:
- **a sandbox.** quen limits what quen itself does. An agent that also has shell or file tools can do anything the user's account can, with or without quen.
- **for online games.** quen reads and writes the running game's memory. It must never be pointed at a game with anti-cheat.
- **an installer.** The game, the Witcher Native Bridge, PresentMon, the profilers, ReShade and RenoDX are the user's to install, under their own terms. quen's own installer script installs only quen and its in-game package.
- **a replacement for the tools beside it.** REA / Ghidra and the RenoDX devkit stay MCP servers of their own; quen records what they find. Profilers are launched by quen, but their analysis stays in their own tools.
- **a game bot.** Keys and console commands exist to reach a test state, not to play.
- **a colour-grading meter.** Captures aim to be consistent within a comparison, not absolutely accurate; absolute-nits work belongs to the RenoDX devkit.
- **for several drivers.** One game session at a time, driven by one agent.
- **multi-game yet.** A second game gets an adapter when someone is actually modding it with quen.

## How it works

### Relay and background process

MCP clients start a server as a child process over stdio, and may kill its whole process tree when they exit or reconnect: Claude Code allows a server about 300 ms after closing its input. A game killed that way can be mid-save, and a journal that never replays leaves the game folder patched. So quen runs as two processes:
- `quen mcp` is the thin relay the client starts. It passes messages through and holds no state.
- `quen serve` is the background process. The relay starts it outside the client's process tree, and it owns the lock, the journal, the Job Object and the session. Relay and background process talk over a named pipe that refuses remote clients, only the user's account can open, and on which each end checks the other is the same `quen.exe`.

A session belongs to the connection that launched it. When that connection goes away, the background process ends the session the normal way: it waits for saves to settle, closes the game and replays the journal. The background process exits once no connection and no session remain. There is one per config file.

Session steps run on one dedicated thread. A cancel only sets a flag that is checked between steps, so no step stops half-way. Long calls send progress notifications. Clients other than Claude Code tend to time out after about a minute, so for them a call that runs past 20 seconds answers with a job to poll instead.

Some clients (Codex CLI, the Python MCP SDK) hold their servers in a Job Object that kills everything when the client closes and forbids leaving it. There quen refuses to start its background process, rather than run in a way that would take the game down with the client. Instead, the user starts `quen serve --stay` in a terminal before opening the client, and the client's relay connects to it. [docs/clients.md](clients.md) has the details.

### One driver

Only one session runs on a PC at a time: a lock file under the data root, held by the process that owns the session. A launch returns a random token, and every tool that touches the game needs it, so a second agent sharing the same quen, such as a subagent, can't drive by accident. `session_plan` checks a launch and lists every change it would make, with no lock and no prompt, so a helper agent can prepare a launch for the agent that talks with the user. While a session is live, the other connections' session tools are refused at once, and status, ledger reads and the guide keep answering.

### Results

Every tool returns one JSON envelope: `ok`, or `refused` with a reason (nothing changed); the state (lock, game, journal); the time each step took; artifacts as paths with their sha256; the ids of the ledger rows written. Logs come back as paths, never inline, so an agent's context stays small.

### The session journal

Before each change, quen appends the step to an append-only, fsynced journal, with what it takes to undo it, and marks it done afterwards. Ending a session is a replay of that journal from the last step back. Recovery after a crash is the same replay, at quen's next start or with `quen recover` from a terminal. There is one undo path, and a crash-injection test kills a session after every step and checks the replay.

Replay is careful not to lose anything:
- a moved file goes back only if its sha256 and file id still match;
- a patched settings file is restored from a backup that verifies against its recorded hash. If the game rewrote the file meanwhile, the game's version is kept beside it (`<name>.quen-current`), so neither version is lost;
- backups and patches are written atomically;
- what quen removes, such as a file it placed or a spent backup, goes to the Recycle Bin.

The game starts suspended, is assigned to quen's Job Object and then resumed; the job closes everything in it when quen closes it. quen kills only through the job, never by image name. Every program quen starts, the game included, gets a short allow-list of environment variables, never quen's whole environment, so an API key in the agent's environment never reaches the game, its mods or a profiler. At the end, quen copies the logs the game and its mods wrote during the boot into the session's log folder, since the next launch overwrites them.

### The game build

Before a launch changes anything, quen hashes the game's exe. The sha256 is the build's identity, and the PE time stamp is its human label; Steam's build id is recorded beside them as a second, independent sign of an update. An exe outside the known builds (the adapter's list, plus builds the user has verified and added) is refused, unless the launch explicitly allows an unknown build, which the boot row then records. quen finds a game update itself, never by waiting for a mod to refuse to load. A GOG install is matched only by GOG entries, and none are known yet. The reverse-engineering knowledge base names its build too, and the memory tools refuse an exe of another build.

### The in-game package

quen needs a way into the running game: to know its state, load a save by name, set the camera by numbers, pause the world, write a save. The game has no such interface. The public Witcher Native Bridge lets a native mod inside the game register functions that game scripts can call, but nothing outside the game can reach it. So quen ships its own small package, quen-w3, built from `w3/` in this repository:
- `W3Quen.asi`, a native mod loaded through the Native Bridge;
- `modQuen`, one script mod.

The two are always installed together, and quen's doctor pins both files exactly: half a package, a mismatched pair or a leftover of an older layout is refused before a launch. The build is reproducible, and releases are GitHub Release assets of this repository.

The package's command channel is locked to the session that launched the game:
- **Inert without quen.** The ASI opens its channel only when the game's environment carries a token that quen made for this launch. Without it, nothing listens and no command is handed to the script.
- **One pipe per launch.** The named pipe is named by that token, created as its first and only instance, refuses remote clients and is open only to the user's account. Each end checks the other: the client must be quen's background process, and the server must be the game in quen's job.
- **Typed verbs only.** Load a save, list saves, pause, set the time, the weather or the player's position, get and set the camera, play a camera path, write a save, read the world and the scene, run an exec function. There is no raw console string: the exec verb carries a function's name and its arguments, and the ASI calls the game's console executor with the line it builds from them, after checking the game build and the executor's first bytes. Messages are size-capped and carry ids and a time to live.
- **Run on the game's thread.** The script takes one command per game tick and replies. quen tells every outcome apart: done, refused by the script, expired, queue full, no reply. An unknown outcome counts as a failure, and what still waits when quen leaves is dropped, so a command never runs late.

Saves load by name through the same game call the menu's Load Game uses, without reading the menu off the screen. The package also keeps console commands that load a save by name, which quen types when the channel isn't up. Typed text is mapped through the keyboard layout to the keys the game's console expects, so it comes out right on any layout, and quen never uses the clipboard.

### Game state

modQuen watches the game's state: gameplay, a scene, a cutscene, photo mode, a fade or loading, dialogue, a menu open. W3Quen.asi writes each change to a state log beside the game's exe. quen relies on this state: a launch or a load is done when it shows gameplay, typing is refused at the main menu, a save needs plain gameplay, and when quen brings the game back to the front, it closes the pause menu the game opened when it lost focus. A state log older than the current launch belongs to an earlier session and is marked stale. The state comes from the game's scripts, so any script mod could write it too: quen treats it as the mods' word as much as the game's.

### Saves

The user's saves are theirs. quen never copies, moves, edits, restores or deletes anything in the save folder. It backs up the folder into its data root before each launch, looks at it after every batch of keys and every scripted load, waits for it to stop changing before it closes the game, and reports at the end every save the game wrote. A save written after a live change (a memory patch, a cvar write, a teleport, a change of time or weather) is marked tainted, because it may carry that change.

In a test session, a save other than the game's own autosaves and checkpoints ends the session. The one exception is `game_save`, which asks the game for one new manual save, as the menu's "Empty save slot" does. Its command carries no slot or name, so it can't overwrite a save. quen checks the save folder before and after, and accepts exactly one new manual save; anything else fails the call.

### The ledger

The ledger is a SQLite database under the data root, never in git. Big files stay on disk; the ledger holds their paths, sha256, sizes and metadata. It records games and builds; boots with their settings, mod set and conditions; saves; captures; measurements; camera poses and sweeps; claims and their support; references from documents to captures; and game knowledge. Views answer the usual questions: a boot's conditions, the rows of an A/B run, how two boots differ, which claims are proven.

- `ledger_query` runs read-only SQL with a time limit.
- `ledger_ingest`, and `quen ledger add` for scripts, write rows from the other tools. A row lands on the running session's boot only when the call carries that session's token.
- Migrations are numbered, and a shipped one is never edited, only followed by a new one, so a tester's ledger survives every update. quen migrates an older ledger when it starts, after a backup.
- [schema/README.md](../schema/README.md) describes every table, view and column, and quen serves it to agents.
- Boots from the Python scripts quen grew out of can be imported. On such a boot an empty column means unknown, never off.

### Claims and proof

Each measurement names its method family, one name per method rather than per tool, so two tools doing the same thing can't pass as two methods: PresentMon, Tracy, Nsight, RenderDoc, the RenoDX devkit, quen's screenshots, static reverse engineering, live memory, and files read by the agent or by quen's data tools. Each also carries a tag: measured, observed in code or files, static reverse engineering, inferred, or unverified. Only the first three count towards proof.

A claim is proven while it is open, has support from at least two distinct method families, and has no standing contradiction. A contradiction stands until two families support the claim again from boots that started after the contradicting one. Grades written by a knowledge-base author are kept as labels; they never count as proof.

### Mod sets

Version numbers drift, many mods have none, and a mod's config changes its behaviour as much as its files do. So quen names the mods that loaded by fingerprint:
- an entry's identity is its kind and name (`asi:W3ElementMap`);
- its build is a content hash: the sha256 of a file, or a tree hash of a folder;
- its config is a hash plus the values of the keys that change behaviour;
- files a mod writes while the game runs (logs, backups) are ignored, so a new log doesn't make a new mod set.

The same mod-set id means the same files and the same config, and two sets can be compared entry by entry. A version string, where one exists, is only a label.

### Captures

quen picks the capture method; the agent never asks for the screen.
- **SDR:** a grab of the game's window.
- **HDR:** Windows.Graphics.Capture of the game's window in 16-bit float. Where that isn't available, quen falls back to Desktop Duplication cropped to the game's window, only while the game is in front, checked just before and just after the frame, and marks the capture `includes_desktop`, since a notification drawn over the game can show in it.

Each capture records its method, its encoding (sRGB, PQ BT.2020, scRGB float, PQ code values) and the display state it was taken under, so a comparison can tell when captures don't match. The aim is consistency within a comparison, not absolute accuracy.

Captures can grow large, so quen reports the capture folder's size against a budget, shows usage by boot, kind and age, and cleans up only what the user approves. The same cleanup covers quen's other leftovers under its data root: session logs and the settings backups they keep, the ledger's backups, oversized query results, old package zips. A file goes to the Recycle Bin when the bin can take it whole; otherwise Windows' own dialog asks the user whether to delete it for good, and No keeps it. quen never deletes a file for good on its own decision. Captures that evidence a claim, a pose or a document stay unless named. The ledger rows and their hashes stay after a cleanup, so a claim keeps the record of its evidence.

### Measurements

`measure_presentmon` runs PresentMon on the game for one window and records its statistics. `measure_ab` compares states inside one boot, because frame times drift between boots:
- it applies state A (a cvar, a mod's hotkey or mode switch), lets it settle, measures, then does the same for B, A and B again;
- drift is how far the two A windows differ, and the two B windows;
- the verdict is "outside drift" only when the difference between A and B exceeds both drifts;
- a window whose present mode differs from the others is refused, and the states are put back at the end;
- each window's UTC times are returned, so another tool's capture of the same boot can be cut to the same windows.

`run_plan` runs a plan of quen's own tool calls as one call, each step checked against its tool's schema before any runs, with a setup that a relaunch repeats and a teardown that runs on every way out; an A/B inside it is a `measure_ab` step. No window measures a paused world: quen closes the game's pause menu first, or refuses.

### Profilers

A launch can attach a profiler through a launch profile, whose file changes are ordinary journaled steps and whose conditions go on the boot row:
- **RenderDoc.** The game loads Direct3D 12 through NVIDIA Streamline's interposer, which hides the calls from RenderDoc. quen refuses unless the game's interposer is NVIDIA's signed original, swaps in a patched development build for the session, and afterwards checks that the original is back by sha256 and signature.
- **Nsight Systems and Nsight Graphics GPU Trace.** ReShade is parked for both; GPU Trace locks the GPU clocks, and the boot is flagged so.
- **Tracy.** The instrumentation is built into quen's own ASI and stays dormant (no hook, thread or socket) in every other launch. The launch parks other mods that hook the same functions, as the knowledge base lists them, and starts Tracy's capture tool on a loopback port.
- **The RenoDX devkit** can be loaded for shader and resource work. quen checks that it really loaded.

A profiled or devkit boot is its own condition, recorded on the boot row. The measure tools warn while the devkit is loaded or Nsight or RenderDoc is attached, and refuse when strict. A Tracy boot's numbers are compared only with numbers from the same boot, never with an untraced one.

### Live memory

The memory tools read, scan and follow pointers in the running game. Writes are narrow:
- a patch must be defined in the reverse-engineering knowledge base: at most 8 bytes inside one aligned 64-bit word, applied only when the game build and the bytes in memory are the ones it expects;
- an undo writes only while the bytes are still the patched ones;
- a config variable (cvar) is found by name, with its layout taken per build from the knowledge base;
- the process is always the game in quen's job, and the write handle is opened only for the write.

Every write is journaled, recorded on the boot row and taints later saves. quen doesn't attach a debugger, because a debugger's exit can kill the game outside the journal. `measure_ab` can also switch a loaded mod's mode by calling its exported function.

### Camera sweeps

Photo mode's camera can be placed by numbers and read back; a set that falls short, because the camera collided with geometry, fails rather than pretending. A camera sweep is data: keyframes (position, rotation, field of view, easing), a timing mode, and the world state it expects. It is stored once by name in a canonical form and never changes, so the same sweep replays after a mod or setting change, and two runs compare frame by frame.

quen sends the whole path to the game once. The ASI computes it and the script sets the camera on every game tick, so playback is smooth and tied to frames, not to pipe timing. The path keeps the horizon level, takes the short way round, and holds still at a hold. A sweep plays in real time, or frame by frame: the world paused, quen asks for each frame, checks it against its own computation and captures it. A digest over the frames' hashes says at a glance whether two runs are identical.

### Play sessions and hand-overs

In a play session the user plays a game quen launched with almost nothing changed: their mods and settings stay as they are, and quen turns on the console line (and adds `steam_appid.txt` if it's missing), both put back at the end. The user has the controls. While they do, quen refuses every tool that sends input, writes to the game or moves the focus. Reads still run (screenshots, frame times, memory reads), and the playbook tells the agent to make one only when the user asks. When the user wants the agent to look at something, the agent takes the controls (the client prompts the user), investigates, and gives them back; it can't give them back while one of its memory writes is still applied, unless the user agrees to keep it. The user's saves stay theirs. The session ends when the user quits the game; closing the chat leaves the game running, after quen undoes the memory writes of a hand-over in progress that the user hasn't agreed to keep.

A test session can hand the controls to the user too, to set something up in the game, under the same rules: while the user has them, quen refuses input, writes and focus moves, and a save the user makes is theirs, listed at the end rather than ending the session. The agent's memory writes may stay applied across such a hand-over. When the controls go to the user, the game says that they have them and hand them back in the chat, and keeps saying it until the agent takes them back.

The user also sees when to keep their hands off. The game shows one line of quen's that lasts and follows what quen does: the agent at work, measuring for about so many seconds, the user's controls, or the agent waiting for the user in the chat. quen sets it at its own transitions and never reads it from the game; it sends it again every 10 s, since the game's own notifications and its inventory key clear it without quen seeing it. The game draws the line into its frames, so quen takes it down half a second before each of its screenshots, frame-time windows and sweeps, and puts it back after. When the agent has the controls and makes no call for 60 s with the game in front, it is waiting for the user: quen brings the agent's chat to the front, and the agent's next game call brings the game back. In a play session it says from the launch that the user has the controls, and that the agent is at work while it has them. The user can turn the lasting line off; the game then shows a short notice at those moments only. The line is the game's own notification, the one hands_give showed first, made to last: it shows in gameplay, in photo mode and over the pause menu, and a window of quen's over the game would change how the game's frames reach the screen during a measurement. Where the game draws its HUD, the package also has a line of its own in the HUD, styled like it, so the line looks like part of the game; the notification carries it everywhere else.

### Game knowledge

Reverse engineering a game binary is expensive, and its results get lost between sessions. The tool used, REA driving Ghidra, analyses the exe afresh in every session (about 17 minutes for The Witcher 3) and keeps no analysis. quen works with a reverse-engineering knowledge base kept as files in a separate repository, never in this one: tables of function names, hook sites, patches and cvar layouts, each naming the build it was made on, plus notes.
- Function names from the knowledge base become REA's seed file, so every fresh analysis starts knowing every name found so far.
- REA's answers are kept whole as evidence files under the capture root and indexed in the ledger, keyed by the query and the analysis profile, so an answer already found is read back instead of asked again.
- The ledger indexes the knowledge base: entities (functions, hooks, patches, shaders) with their addresses per build, links between them, the notes' sections and what each mentions, and claims taken from its tables. `ledger_about` gathers everything known about one entity.
- Decompiled code never goes into a repository, a note or a ledger text column: the evidence files under the capture root are the one place it is kept.

### Setup and paths

quen keeps its config, ledger, logs and captures under `%USERPROFILE%\.quen`. It avoids AppData on purpose: Windows redirects AppData for packaged apps such as Claude Desktop and everything they start, so two clients would see two different ledgers and two locks. `quen_setup` finds the game (Steam's library folders and manifests, GOG's registry entry) and the optional tools, shows what it found with where each value came from, and writes only what the user confirms. `quen_doctor` is read-only: it lists every dependency as found, missing or the wrong version, what each one enables, and where to get it. A session needs the game at a known build, the Witcher Native Bridge and quen's package; PresentMon, the profilers, ReShade, RenoDX and its devkit, and REDkit each enable their own tools.

### Core and adapters

The code is two crates:
- `quen-core` holds the platform-neutral logic and runs its tests on any OS: the journal and its replay, the settings parser, the park planner, PresentMon statistics and the A/B verdict, the HDR encoder, the ledger, the state-log parser, the command-channel protocol, the camera-path maths, the build pin and the seed files.
- `quen` is the program: the MCP front, the background process, the Windows layer and the Witcher 3 adapter.

The command-channel protocol and the game-state source are defined in the core, game-neutral, so that another game's in-game end could reuse the Rust side. The Witcher 3 code stays a module of the program until a second game is really being modded with quen.

## Safety and privacy model

- **The trust boundary is the user's Windows account.** Any program running as the user can drive quen, read what it records and change the files it trusts (its config, its journal, the knowledge base's tables). quen limits its own actions; it doesn't defend against the account itself.
- **Consent for what matters.** Launches, new saves, capture cleanups and the agent taking the controls from the user need the agent's word and the client's prompt. A denied or pending prompt is the user's answer, and the playbook forbids routing around it.
- **Contained processes.** Everything quen starts runs in its Job Object, with a minimal environment, and is killed only through it.
- **Awake while live.** While a session is live, quen keeps the PC and its display from sleeping, and lets go when the session ends.
- **Nothing lost, ever.** Undo through the journal, keep both versions when a file changed underneath, Recycle Bin instead of delete (a file the bin can't take is deleted only on the user's Yes in Windows' own dialog), saves never touched.
- **Narrow live writes.** Knowledge-base patches, checked bytes and build, cvars; all journaled, all tainting later saves.
- **No network.** No telemetry, no update check, and quen opens no listening socket; the in-game channel and the relay pipe are local and locked to the user and to quen's own processes.
- **Data, not instructions.** What the game, its mods, their logs and the ledger hand back is data for the agent, never instructions; a reason in a result is quen's own words.
- **Personal data stays home.** Captures and logs stay under the data root; some can identify the user (a game log holds a Steam id, a main-menu screenshot shows account names, Nsight's reports name the PC), and the playbook tells agents never to copy them into a repository or a shared note. The repository's commit hooks scan for personal data with generic patterns and with each contributor's own deny-list, and CI runs the generic scan.

[SECURITY.md](../SECURITY.md) lists the details: the programs quen starts, the files it changes, what it trusts and its limits.

## Status

- **Early access.** quen is 0.1.24; its in-game package is quen-w3 0.1.18.
- **One test machine.** Built and tested on one PC: The Witcher 3 5.01, Steam, DX12, on Windows, with one GPU and one display. GOG copies are untested: their exe counts as an unknown build.
- **Clients.** Claude Code is the client quen is built and tested with. VS Code, Cursor and Gemini CLI pass the protocol checks with the official MCP tools, but haven't been tried with a model. Codex CLI works with `quen serve --stay` started in a terminal first; tested with real Codex CLI (a launch, in-game calls, closing Codex mid-session), with Codex's approvals set so only the launch asks.
- **Not yet checked:**
  - whether a window grab gives the same values as the game's own frame, which HDR screenshot values depend on: until that check runs, treat HDR values as untested;
  - whether quen's job still holds the game when it is started under Nsight;
  - whether a stale dialogue state can refuse a save that should work.

## Direction

**These are plans, not features.** None of what follows is built.

- **Installing the package from quen.** A journaled install of quen's in-game package from a release, with the user's approval.
- **Recovery at logon.** An optional, user-approved task that replays a stale journal when the user logs on.
- **Retiring the typed console commands** once the channel's load has proven itself in the game.
- **Frame-level knowledge.** Tables for a frame's draws and resources, their importer, and a graph view over the ledger; single entity and link rows from agents; a ledger rebuilt from the knowledge base's exported claims; search over the knowledge base.
- **Recording mod load status.** Which installed mods actually loaded or refused, read from their logs, on every boot.
- **More memory tools when a test needs them twice:** watching a value, snapshots and diffs, data breakpoints.
- **GOG support:** hashing GOG's exe, comparing the two builds, and checking the bridges and quen's package on it.
- **Observing a game quen didn't launch.**
- **A second game,** through its own adapter, only once someone is modding it with quen.
