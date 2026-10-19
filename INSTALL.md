# Installing quen

quen lets a coding agent (Claude Code, or another MCP client) drive one Witcher 3 session at a time for mod testing: launch with settings overrides and mods parked, load saves, take screenshots, run PresentMon A/B measurements, and record everything in a local database. Whatever happens, it puts the game folder back.

> **Early access.** quen is in early development, and so is this guide.
>
> quen runs on its own: it needs no Python. Sections that still differ from what's described say so.

## Quick install

`install.ps1`, a file of each GitHub release beside its zips, installs quen and its in-game package in one step. It runs without admin rights and installs no third-party tool: the Native Bridge, PresentMon and the rest stay yours to install (section 1).

**Download it, read it, run it** (recommended). In Windows PowerShell:

```powershell
Invoke-WebRequest https://github.com/snackbar92hypernova/quen-mcp/releases/latest/download/install.ps1 -OutFile install.ps1
notepad install.ps1
powershell -ExecutionPolicy Bypass -File install.ps1
```

**Or in one line.** It runs the script without saving it, and takes no options:

```powershell
irm https://github.com/snackbar92hypernova/quen-mcp/releases/latest/download/install.ps1 | iex
```

What it does, with every check before the first change:
1. Downloads the latest release (`-Version <x.y.z>` for another) into a new temp folder, or uses the release files in `-From <folder>`. It checks every zip against the release's `SHA256SUMS.txt` before opening it.
2. Puts `quen.exe` in `%USERPROFILE%\.quen\bin`. It offers to add that folder to your user PATH, and, when `claude` is on your PATH, to register quen in Claude Code (`claude mcp add --scope user quen -- <path> mcp`). It prints the lines for VS Code and Cursor (section 4).
3. Installs quen's in-game package into the game folder `quen setup` reports, or `-GameDir <folder>` (`-NoGamePackage` skips it). It refuses while the game runs or the Native Bridge is missing, lists each file it will place or move, and asks. An old `Mods\modQuen`, any `Mods\modQuenRpc` and an old `W3Quen.asi` go to the Recycle Bin.
4. With `-WithRenderDoc`, unzips quen's Streamline dev interposer into `%USERPROFILE%\.quen\tools\streamline`.
5. Runs `quen doctor` and prints what is missing, with where to get it.

Then ask your agent to run `quen_setup`, then `quen_doctor` (section 3).

- It refuses, and changes nothing, while a quen session is live or its journal is left: end the session, or run `quen recover`.
- **`-Update`** replaces an installed `quen.exe` (and, with `-WithRenderDoc`, the interposer) with the release's; without it, the script refuses to. A `quen.exe` running from `.quen\bin` is stopped first, after you agree, when no session is live. Then restart Claude Desktop (its MCP dialog has no Reconnect for user-scope servers), or in the Claude Code CLI reconnect quen with `/mcp`: a client session keeps the tool list it got when it connected. `session_status` gives the running quen's version (`quen`).
- **`-Uninstall`** sends `.quen\bin` and the package to the Recycle Bin, and takes quen out of Claude Code (when `claude` is on your PATH) and your user PATH, after one confirmation. The rest of `%USERPROFILE%\.quen` (config, ledger, logs, captures) stays.
- **`-WhatIf`** prints every step and changes nothing. It still downloads and checks the release, in a temp folder that then goes to the Recycle Bin.

## 1. What you need

**Windows 10 or 11, 64-bit.** quen runs only on Windows.

**The Witcher 3: Wild Hunt, game version 5.01, DX12** (`bin\x64_dx12\witcher3.exe`).
- quen starts `witcher3.exe` directly, not through Steam. With Steam, set the game's *Properties → Updates* to "Only update this game when I launch it", so the game stays on the build you test against.
- **quen checks the build itself.** Before a launch changes anything, it hashes `witcher3.exe`: a build it doesn't know (a game update, another store's copy) is refused, and the reason gives the exe's sha256 and its PE stamp. quen knows Steam's 5.01. To test another build anyway, the agent passes `allow_unknown_build` for that launch (the boot's record says so); once you have checked a build works, you can add it to the config's `known_builds` (§3).
- Only the Steam copy has been used so far. GOG is untested: its exe counts as unknown until someone verifies it there.

**An MCP client** to talk to quen: Claude Code (the CLI or the desktop app) is the one quen is built and tested with. VS Code, Cursor and Gemini CLI should work: quen passes protocol checks with the official MCP tools, but nobody has tried them with a model yet. Codex CLI needs `quen serve --stay` started in a terminal first (section 4). What each client does with quen's long calls: [docs/clients.md](docs/clients.md).

**Rust and Git, to build quen.**
- Rust: install rustup from <https://rustup.rs>. On Windows it also needs the Microsoft C++ build tools ("Desktop development with C++"); the rustup installer offers them. The repo pins its Rust version (`rust-toolchain.toml`), and rustup downloads it on the first build.
- Git: <https://git-scm.com>.
- Release binaries that need no Rust: none yet. TODO: link the Releases page once there is one.

**PresentMon 2.x, the console application**, for `measure_presentmon` and `measure_ab` only.
- Intel's frame-timing tool: <https://github.com/GameTechDev/PresentMon/releases>. It isn't bundled with quen. Tested with 2.4.1.
- The installer puts it at `C:\Program Files\Intel\PresentMon\PresentMonConsoleApplication\PresentMon-<version>-x64.exe`, where quen finds it. Elsewhere, name its full path in the config (§3): a `PresentMon.exe` on your PATH is quen's last resort ([SECURITY.md](SECURITY.md), what quen trusts).
- **Permission:** quen runs PresentMon without admin rights. PresentMon reads Windows' event tracing, so your account must be in the *Performance Log Users* group. Run once in a terminal opened as administrator, then sign out and back in:

  ```bat
  net localgroup "Performance Log Users" "%USERNAME%" /add
  ```

  Without it, PresentMon stops at once with an access-denied error, which the measure tools report.

**The in-game parts quen relies on** (Witcher 3 mods):

| Part | Files | What quen uses it for | Where to get it |
|---|---|---|---|
| Witcher Native Bridge 2.0 | `bin\x64_dx12\WitcherNativeBridge.asi` and the ASI loader in its download (`dinput8.dll`; an Ultimate ASI Loader named `winmm.dll` works too) | lets the scripts of quen's package call its ASI | public, on Nexus Mods. Needs the Microsoft Visual C++ Redistributable (x64). |
| quen's in-game package | `bin\x64_dx12\W3Quen.asi`, `Mods\modQuen` | tells quen whether the game is in gameplay, a menu, a loading screen or photo mode; loads a save by name (`game_load`, and a launch of a save that isn't the newest); later the command channel into the game | `quen-w3-<version>.zip` from the GitHub Releases of quen-mcp. Unzip it into the game folder. |

- Both are needed for every launch.
- quen doesn't need the W3 Game State Bridge. If another mod of yours uses it (RenoDX does), leave it installed: quen reads nothing of it, and parks it only when a launch names both of its parts (`W3GameStateBridge.asi,modW3GameStateBridge`), never one alone: one without the other breaks the game's script compile.
- quen never parks the Native Bridge or its own package, even when a launch parks everything else.
- **quen's package is checked exactly:** both files must come from the one release this quen expects. quen refuses a launch while only half of it is there, while the two files are from different releases, or while a part of the old layout is left: move an old `Mods\modQuen` (the pure-script one) and any `Mods\modQuenRpc` to the Recycle Bin before you unzip the package.
- After installing them, start the game once on its own. The first start after adding script mods recompiles the game's scripts and takes longer.
- If the game shows "errors compiling scripts", a script mod doesn't fit the game version.
- To uninstall, remove the `Mods` folders first, then the `.asi` files. The other order leaves script mods calling a bridge that isn't there, and the game shows that same error box.

**Optional:** ReShade with RenoDX, the RenoDX devkit, Nsight Systems, Nsight Graphics, RenderDoc, and Tracy's capture tool. Each stays off until its key is in the config (§3: `quen_setup` finds the ones an installer put in place, and writes a key once you confirm it; Tracy's capture tool comes as a zip, so you name its path); then a launch can ask for it. Tracy needs version 0.14.1 exactly, `tracy-capture.exe` from Tracy's v0.14.1 release (<https://github.com/wolfpld/tracy/releases>): it must match the Tracy client inside quen's package from quen-w3 0.1.3, which runs only in a `tracy` launch. Once the devkit is configured it loads by default (ReShade loads it; the RenoDX add-on isn't needed for it); `devkit: false` leaves it out, for timing runs.

## 2. Build and install

In a terminal:

```bat
git clone <the repo URL from your invitation> quen-mcp
cd quen-mcp
cargo install --path crates/quen
```

This builds quen and puts it at `%USERPROFILE%\.cargo\bin\quen.exe`. rustup has already put that folder on your PATH. Check it by running `quen`: it prints its usage.

**To update:** `git pull`, then the same `cargo install` line. Close the MCP clients that use quen first, and wait until no `quen.exe` runs (Task Manager): Windows can't replace a running one.

## 3. Configure: ask your agent

Once quen is registered (section 4), ask your agent to run **`quen_setup`**, then **`quen_doctor`**. There is nothing to edit by hand.

- **`quen_setup`** lists every path and tool quen uses, each with where it came from:
  - Windows defaults for quen's own folders: its config file, its data root (section 5) and where captures go;
  - the game folder, found from Steam's libraries or GOG's install entries;
  - the documents folder, from Windows' own Documents location, so a Documents folder kept in OneDrive is found too;
  - PresentMon, Nsight Systems, Nsight Graphics and RenderDoc, where their installers put them under Program Files, or on your PATH. Every installed version is listed;
  - ReShade, the RenoDX add-on and the RenoDX devkit, in the game's `bin\x64_dx12` folder;
  - the RE knowledge base and the Streamline dev interposer only when you name them: quen doesn't search for them;
  - or your config.

  Each one has a status: configured; configured but missing now; found but not configured; or not found, with where to get it. The agent shows you what it found once and asks whether it's right; quen then writes your answers to its config. With two copies of the game (say, two versions), or two versions of a tool, it asks which one: quen never picks. A GOG copy is marked untested: quen and its in-game parts are checked on the Steam version only, and `quen_doctor` reports a GOG exe as a build quen doesn't know.
- **Every value is checked before anything is written,** as `quen_doctor` checks it. A path must be the right kind of file: `nsys.exe`, `ngfx.exe`, `renderdoccmd.exe`, PresentMon 2.x, the game folder, a knowledge-base folder holding its tables. The Streamline dev interposer must not be NVIDIA's signed build, `ngfx_args` must not hold `--platform`, `known_builds` must read as builds, and `capture_budget_gb` must be a number above 0. If any value fails, nothing is written.
- **When you install a tool later,** tell your agent and ask it to run `quen_setup` again. The new tool shows as found but not configured; once you confirm it, quen writes it to the config.
- **No reconnect.** Once `quen_setup` has written the config, every client session connected to quen uses it at once. While a game session runs, quen keeps the old config until that session ends, then takes up the new one. The result says which happened.
- **`quen_doctor`** lists everything quen depends on (section 1): found, missing or the wrong version, what each one is for, and where to get a missing one. It changes nothing, and quen installs nothing ([SECURITY.md](SECURITY.md)).
- **When a tool refuses** because something is missing, its result has a `needs` list: what's missing, what needs it and where to get it. The agent tells you, or offers another way (an SDR screenshot when HDR is off, for example).
- **In a terminal:** `quen setup` and `quen doctor` print the same results. `quen setup game_dir=<folder> --user-ok` writes a value.

### The config file, for reference

quen reads `%USERPROFILE%\.quen\config.toml`. Set the `QUEN_CONFIG` environment variable to use another file. Not AppData: Claude Desktop is a packaged (MSIX) app, and Windows redirects AppData writes from it, and from every program it starts, into the app's private folder, which other clients and your own tools don't see. An older quen kept its files there: `quen_doctor` says so, and `quen_setup` with `move` (or `quen setup --move`, then `--move --user-ok`) copies them into `.quen` once, checks every copy, and renames the old folders `quen.moved-<date>`. `quen_setup` writes it; you can also edit it yourself. A key in this file counts as confirmed: `quen_setup` lists the others as still to confirm.

The format is a small part of TOML:
- one `key = 'value'` per line, and `#` starts a comment;
- single quotes around paths, so backslashes stay as they are;
- **real, absolute paths**: quen expands no `%VARIABLES%`, so write them out;
- an unknown key is refused, so a typo can't go unnoticed.

A complete example, as `quen_setup` writes it (comments are yours: it keeps them). Replace every `<...>`:

```toml
# quen's local config: %USERPROFILE%\.quen\config.toml

# The game folder: the one that holds bin\ and Mods\.
game_dir = 'C:\Program Files (x86)\Steam\steamapps\common\The Witcher 3'

# The game's documents folder: dx12user.settings and gamesaves\.
# If Windows keeps your Documents in OneDrive, use that path.
documents = 'C:\Users\<you>\Documents\The Witcher 3'

# PresentMon's console exe, for measure_presentmon and measure_ab.
# Optional where its installer put it; elsewhere a full path here is safer than PATH.
presentmon = 'C:\Program Files\Intel\PresentMon\PresentMonConsoleApplication\PresentMon-<version>-x64.exe'

# Optional: the game's exe name in bin\x64_dx12. Default witcher3.exe.
# game_exe = 'witcher3.exe'

# Optional: where quen keeps what it produces (section 5). Default %USERPROFILE%\.quen.
# data_root = '<drive>:\quen-data'

# Optional: where captures go, one folder per session. Default <data_root>\captures.
# capture_root = '<drive>:\quen-data\captures'

# Optional: GB of captures before quen warns you (quen_doctor, session_plan, session_launch, session_end). Default 20.
# capture_budget_gb = '20'

# Optional: game builds you have verified, beside the ones quen knows (Steam 5.01). Each entry is
# <platform> <sha256 of witcher3.exe> <label>, platform steam or gog, ';' between entries. A refused launch's reason
# gives the entry to add. Listing a build quen already knows under another label is refused.
# known_builds = 'steam <64 hex digits> <label, e.g. 5.02 (Steam build <buildid>)>'

# Optional: the RE knowledge base (witcher3-mods\re), for game_cvar and the mem_* tools. While its names.tsv and
# hooks.tsv name a build (a '# build: sha256=... stamp=0x...' line), those tools refuse another build's exe.
# kb = '<folder>\witcher3-mods\re'

# Optional features, each off until set.
# RenoDX: its add-on's file name in bin\x64_dx12. Launches then patch ReShade.ini (DevAutoReload, preset 1's
# values) and ReShadePreset.ini (no ReShade effects), and wait for the add-on's line in ReShade.log.
# renodx_addon = 'renodx-thewitcher3remastered.addon64'
# With it, a launch is refused while the installed add-on differs from this build.
# renodx_build = '<folder>\renodx-thewitcher3remastered.addon64'
# The RenoDX devkit, as ReShade.ini's [ADDON] LoadFromDllMain takes it.
# devkit = 'devkit\renodx-devkit.addon64'
# Profilers, for profiler: 'renderdoc:NAME', 'nsys:NAME' or 'ngfx:NAME'. ngfx_args go to every ngfx
# launch; quote a value with spaces (never --platform: ngfx then fails to start).
# nsys = '<folder>\nsys.exe'
# ngfx = '<folder>\ngfx.exe'
# ngfx_args = '--architecture "<your GPU architecture>" --metric-set-id 0'
# RenderDoc needs both: renderdoccmd.exe, and a patched dev build of Streamline's interposer, which the
# launch swaps in for the game's signed one and swaps back. Download quen's build
# (quen-streamline-dev-interposer-2.14.1.zip from quen-mcp's GitHub Releases), or build it yourself:
# tools\streamline\README.md. quen doctor tells the two apart.
# renderdoc = 'C:\Program Files\RenderDoc\renderdoccmd.exe'
# streamline_dev_interposer = '<folder>\_artifacts\sl.interposer\Develop_x64\sl.interposer.dll'
# Tracy, for profiler: 'tracy:NAME': Tracy 0.14.1's capture tool (quen doctor checks its version). A tracy
# launch also needs quen-w3 0.1.3 or later in the game and kb above: the KB's hooks.tsv says which mods to park.
# tracy_capture = '<folder>\tracy-capture.exe'
```

GPU Trace (`ngfx`) needs the GPU performance counters open to all users. That's a system setting only you change, in the NVIDIA Control Panel: Desktop > Enable Developer Settings, then Developer > Manage GPU Performance Counters > Allow access to all users. `quen doctor` shows whether it's on.

quen takes up an edited config, whether you, `quen setup` or `quen_setup` changed it, at its next client connection or session tool call while no game session runs. During a game session it keeps the old config until the session ends. A config that doesn't load stops quen's session tools until you fix it; `quen_doctor` names the error.

## 4. Register quen in your MCP client

Every client starts `quen mcp` over stdio. If `quen` isn't on your PATH, give its full path (in JSON, with every backslash doubled). Long calls: a launch takes about 90 s, a save load about 33 s, `measure_ab` minutes. Claude Code waits for them. For any other client quen answers a call that runs past 20 s with `{job, state}`, and the agent fetches the result with `job_status` (it returns the call's own result), so a client's short tool timeout doesn't cut a launch off. `QUEN_JOB_BUDGET_S` in the server's `env` sets another time for that client: seconds, well below its timeout, or `0` to always wait. Details per client: [docs/clients.md](docs/clients.md).

**Claude Code.** Once, in a terminal:

```bat
claude mcp add --scope user quen -- quen mcp
```

`--scope user` makes quen available in every project. Check it with `claude mcp list`, or `/mcp` inside a session. If you'd rather edit the file yourself, add this to `mcpServers` in `%USERPROFILE%\.claude.json`:

```json
"quen": { "type": "stdio", "command": "quen", "args": ["mcp"] }
```

Nothing to set for time: its tool timeout is about 28 hours.

**Codex CLI: start `quen serve --stay` first.** Codex runs each MCP server inside a Windows job that closes everything when Codex closes, and won't let quen's background `quen serve` leave it, so quen won't start it there: closing Codex would kill the game mid-save. Start it yourself instead, before opening Codex:

1. Open a terminal (PowerShell or Windows Terminal) and run `quen serve --stay`. It prints one line with its pid and says Ctrl+C ends it. Leave that terminal open while you use Codex.
2. Open Codex. Its `quen mcp` connects to that `quen serve`.
3. When you're done, press Ctrl+C in that terminal: a live session ends the normal way, then `quen serve` exits. Don't close the terminal window during a session: Windows gives a closing console only 5 s.

The terminal must run the same `quen.exe` as Codex's `command` (both ends check). Tested with Codex CLI 0.163.0-alpha.10.2: a launch, in-game calls, `session_end`, and closing Codex mid-session. Details: [docs/clients.md](docs/clients.md#clients-that-hold-servers-in-a-job-object). Codex's entry, in `%USERPROFILE%\.codex\config.toml`:

```toml
[mcp_servers.quen]
command = "quen"
args = ["mcp"]
startup_timeout_sec = 30
tool_timeout_sec = 120
default_tools_approval_mode = "approve"

[mcp_servers.quen.tools.session_launch]
approval_mode = "prompt"

[mcp_servers.quen.tools.game_save]
approval_mode = "prompt"

[mcp_servers.quen.tools.hands_take]
approval_mode = "prompt"

[mcp_servers.quen.tools.capture_clean]
approval_mode = "prompt"
```

- `tool_timeout_sec`: quen answers within about 20 s (then `job_status`), so 120 leaves room. Codex's default is 60 or 300 s depending on the version.
- **Approvals.** Without `default_tools_approval_mode`, Codex asks before every quen tool that changes something (quen 0.1.8 marks only its read-only tools, which run without asking), so during a launch you would switch from the game to Codex for each call. With the entry above, Codex asks only for the four calls Claude Code also asks for every time: `session_launch`, `game_save`, `hands_take` and `capture_clean`, each with an approval card (the launch's was tried on 2026-10-19; the other three use the same setting). Use a Codex permission mode that asks, not Full access: there Codex refuses every tool that needs approval, including the launch.
- Codex passes only a short list of environment variables to servers. If you set `QUEN_CONFIG`, add `env_vars = ["QUEN_CONFIG"]`, or `env = { QUEN_CONFIG = '<path>' }`, and set the same `QUEN_CONFIG` in the terminal that runs `quen serve --stay`: with another config, Codex's `quen mcp` looks for another `quen serve`.

**VS Code (Copilot agent mode).** In your user `mcp.json` (Command Palette: *MCP: Open User Configuration*) or a workspace's `.vscode\mcp.json`:

```json
{
  "servers": {
    "quen": { "type": "stdio", "command": "quen", "args": ["mcp"] }
  }
}
```

VS Code has no tool timeout, so nothing to set; long calls still come back as jobs after 20 s unless you add `"env": { "QUEN_JOB_BUDGET_S": "0" }`.

**Cursor.** In `%USERPROFILE%\.cursor\mcp.json` (all projects) or a project's `.cursor\mcp.json`:

```json
{
  "mcpServers": {
    "quen": { "command": "quen", "args": ["mcp"] }
  }
}
```

Cursor's timeout can't be changed: about 60 minutes in the editor, 60 s in its CLI (`agent`). The 20 s jobs cover both.

**ChatGPT (the web and desktop apps) can't use quen.** They only reach MCP servers on the internet (a remote server over HTTPS), not a program on your PC. quen deliberately has no remote server: it drives a game on your PC.

**Permissions.** Your client asks you before every launch: quen marks `session_launch` so that Claude Code (2.1.214 or newer) shows its permission prompt each time, in every permission mode, with no "always allow". In another client, keep `session_launch` on the permission prompt: allow it once each time it asks, never "always allow". The agent must ask you before a launch (`user_ok`), but quen can't check that it did: the prompt is your check. `session_plan`, which only checks a launch, doesn't ask. What else quen can do on your PC, and what it trusts: [SECURITY.md](SECURITY.md).

**Claude Code's auto mode** may refuse a quen tool in the middle of a session (seen 2026-10-19: `mem_patch` as "Modify Shared Resources", `mem_read` as "Security Weaken"). An allow rule in `%USERPROFILE%\.claude\settings.json`, under `permissions.allow`, names a tool that may run: the rule for quen's tool `<tool>` is `mcp__quen__<tool>`, for example `{"permissions": {"allow": ["mcp__quen__mem_read"]}}`. Which tools you allow is your decision; quen doesn't read Claude Code's settings. The tools on the game's memory:
- **Only read it:** `mem_read`, `mem_scan`, `mem_follow`, `mem_walk`, `mem_find`, `mem_regions`.
- **Write it:** `mem_patch`, `mem_unpatch` (journaled; every save the game writes afterwards is marked tainted).
- **Both:** `game_cvar` reads a cvar, and with `set` or `restore` writes it. One rule, `mcp__quen__game_cvar`, covers both.
- `session_launch`, `game_save`, `capture_clean` and `hands_take` prompt on every call, whatever the rules say.

**What runs.** `quen mcp` only relays messages. The first one starts `quen serve` in the background, which holds the game lock, the session and its journal. Several client sessions share that one `quen serve`. It exits a few seconds after the last of them closes. If a game session is still running then, it first ends it (waits for saves to finish, closes the game, puts the folder back), so it can outlive the client by a little while.

**Who drives.** Only one agent drives the game at a time. The launch returns a token, and only the connection that launched the game can end it or act in it. Closing that client session, or reconnecting quen (`/mcp` in Claude Code), ends the game session the normal way: wait for saves to finish, close the game, put the folder back.

**Keyboard layouts.** Any Windows keyboard layout works, and there is nothing to set.
- The Witcher 3 reads most keys by their physical position on a US keyboard, whatever layout Windows uses. quen types the same way: by physical position, not by the characters printed on your keys.
- The console key is the exception. The game opens its console with the key Windows calls `VK_OEM_3`: `~` on a US layout, possibly somewhere else on yours. quen uses the game's own console binding if it can read it, else wherever your layout puts that key.
- quen can type into the console only characters a US keyboard has. A line holding any other character, an accented letter for example, is refused before anything is typed. That includes save names passed to `game_load`.

**First steps.** Ask your agent to run `quen_setup` and `quen_doctor` (section 3), then quen's `session_status`. None of them changes anything until you agree to the paths. Then ask it to run `session_plan`, which checks a launch without touching anything. A real launch (`session_launch`) needs `user_ok: true` and your OK at the permission prompt (Permissions, above). The agent's guide comes from quen itself: its `quen_guide` tool.

## 5. The data root

Everything quen produces lives under the data root, by default `%USERPROFILE%\.quen\` (beside its config), never in the repo or the game folder:

| Path | What |
|---|---|
| `state\game.lock` | the lock: one driver at a time |
| `state\journal.jsonl` | the live session's journal: every change, written before it is made, and how to undo it |
| `state\quen.db` | the ledger (SQLite): games, boots, saves, mod sets, captures, measurements |
| `gamesaves\` | a copy of your `gamesaves` folder, refreshed before every launch |
| `logs\<time>-<session>\` | one folder per session: the backups of the files it patched (`backup\`), the game's log, a numbered log per tool step (PresentMon's too), the finished journal, and `game-logs\`: copies of the game folder's `*.log` files the boot wrote (quen's, the Native Bridge's, ReShade's, the mods'), which the next launch would overwrite. They can hold personal data (a Steam id, the PC's name): keep them on this PC |
| `logs\serve.log` | the background `quen serve` log (a `quen serve --stay` writes to its terminal instead) |
| `logs\queries\` | `ledger_query` results too big to answer inline |
| `state\backups\` | a copy of the ledger from before each upgrade of its schema |
| `captures\<session>\` | screenshots, PresentMon CSVs and memory-tool results too big to answer inline (or under `capture_root`) |
| `captures\<boot id>\` | a profiler's output: RenderDoc captures (3-5 GB each), Nsight reports and traces, Tracy captures |
| `_to_delete\<date>\` | from quen 0.1.5 and older: captures you agreed to clean up that the Recycle Bin couldn't take whole. A cleanup takes them now, with your OK |

- quen refuses a data or capture root inside its own source folder, its install folder, any git work tree, the game folder, the game's documents folder, or Pictures.
- **Captures grow with every session**, and quen tells you: `quen_doctor`, `session_plan`, `session_launch` and `session_end` report the capture root's size, and warn once it passes `capture_budget_gb` (20 GB unless your config says). Your agent can show what is there (`capture_usage`: by boot, kind and age) and, only with your OK, clean up what you pick (`capture_clean`; your client asks you each time). Files go to the Recycle Bin. For one the bin can't take whole (bigger than the bin, or no bin on that drive), Windows' own dialog asks you whether to delete it permanently: Yes deletes it, No keeps it where it is. quen never deletes a file for good without that answer. The ledger keeps every row, marked removed. A file a claim or a pose relies on stays unless named.
- **The same cleanup takes quen's other leftovers** under the data root, when you pick them: the session logs (with the settings backups a session kept), `logs\queries\`, the ledger's backups in `state\backups\`, zips in `packages\` other than the installed package's, and what waits in `_to_delete\`. Your config, the ledger, `logs\serve.log` and the `gamesaves\` copy are never taken.

## 6. What quen changes, and how it puts it back

**During a session, quen may change:**
- `dx12user.settings` in the documents folder: frame generation off (unless the launch keeps it), camera auto-rotate off, the launch's settings overrides, and VSync off when asked;
- `bin\config\base\general.ini`: the developer console is turned on, so quen can type console commands (`game_load` types modQuen's);
- `ReShade.ini` in `bin\x64_dx12`, with RenoDX or the devkit configured, and `ReShadePreset.ini`, with RenoDX configured;
- `steam_appid.txt` beside `witcher3.exe`, only if it is missing: it keeps Steam from restarting the game outside quen's control;
- parked mods: moved out of `Mods\` and `bin\x64_dx12\` into `quen-parked\Mods\` and `quen-parked\bin\` in the game folder (the journal lists them);
- staged test files (a launch's `stage`: an ASI, its ini, a DLL from elsewhere on your PC): copied into `bin\x64_dx12` for the session, a file of the same name there parked first. At the end the copies go to the Recycle Bin, with the `<name>.log` a staged mod wrote where none was (after a copy into the session's logs), and the parked files come back;
- `content\content0\x64.final.redscripts`, the game's compiled scripts: with a mod parked the game recompiles them at launch, so quen backs the file up first and puts it back afterwards with its old write time;
- the running game's memory: cvar writes and ASI mode switches. These live only in the game process and are gone when it closes;
- `gamesaves` in the documents folder, only through `game_save` (quen-w3 0.1.4 or later), which your client asks you about on every call: the game writes one new manual save, as its menu's "Empty save slot" does, and names it. It never overwrites a save. quen checks the folder before and after: anything else that changed ends the session.

**While a session is live,** quen keeps the PC and its display from sleeping. That ends with the session, however it ends.

**In a test session the agent can hand you the controls** (`hands_give`), to set something up in the game, and takes them back after you agree (`hands_take`, Claude Code asks you each time). While you have them quen sends no key and writes nothing to the game, and a save you make is yours: the end lists it.

**A play session** (you play, and hand the agent the controls when you want it to look at something): quen changes only `general.ini`'s console line, and places `steam_appid.txt` if it is missing. Your `dx12user.settings`, `input.settings` and ReShade's ini stay as they are, unless you ask the agent for a settings override, the devkit, or frame generation or VSync off. It parks nothing.
- The agent sends no key and writes nothing to the game until you ask it in the chat and approve the hand-over (Claude Code asks you each time); you get the controls back with `hands_give`. With your OK it may take screenshots, record frame times or read the game's memory while you play.
- Your saves stay yours; the end lists them. A save written while the agent has the controls gives them back to you.
- After a memory write, cvar write, teleport, time or weather change, every save the game writes in that session is marked tainted in quen's ledger.
- The session ends when you quit the game: quen then puts the console line and `steam_appid.txt` back by itself. Closing the agent's chat leaves the game running; if the agent had the controls then, quen first undoes its memory and cvar writes of that hand-over, except the ones you agreed to keep. If `quen serve` itself exits (a crash, an update that stops it), the game closes with it.

**How it puts things back:**
- Before each change, quen writes the step to its journal, with what it takes to undo it.
- `session_end` waits until the game has stopped writing saves, closes the game (only the processes quen started, never another copy), then undoes the journal from the last step back:
  - a file is restored from its backup, after checking the backup's hash;
  - a moved mod goes back only if it is still the same file, and never over a new copy in its place;
  - a file quen placed goes to the Recycle Bin, and so do the spent backups.
- If the game rewrote a file quen patched, or a key was rebound in the game's menu (`input.settings`), quen keeps that version beside it as `<name>.quen-current` (`<name>.quen-current-2`, `-3`, ... when that name holds another version) and says so. Neither version is lost, and quen leaves the copy for you to check: `quen doctor` names each one in its `warnings` until you move or delete it. The compiled scripts and `dx12user.settings.bak`, which the game rebuilds from quen's changes, are put back without a copy.
- **Your saves are never restored or deleted.** Saves the game writes during a test stay, and so do the ones `game_save` made. `session_end` lists them, and "tainted" ones come first: written after a memory or cvar change, so they may carry the change. Its `saves.quen` names the ones `game_save` made, and `quen doctor`'s `quen_saves` lists every one of those still in `gamesaves`.

## 7. After a crash

- **quen crashed or was killed:** the game closes with it. The next time a client starts quen, quen undoes the journal first (the startup replay) and the folder is back. You can also ask the agent to call `session_recover`.
- **The PC crashed or lost power, or Windows restarted or logged you off during a session:** the same, at the next start. Until then the game folder stays as the session left it (mods parked, the console on, frame generation and VSync off). Before you start the game from Steam, run `quen recover` in a terminal: it undoes the journal as the startup replay would and prints the result. It refuses while another quen holds the game lock.
- **Check:** `session_status` shows who holds the lock, whether a journal is live, which mods are parked, and each patched file beside its backup.
- **If an undo fails**, the journal stays in `state\`, and the result's `replay.left` names each step it left, and why. Nothing is deleted. Fix the cause and call `session_recover` again. Two common causes:
  - a `witcher3.exe` you started yourself is running: quen undoes nothing while one runs outside its control;
  - a file is open in another program.
- **By hand, as a last resort:** `state\journal.jsonl` lists every change: each moved mod with its old and new place, and each patched file with its backup. Put them back in reverse order, then send the journal to the Recycle Bin.

## 8. Known limits

- **Early development.** Built and tested on one PC with one GPU and one display. Expect rough edges. When something goes wrong, keep the tool's result and the session's journal (`logs\<time>-<session>\journal.jsonl` in the data root), and send both.
  - Results show paths under your user folder as `%USERPROFILE%\…`. The journal holds them in full: replace your user folder in it with `%USERPROFILE%` before you send it.
  - Don't send the rest of the log folder: `backup\` holds your settings files, and `game.log` and the `game-logs\` copies can hold your Steam id.
- **Config keys may still change.**
- **A check still pending:** whether a window grab gives the same values as the game's own frame, which `game_screenshot`'s HDR mode depends on. Until it passes, treat HDR screenshot values as untested.
- **One game:** The Witcher 3 5.01, DX12. One session at a time, on Windows only.
- **PresentMon:**
  - with frame generation on, every present counts, generated frames included. `presentmon_flags: ["--track_pc_latency"]` adds the column that tells them apart to the CSV, but quen's numbers don't use it yet;
  - a frame-rate cap or VSync hides frame-time changes, so measure below the cap with `vsync: false`;
  - numbers taken under a profiler (Nsight) aren't comparable with plain ones;
  - builds and analysis tools running at the same time skew CPU-bound numbers. quen warns about compilers, cargo and java, and refuses with `strict: true`.
- **Ending a session ends the game:** closing the client session that launched it, or reconnecting quen, ends the game session. A play session is the exception: the game runs on, and the session ends when you quit it.
