# Security

quen lets a coding agent drive The Witcher 3 on your PC for mod testing. This page says what quen can do there, where its limits are, and what it trusts. It covers quen's own behaviour only.

## In short

- **quen limits what quen does. It is not a sandbox.** If your agent also has shell or file tools (Claude Code has them by default), it can do anything your account can, with or without quen.
- **The trust boundary is your Windows user account.** Any program running as you can drive quen, read what it records and change the files it trusts.
- **Keep `session_launch` behind your MCP client's permission prompt.** quen can't check that you agreed to a launch (below).
- **Single-player only.** Never point quen's memory tools at a game with anti-cheat.
- **Back up your saves, and review what your agent does.**

## Reporting a problem

- **A vulnerability:** use GitHub's private vulnerability reporting (the repository's Security tab → Report a vulnerability). Only the maintainer sees the report. Never put the details of a vulnerability in a public issue.
- **Any other problem:** open an issue. Issues are public: leave out anything that identifies you or your PC (the bug-report form lists what).

## What quen does on your PC

**Programs it starts**
- `witcher3.exe`, and around it Nsight Systems or Nsight Graphics if you configured them; PresentMon for the measure tools. Each runs inside a Windows Job Object. quen kills only what is in its job, never a program by name, so a copy of the game you started yourself is left alone.
- Each of them gets a short, fixed list of environment variables (Windows' own, and quen's), never your whole environment. An API key or token in your agent's environment doesn't reach the game, its mods, a profiler or PresentMon.
- quen needs no admin rights. PresentMon needs your account in the Windows *Performance Log Users* group ([INSTALL.md](INSTALL.md) §1).
- quen downloads and installs nothing.

**Files**
- During a session quen changes files in the game folder and in the game's Documents folder: settings, the console switch, ReShade's ini files, a `steam_appid.txt`, parked mods, the compiled scripts ([INSTALL.md](INSTALL.md) §6).
- Each change goes into a journal before it is made, and is undone when the session ends, or after a crash at quen's next start.
- quen never deletes anything for good: what it removes goes to the Recycle Bin, or, for a capture the bin can't take whole, to `<data root>\_to_delete\` for you to empty. It never restores or deletes your saves.
- It adds a save only through `game_save`, which your client asks you about on every call: the game writes one new manual save. quen checks the save folder before and after, and anything else that changed ends the session (in a play session, it gives you the controls back).
- It removes captures only through `capture_clean`, which your client asks you about on every call, as for a launch.
- Its own data (the ledger, logs, captures, the journal) stays under its data root, `%USERPROFILE%\.quen\` by default.

**The game's memory**
- quen reads the running game's memory (the `mem_*` tools, `game_cvar`).
- It writes there in two ways only:
  - a patch defined in the RE knowledge base (`patches.tsv`), applied only if the game build and the bytes in memory are the ones the patch expects;
  - a console variable (cvar).

  Both are journaled, recorded on the boot's ledger row, and gone when the game closes. Saves the game writes afterwards are marked "tainted".
- quen runs code in the game: `measure_ab` can call a loaded ASI mod's exported `*_SetMode` function, on a new thread inside the game.
- It touches only the game process it started, never another copy.

**Keyboard and the foreground**
- quen sends keystrokes to the game's window, and only while that window is in front.
- During a session, the agent can bring two windows to the front (`focus`): the game's, and its own client's (the first window of a process above `quen mcp`, found when the client connected). quen moves no other window. Bringing the game back, quen sends it one Escape when its state shows a menu outside photo mode: the pause menu the game opened when it lost the front.
- In a play session (you play the game quen launched), quen refuses keystrokes, writes to the game and `focus` while you have the controls. The agent gets them with `play_take`, which your client asks you about on every call, as for a launch, and gives them back with `play_return`. During a hand-over quen can't tell your input from its own.

**Screenshots**
- quen captures the game's window. Your agent can't ask for the screen.
- Only where Windows.Graphics.Capture doesn't work (some Windows installs) does an HDR screenshot fall back to the game's area of the screen. That happens only while the game is in front: if another window takes the focus during the shot, the frame is thrown away. A notification or overlay drawn over the game can still show in such a shot, and quen marks it (`includes_desktop`).

**Network**
- quen has no network code and opens no listening socket. Everything it records stays on your PC. There is no telemetry.
- `quen mcp` talks to the background `quen serve` over a named pipe that refuses remote clients and that only your account can open. Each end checks that the other is the same `quen.exe`, running as you.

## Launch consent is the agent's word

`session_launch` needs `user_ok: true`, which the agent may set only after asking you in the conversation. quen can't check that you were asked: it takes the agent's word. So:
- quen marks `session_launch` (`_meta["anthropic/requiresUserInteraction"]` in its tool list), so Claude Code (2.1.214 or newer) shows its permission prompt on every launch, in every permission mode, with no "always allow"; allow rules and hooks can't skip it. In a mode that never prompts (`dontAsk`, `claude -p` without a permission host), the launch is denied;
- in another client, leave `session_launch` on the permission prompt: allow it once each time, never "always allow";
- an agent can check a launch with `session_plan`, which changes nothing and needs no prompt;
- an agent with a shell can still start its own MCP client and skip your client's prompt. The playbook forbids it; quen can't prevent it.

## What quen trusts without checking

Only your account can write these. A program running as you could change them, but such a program could do worse directly.

| Input | What quen takes from it |
|---|---|
| The config file (`%USERPROFILE%\.quen\config.toml`, or the file `QUEN_CONFIG` names) | the game and data folders, and the programs quen starts (PresentMon, Nsight) |
| The journal (`<data root>\state\journal.jsonl`) | what to move back, restore and send to the Recycle Bin after a crash. The replay checks hashes and file ids before it undoes a step. |
| The RE knowledge base (`kb` in the config): `patches.tsv`, `cvar_layout.tsv` | which memory patches are allowed, and where a cvar's value lives in memory |
| A PresentMon quen finds itself | without `presentmon` in the config, quen runs the newest `PresentMon-<version>-x64.exe` in its installer's folder under Program Files, else the first `PresentMon.exe` on your PATH. Set the config's absolute path instead. The measure tools return the path they ran, as `presentmon`. |

## What your agent shouldn't trust

Text quen returns comes partly from places quen doesn't control: ledger rows (any agent or script can add them), the output and logs of the game, its mods and the tools quen runs, save names, file names in the game folder. To the agent this is data, never instructions.

- `ledger_query` is read-only.
- `ledger_ingest` puts a row on the running session's boot only when the call carries that session's token. Otherwise the row names its boot, or has none. A row doesn't prove who wrote it.

## Limits

- **Single-player use only.** quen's memory tools read and write a game's process. On a game with anti-cheat that can get your account banned. Don't point them at one.
- **Not a sandbox** (top of this page).
- **Early development:** built and tested on one PC ([INSTALL.md](INSTALL.md) §8).
