# quen

**An MCP server that lets an AI coding agent drive The Witcher 3 for mod testing: reproducibly, with guard rails, and with a record of every result.**

quen gives an agent (Claude Code, VS Code's agent mode and other MCP clients) the tools to launch the game into a test session, control it, capture frames, measure frame times, read and patch memory, and fly the camera. Afterwards it puts the game back exactly as it was. Every boot, capture and measurement goes into a local SQLite ledger. Results from the other tools a modder uses (the RenoDX devkit, Nsight, RenderDoc, Tracy, REA / Ghidra) go there too, so a later agent can investigate across all of them.

> **Status: early access.** quen has been built and tested on one PC: The Witcher 3 5.01 (Steam, DX12), on Windows. Expect rough edges, and config keys that may still change.

## Why

Testing a render mod by hand means the same chores every time:
- launch with the right settings and the right mods parked;
- get to the same spot and toggle a setting;
- measure, screenshot, and put everything back.

quen makes that the agent's job, with guard rails:
- every tool checks first, and changes nothing when it refuses;
- every change to the game's files is journaled before it is made;
- the journal is replayed on every way out: a normal end, a failed launch, a crash.

## What it can do

- **Sessions.**
  - `session_plan` checks a launch and changes nothing.
  - `session_launch` starts the game at a named save, with your `dx12user.settings` overrides, mods parked, frame generation and VSync as asked, and photo mode.
  - `session_end` puts every file it changed back, byte for byte, write times included, and reports any save the game wrote.
  - Play sessions (`mode: "play"`): you play the game quen launched with your mods and settings as they are, and hand the agent the controls when you want it to look at something; the session ends when you quit the game.
  - One agent drives at a time. A crash is recovered at quen's next start, or with `quen recover`.
- **In-game control.**
  - Console commands typed for your keyboard layout. quen never uses your clipboard.
  - Keys, and loading a save by name.
  - Pause, time of day, weather and teleport.
  - Engine variable (cvar) reads and writes, which last only for the session.
- **Captures.**
  - SDR and HDR screenshots of the game window (Windows Graphics Capture, with Desktop Duplication as the fallback).
  - Disk use tracked against a budget. Captures are cleaned up only when you approve, to the Recycle Bin, never as a hard delete.
- **Measurements.** PresentMon frame times, and A/B/A/B comparisons inside one boot, with a drift check and a quiet-machine check. The result is a verdict, not just a number.
- **Camera sweeps.** Keyframed paths on the photo-mode camera:
  - played in real time, or frame by frame with a capture of each frame;
  - stored, so the same sweep replays after a change and the two runs compare frame by frame.
- **Profilers.** Launch under:
  - **RenderDoc.** quen swaps in a patched Streamline interposer, then puts NVIDIA's signed file back and verifies it.
  - **Nsight Systems** or **Nsight Graphics** GPU Trace.
  - **Tracy.** Built into quen's own package, dormant unless a Tracy launch triggers it.
  - Or with the **RenoDX devkit** loaded.
- **The game's data.** Find, read and scan the files in its bundles and in REDkit's editor depot (CR2W files decoded to JSON; which files import or name something), and run REDkit's offline script compile on a scratch copy.
- **Memory.**
  - Read, scan and follow pointers in the running game.
  - Apply and undo live patches defined in your reverse-engineering knowledge base. Saves written after a patch are flagged as tainted.
- **The ledger.**
  - Boots, with their game build, mod set, settings and conditions.
  - Captures and measurements.
  - Claims, which count as proven only when two independent methods support them.
  - Reverse-engineering findings and knowledge-base notes, indexed per function, shader or hook (`ledger_about`), so nothing has to be extracted twice.
  - Views answer the common questions; `ledger_query` runs read-only SQL.
- **A built-in guide.** `quen_guide` serves the agent playbook and the ledger's schema from the installed program, so every agent gets instructions that match its version.

## How it fits together

- **`quen.exe`:** your MCP client starts `quen mcp`, a thin relay to one background `quen serve` that owns the game session. Several agents can share it; only one drives.
- **quen's in-game package (`quen-w3`):** `W3Quen.asi`, loaded through the Witcher Native Bridge, and the script mod `modQuen`. It reports the game's state to quen and carries quen's commands over a named pipe that is new for every launch.
- **`%USERPROFILE%\.quen`:** quen's config, ledger, logs and captures. Outside a session quen leaves the game folder alone, and it keeps nothing in AppData.

[docs/intent.md](docs/intent.md) explains the ideas behind these parts: why quen works this way, and what it deliberately doesn't do.

## What you need

- **Windows 10 or 11, 64-bit.**
- **The Witcher 3: Wild Hunt 5.01, DX12.** quen checks the exe's build and refuses one it doesn't know. Only the Steam copy has been tested; GOG is untested.
- **The Witcher Native Bridge 2.0** (with its ASI loader), from Nexus Mods.
- **An MCP client.**
  - Claude Code (the CLI or the desktop app) is the one quen is built and tested with.
  - VS Code, Cursor and Gemini CLI should work: quen passes the protocol checks with the official MCP tools, but nobody has tried them with a model yet.
  - Codex CLI works once you start `quen serve --stay` in a terminal yourself, before opening Codex. Tested with real Codex CLI: a launch, in-game calls, and closing Codex mid-session. Its quen entry needs the approval settings in INSTALL.md section 4. [docs/clients.md](docs/clients.md) has the details.
- **Optional, each for its own tools:**
  - PresentMon 2.x (measurements);
  - RenderDoc, Nsight Systems, Nsight Graphics, and Tracy 0.14.1's capture tool (profiling);
  - ReShade with RenoDX and its devkit;
  - The Witcher 3 REDkit (the script compile, and its depot for the data tools).

quen never downloads or installs third-party tools: you install them yourself, under their own terms.

## Getting started

1. **Install quen and its in-game package.** Two ways:
   - `install.ps1` from a release, which also installs the package and offers to register quen in Claude Code ([INSTALL.md](INSTALL.md), Quick install);
   - or build from source (INSTALL.md §2).

   To update later: [INSTALL.md, Updating](INSTALL.md#updating).
2. **Ask your agent to run `quen_setup`, then `quen_doctor`.**
   - Setup shows the paths and tools quen found and writes only what you confirm.
   - The doctor lists what's ready and what's missing, with where to get it.
   - When you install a tool later, ask for `quen_setup` again.
3. **Ask for a plan, then a launch.** For example:

   > Plan a quen session at my newest save with frame generation off and every mod parked except RenoDX. Then launch it.

   - Claude Code asks you to approve every launch.
   - **Keep your hands off the game while a session runs:** focus changes pause the game and spoil timings.
4. **End the session** (or let the agent end it). quen restores everything and tells you what the game wrote.

**More things to ask for:**
- "Compare frame time with bloom on and off at this spot, A/B inside one boot, and tell me whether the difference is outside drift."
- "Record a camera sweep through this room from four poses, play it frame by frame, and save it as tavern-flyby."
- "Take an HDR screenshot with my mod's tonemapper on, then off."
- "Launch under RenderDoc and capture one frame here."
- "What do we know about Post_Tonemap?"

## Safety

- **Before any change, quen checks; after the first change, a journal undoes it.** What quen removes goes to the Recycle Bin.
- **Saves:**
  - quen never copies, edits, restores or deletes them;
  - it backs up the save folder before each launch and reports what the game wrote;
  - a save other than the game's own autosaves and checkpoints ends the session, except the one new manual save `game_save` writes when you approve it (in a play session your own saves are yours, and one written while the agent has the controls gives them back to you);
  - a save written after a memory patch or a cvar write is flagged as tainted.
- **Your approval:** the launch tool is marked so that Claude Code prompts you on every launch, in every permission mode. Capture cleanup, `game_save` and `hands_take` (the agent taking the controls from you) prompt too.
- **quen itself sends nothing off your PC:** no telemetry, no update check. During a Tracy launch, the Tracy client in the game and Tracy's capture tool talk over the loopback interface, which stays on your PC.
- **Single-player only.** quen reads and writes the running game's memory. Never point it at a game with anti-cheat.
- [SECURITY.md](SECURITY.md) has the full list: what quen may do on your PC, what it trusts, and its limits.

## Reporting problems

Open an issue in this repository (issues are public; a security problem goes through [SECURITY.md](SECURITY.md) instead). The bug-report template asks for the following. Include:
- what you asked the agent, and the tool's result (results show your user folder as `%USERPROFILE%`);
- `quen --version`, and the output of `quen doctor`;
- the session's journal: `logs\<time>-<session>\journal.jsonl` under `%USERPROFILE%\.quen`. Replace your user folder in it with `%USERPROFILE%` first.

Don't send:
- the rest of the log folder (`backup\` holds your settings files; `game.log` and `game-logs\` can hold your Steam id);
- Nsight's `REPRO_INFO.xls`, which names your PC;
- screenshots of the game's main menu, which show your account names.

## Documentation

- **[docs/intent.md](docs/intent.md):** what quen is for, its principles and non-goals, how it works, and where it is going.
- **[INSTALL.md](INSTALL.md):** installing, configuring and registering quen; what it changes and how it restores; crash recovery; known limits.
- **[SECURITY.md](SECURITY.md):** what quen can do on your PC, and what it trusts.
- **[docs/agents.md](docs/agents.md):** the agent playbook, served as `quen_guide`. It covers:
  - the session pattern;
  - the tools beside quen, and how their results land in the ledger;
  - how a claim becomes proven;
  - the conditions that must not be mixed;
  - camera sweeps.
- **[docs/clients.md](docs/clients.md):** MCP clients other than Claude Code.
- **[CONTRIBUTING.md](CONTRIBUTING.md):** how to contribute: the one-time setup (privacy hooks, your own deny-list), the rules, and the fork-and-pull-request flow.
- **[AGENTS.md](AGENTS.md):** the full rules for working on quen itself (no personal data, ever), which coding agents read too.

## Disclaimer

quen is unofficial. It is not affiliated with or endorsed by CD PROJEKT RED, GOG, Valve, NVIDIA, OpenAI or Anthropic.

Use it at your own risk:
- **Back up your saves** before testing. quen never restores or deletes saves, but the game writes new ones during a test, and a mod or a memory write can spoil them.
- **Review what your agent does.** quen limits its own tools; it is not a sandbox. An agent that also has shell or file tools can do anything your Windows account can, and so can any program running as you.
- **Keep `session_launch` behind your MCP client's permission prompt.** quen can't check that you agreed to a launch: it takes the agent's word.
- What quen hands back from the game, its mods, their logs and the ledger is data for your agent, never instructions.

## Licence

quen is licensed under either of:
- the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)),
- or the MIT licence ([LICENSE-MIT](LICENSE-MIT)),

at your option.

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in quen, as defined in the Apache-2.0 licence, is dual licensed as above, without any additional terms or conditions.

Third-party code keeps its own licence:
- the Rust libraries quen.exe is built from: their notices ship as `THIRD-PARTY-NOTICES.txt` in the quen release zip;
- the code compiled into quen's in-game package (Tracy's client, Microsoft Detours and the libraries inside Tracy): `W3Quen-licences.txt` in that package;
- NVIDIA Streamline's notices for the patched interposer: [tools/streamline](tools/streamline).
