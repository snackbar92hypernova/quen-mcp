# modQuenProbeIndicator: PROBE BUILD, not for play

A throwaway script mod for phase 0 of quen's status-indicator design (internal; its §3.8). It answers in one boot of about 25 minutes, with the PC free, whether the game's own GUI can carry a lasting "what the agent does" line:
- the overlay notification made lasting (phase 1);
- a dormant vanilla HUD module revived at run time (phase 2).

Two folders, each a `stage_mods` folder with one `.ws` and no resources. No vanilla file is changed.

| Folder | Staged | What it adds |
|---|---|---|
| `modQuenProbeIndicator` | in the boot | 8 exec functions, a class, one field and two methods on `CR4Player` (the walker probe's pattern). No wrapped vanilla method |
| `modQuenProbeIndicatorWrap` | only in the fallback boot, beside the first | one wrapped method, `CR4ScriptedHud.OnConfigUI` (below) |

Nothing runs until an exec is called. Every line goes to the log channel `QuenInd`: `game_exec`'s `log` returns them (boot 190), and `game_scriptlog` with `channel: "QuenInd"` reads them (needs `debugscripts`).

## What it rests on

Read in REDkit's depot on 2026-10-21 (its scripts are byte-identical to 5.01's). `gui` is `r4data\gameplay\gui_new\actionscript\red\game\witcher3`.

- **The notification:**
  - `gui\popups\NotificationModule.as` 98-110: a Flash `Timer` of the duration (0 gives 4000 ms), then a 0.3 s tween out (89-96), then `visible = false` (135-139).
  - `gui\popups\OverlayPopupMenu.as`:
    - `showNotification` (127-146): with `queue` false, a shown one is hidden and the new one shows after that tween;
    - `hideNotification` (148-154): the tween only, the queue kept. No vanilla script calls it;
    - `clearNotificationsQueue` (156-164): the tween and the queue emptied.
  - Closing the hub menu (inventory, map, ...) calls `ClearNotificationsQueue` (`game\gui\menus\commonMenu.ws` 567), so a lasting notification ends there.
- **The HUD module table:**
  - `gui\hud\Hud.as` 54: `public var moduleManager`.
  - `HudModuleManager.as` 18-23: `AddEntry(name, file, depth)` fills `entries` (a `Vector`) and a dictionary.
  - `Hud.as` 287-301: `loadModule` looks the name up; an unknown name loads nothing.
  - The AreaInfo and Watermark entries are commented out (`Hud.as` 246, 234). `hud.ws` 385 creates AreaInfoModule anyway, so its movie never arrives.
- **A module loaded late sits hidden:**
  - A module starts in its Flash `Hide` state (`HudModuleBase.as` 59-68). On load it takes its entry's `m_state` (`Hud.as` 337), which `SetInputContext` sets only for entries already in the table (383-460). An entry added later gets none, so the module stays in `Hide` until the input context changes.
  - In `Hide`, `ShowElement` only records the wish (`states\HideState.as`).
  - `CR4ScriptedHud.UpdateHUD()` (`hud.ws` 1544) sends the current context again. In `Exploration` AreaInfo is `OnDemand`, where `ShowElement(true, true)` sets `visible` and `alpha` within the call (`HudModuleBase.as` 137-175).
  - AreaInfo is `Hide` in the contexts `Combat`, `CombatFists`, `Combat_Replacer_Ciri`, `Scene`, `RadialMenu` and `Death` (`Hud.as` 70-222).
  - `ShowElement`'s third argument, `bIgnoreState`, skips the state (`HudModuleBase.as` 119-129).
- **AreaInfo itself:**
  - `HudModuleAreaInfo.as` 38-42: `SetText` upper-cases its text into `textField`.
  - Its 3 s Flash timer ends in nothing (76-80).
  - The script class hides it after its own `ShowAreaInfo` only (`hudModuleAreaInfo.ws`), which no vanilla script calls but the `testarea` exec. The probe never calls it.

## Console commands

All of them are exec functions, called with `game_exec` (no typing). Arguments are optional, and a string goes in `"`.

| Command | What it does | Log lines |
|---|---|---|
| `qinote(text, secs)` | `ShowNotification(text, secs × 1000)`, as quen's `notice` sends it with 8000. Default text: phase 1's "working" line (56 bytes); default `secs` 3600 | `note shown duration_ms=3600000 popup_before=.. now visible=.. alpha=.. x=.. y=.. scale=.. text=[..]` |
| `qinhide(clear)` | `HideNotification()`; with `clear` true, `ClearNotificationsQueue()`. Starts the watch | `note hide sent clear=.. now ..`; then `watch notification hidden t=<s> ticks=<n> ..` |
| `qiadd(which)` | The run-time entry for slot `which` (0 AreaInfo, the default; 1 Watermark), then `DiscardHudModule` and `CreateHudModule` (next section). Does nothing once the module is loaded | `add <name>: before, script object=..`, `entry root ..`, `entry <name> <file> depth=.. added: entries 29 -> 30` (or `entry refused: ..`), `add <name>: DiscardHudModule called ..`, `add <name>: CreateHudModule called ..` |
| `qishow(text, which, force)` | AreaInfo: the Flash `SetText` (default "quen: agent at work"), then `ShowElement(true, true)`. First `UpdateHUD()` if the module's state isn't `OnDemand`. `force` true: the third argument, `bIgnoreState`, instead | `show: state Hide, UpdateHUD() for context Exploration: state now OnDemand`; `show slot=0 force=false now visible=true alpha=0.80 .. state=OnDemand text=[QUEN: AGENT AT WORK]` |
| `qihide(which)` | `ShowElement(false, true)`, then `ShowElement(false, true, true)` (hides in any state), read back in the same call. Starts the watch | `hide slot=0 in the same call: visible=false alpha=0.00 ..`; `watch module hidden t=.. ticks=..` |
| `qiblink(after, which)` | Both elements shown with their default texts, both hidden `after` s later (default 2.5) by the probe's tick (the notification with `ClearNotificationsQueue`). For check 7 | `blink shown module=.. hide due in 2.50 s at t=..`; `blink hid notification=true module=true t=..`; the watch lines |
| `qisave(on)` | The overlay's saving spinner on, or off at once | `save indicator on=.. now visible=..` |
| `qistate(which)` | Read-backs only: <br>- the notification's sprite and text; <br>- the spinner; <br>- the HUD root, its `loadError` (module movies that failed to load) and the input context; <br>- slot `which`'s script object, sprite, Flash state, `isEnabled` and text; <br>- the engine time | `state t=.. note ..`, `state spinner ..`, `state hud root .. loadError=.. context=..`, `state AreaInfoModule: script object=.. state=.. enabled=.. text=[..]` |

**The watch:** after a hide, the probe's timer on the player looks at each hidden element every tick. It logs the first tick on which the element's sprite reads hidden (`visible` false or `alpha` 0), with `t`, the engine seconds since the hide, and gives up after 3 s. It runs on the world's clock, which stops in photo mode and under the pause menu. So it measures hides in gameplay only.

**Null checks:** each function checks the GUI manager, the popup, the HUD, its root sprite, `moduleManager`, `AddEntry`, the module's sprite and the Flash functions before it calls anything. On a null it logs `.. refused: ..` and changes nothing.

## The run-time module entry, and its fallback

**The order in `qiadd`** (`QuenIndEntry`, then the HUD calls). Every read comes before the first change:
1. Read the HUD (`theGame.GetHud()`) and its root sprite (`GetHudFlash()`).
   - It logs the root's public `isDynamic` and `loadError`: this proves `GetMember*` reaches the root's AS3 members.
   - Then `GetMemberFlashObject("moduleManager")` and its `GetMemberFlashFunction("AddEntry")`. A null stops here.
2. Read the table: `GetMemberFlashArray("entries")`, and vanilla's `GetObjectFromArrayWithLabel(entries, "m_name", name)`. An entry already there isn't added twice.
3. `AddEntry(name, file, depth)`, with the file name and depth of the commented-out line (`hud_areainfo.swf`, 14).
   - It reads the table back: the entry must be there.
   - If the `Vector` doesn't read as an array, the log says `unconfirmed` and the probe goes on.
   - If the table reads but lacks the entry, nothing more happens.
4. `DiscardHudModule(name)`, then `CreateHudModule(name)`.
   - Vanilla `OnConfigUI` already created AreaInfo with no entry. Whether the engine keeps a record of that, and so ignores a second `Create`, is unknown; `Discard` clears it first.
   - On a module whose movie never loaded, `Discard` should reach Flash's `unloadModule` (through `CoreHud.as`'s `_HOOK_unloadModule`, 64-67), which finds no clip and removes nothing (`Hud.as` 305-321). Inferred: the engine's side can't be read here.
   - No script holds the old module: `AddHudModuleReference` runs only from the module's own `OnConfigUI`, which never ran.
5. The movie loads asynchronously (`Loader`, then `registerChild`, then the module's `OnConfigUI`). So `qistate` reads the result in a later call.

**What can't be known before the boot:**
- whether GFx hands back a plain AS3 object (`moduleManager`) and its method through `GetMemberFlashObject` / `GetMemberFlashFunction` (no vanilla script calls `GetMemberFlashObject`);
- whether `InvokeSelf` runs a method taken from such an object;
- whether `CreateHudModule` works after the HUD's `OnConfigUI`.

The log names the first step that fails.

**The fallback (`modQuenProbeIndicatorWrap`):**
- Use it only if the entry was added (`entry .. added` or `unconfirmed`) but `qistate` never shows a loaded module. That is, the engine refuses a late `Create`.
- It wraps `CR4ScriptedHud.OnConfigUI`, the one script hook between the HUD movie's construction and its module loop (`hud.ws` 340-412). It adds the entry with the same `QuenIndEntry`, then runs the vanilla body, whose own `CreateHudModule("AreaInfoModule")` (385) then finds it.
- No `Discard`, no late `Create`. This is why it is a wrap: the run-time route needs none.
- It runs once per HUD creation, so it needs its own launch with both folders staged.
- If `GetMemberFlashObject("moduleManager")` itself gives null, the wrap can't help: it uses the same call. Then no script route exists, and phase 2 needs another way in (not built here).

## Offline compile (2026-10-21)

Through quen's `script_compile` (`stubs: true`), with wcc_lite's `-agreetoterms`. The tree: the game's scripts, every installed `Mods\*\content\scripts` and the probe. The game wasn't running, and the lock was free.

| Run | `extra` | Result |
|---|---|---|
| clean | `modQuenProbeIndicator` | exit 0, 0 errors, 724 warnings, none in the probe's file |
| fallback | `modQuenProbeIndicator`, `modQuenProbeIndicatorWrap` | exit 0, 0 errors; the same 724 warnings (`messages.json` byte-identical to the clean run's) |
| control | a scratch copy with `QuenNoSuchCall();` planted after `AddEntry`'s call in `QuenIndEntry`, plus the wrap | exit 1: `local\quenProbeIndicator.ws[235]: Could not find function 'QuenNoSuchCall'`, the planted line |

The compiled files' sha256:
- `quenProbeIndicator.ws` `8c8a63f4cef662dd639433a92d472d9c99bd8bdff938e229abfe4d4b19bbb112`
- `quenProbeIndicatorWrap.ws` `6f7b5075deea210889fce8eeccd6f51ca4bca517f4a40c927a207c9723c69f7f`

What the compiler taught on the way:
- `name` and `set` are keywords;
- a `var` initialiser can't hold a cast;
- an object can't be compared with `!= NULL`: test it in an `if`;
- a wrapped event can call `wrappedMethod();` without returning its value.

## Boot plan (about 25 minutes, the PC free)

The owner and the main session schedule it. Nothing here launches the game.

**Needs:** quen 0.1.11 with quen-w3 0.1.13 or later (`game_exec`), PresentMon, no other load on the PC (a frame-time check). With quen 0.1.12 installed, the timed pairs below can go in `run_plan` (see "Timing").

**Launch:** `session_plan`, then `session_launch` after the owner's yes:
```json
{"save": "<the Hierarch Square save, the walker probe's>",
 "stage_mods": ["<checkout>\\w3\\probes\\indicator\\modQuenProbeIndicator"],
 "debugscripts": true, "photo": false, "devkit": false, "vsync": false,
 "park": "<the owner's usual park list for clean tests>", "user_ok": true}
```
- `photo: false`: HUD modules don't show in photo mode.
- `devkit: false` and `vsync: false`: a timing boot (quen_guide).

**Where to stand:** Hierarch Square, Novigrad, at the ledger's pose `hierarch-geralt-front`: `game_teleport {pos: [668.99, 1875.87, 9.94], rot: [0, 52.3, 0]}`.
- It is quen's proven test spot (boots 187-191): a stable third-person view, with merchants nearby for the dialogue.
- Checks 1-5 run at noon: `world_get` (note `day`), then `game_settime {day: <day>, hour: 12}` and `game_weather {name: "WT_Clear"}`.
- Checks 6-7, the frame-time windows, run at 02:00 (`game_settime {day: <day>, hour: 2}`), when fewer townsfolk walk.
  - Teleport to the pose again, wait 30 s (`game_keys {tokens: ["wait:30000"]}`), and note `world_actors {radius: 30}`'s `found`.
  - `measure_ab`'s drift guard then watches the count.

**Timing:**
- quen runs one call at a time, and each call is an agent turn. A screenshot can't be placed at +0.1 s or +0.5 s by hand.
- So each hide is timed by two methods:
  - the probe's watch line: `t`, engine seconds from the hide to the first tick on which Flash reads it hidden;
  - a `game_screenshot` at a known delay. With quen 0.1.12 that is one `run_plan`, e.g. `{"tag": "c1h", "steps": [{"game_exec": {"line": "qinhide()"}}, {"wait": 0.5}, {"game_screenshot": {"tag": "c1_hide05"}}]}`. Keep each plan under 30 s of estimate, so that quen shows no notice of its own.
- With quen 0.1.11 the screenshot is the very next call. It shows the hide was done by then, a few seconds at most, not at 0.5 s: `W3Quen.log` stamps ticks, and a capture's `t` has whole seconds. The 0.5 s itself then rests on the watch alone.
- Waits: `game_keys {tokens: ["wait:<ms>"]}` (up to 60000 a token). It sends no key, and the 60 s idle focus counts only from a call's end. Never leave 60 s without a quen call: idle focus would bring the chat forward and pause the game into its menu.
- quen's own notices replace the probe's notification: one before a `measure_ab` of 30 s or more, and one at `hands_give`. Show it again after them.

**The checks, in order** (the crash-prone check 3 comes after the notification's checks):

0. **Baseline:** `game_exec qistate()`. Expect:
   - `note .. visible=false`;
   - `state hud root visible=true .. loadError=0 context=Exploration`;
   - `state AreaInfoModule: script object=<true|false>`. This tells whether vanilla's `CreateHudModule` left a script object behind.
   - Then `game_screenshot {tag: "c0_plain"}`.

1. **The lasting notification** (about 3 min):
   1. `game_exec qinote()`. The log: `duration_ms=3600000`, `text=[quen: agent at work. Please keep off the mouse and keys.]`.
   2. `game_keys {tokens: ["wait:8000"]}`, then `game_screenshot {tag: "c1_t10"}`.
   3. `game_keys {tokens: ["wait:45000"]}`, then `game_screenshot {tag: "c1_t60"}`, then `game_exec qistate()` (`visible=true alpha=1.00`).
   4. The hide: `game_exec qinhide()`, then at once `game_screenshot {tag: "c1_hide"}` (or the `run_plan` above).
   5. `game_scriptlog {channel: "QuenInd", lines: 40}`: `watch notification hidden t=..`.
   6. A longer text (an exec argument holds at most 100 bytes): `game_exec qinote("quen: you have the controls. Tell the agent in the chat to take them back.")` (74 bytes), then `game_screenshot {tag: "c1_long"}` and `game_exec qistate()`: does the box fit it, and did the text arrive whole through the console's parser? Then `game_exec qinhide(true)`.

2. **Replacement** (about 2 min):
   1. `game_exec qinote()`, then `game_exec testnotify()` (vanilla: "Some test notification", 4 s).
   2. `game_screenshot {tag: "c2_vanilla"}`.
   3. `game_keys {tokens: ["wait:6000"]}`, then `game_screenshot {tag: "c2_after"}` and `game_exec qistate()`. Expected: nothing on screen, `text=[Some test notification]`: quen's line doesn't come back.
   4. The hub menu: `game_exec qinote()`, `game_keys {tokens: ["i"]}` (the inventory), `game_screenshot {tag: "c2_hub"}`, `game_keys {tokens: ["escape"]}`, `game_screenshot {tag: "c2_hub_closed"}`. Expected: cleared on closing (`commonMenu.ws` 567).
   5. Then `game_exec qinhide(true)`.

3. **The run-time module entry** (about 2 min):
   1. `game_exec qiadd()`. Read its `log` line by line: which step passed, and whether the entry count went 29 to 30.
   2. `game_exec qistate()`. Expect `script object=true visible=true alpha=0.00 .. state=Hide`.
      - If it shows `sprite=null`: `game_keys {tokens: ["wait:2000"]}` and `qistate()` again. A movie load takes frames.
      - Compare `loadError` with check 0's: a rise means the movie file failed to load.
   3. **If the game exits** (every result's `state.game` says `exited`): `session_end`, report the log's last `QuenInd` line, and stop. The fallback needs the owner's call after a crash.
   4. **If the entry was added but no module loads** after two reads: the fallback boot (below).
   5. **If a step before `AddEntry` refused:** the fallback can't help. Record which step, and skip checks 4, 5 (module part) and 9.

4. **Text, look, hide latency** (about 2 min):
   1. `game_exec qishow()`. The log: `state Hide, UpdateHUD() .. state now OnDemand`, then `visible=true alpha=0.80 .. text=[QUEN: AGENT AT WORK]`.
      - If the state stays `Hide` (another context): `game_exec qishow("quen: agent at work", 0, true)`.
   2. `game_screenshot {tag: "c4_shown"}`. Expected: gold upper-case, about 50 px at 1080p, centred near the top (the anchor `mcAnchorAreaInfo`, 960 / 167 at 1920 × 1080). Note where, how big, which font, against the HUD's other text.
   3. A longer line: `game_exec qishow("quen: measuring for about 60 s. Hands off until it ends.")`, then `game_screenshot {tag: "c4_long"}`: does the 729 px field wrap it?
   4. The hide: `game_exec qihide()`, then at once `game_screenshot {tag: "c4_hide"}` (with 0.1.12: `run_plan` with `{"wait": 0.1}`).
      - The log must say `in the same call: visible=false alpha=0.00`.
      - The watch must say `watch module hidden t=0.0.. ticks=1`.

5. **Where the game hides each element** (about 5 min). Both up first: `game_exec qinote()`, then `game_exec qishow()`, then `game_screenshot {tag: "c5_gameplay"}`.
   1. Photo mode, its UI shown:
      - `game_photo {action: "enter"}`, then `game_screenshot {tag: "c5_photo"}` and `game_exec qistate()` (is the HUD root `visible=false`?);
      - `game_keys {tokens: ["tab"]}`, then `game_screenshot {tag: "c5_photo_tab"}`;
      - `game_keys {tokens: ["tab"]}`, then `game_photo {action: "exit"}`, then `game_screenshot {tag: "c5_back"}`: do both come back?
   2. The pause menu: `game_keys {tokens: ["escape"]}`, `game_screenshot {tag: "c5_pause"}`, `game_exec qistate()` (exec works behind the menu, boot 190), then `focus {to: "game"}`.
   3. A dialogue (optional; skip it if no merchant stands within 30 m):
      - `world_actors {radius: 30}`: pick an actor whose `name` holds "Merchant" and whose `scene` is false;
      - `game_place {pos: <1.2 m from it>, face: [<its x>, <its y>]}`, then `game_keys {tokens: ["e"]}`;
      - `game_screenshot {tag: "c5_dialog"}`, then `game_exec qistate()`: the context `Scene`, the module's state `Hide`;
      - end it on its "Goodbye" line: arrows, then `enter`, with a screenshot before the `enter`;
      - `game_screenshot {tag: "c5_after_dialog"}`: does the module come back (`OnDemand` restores the shown wish)?
   4. Teleport back to the pose. Then `game_exec qinhide(true)` and `game_exec qihide()`.

6. **Frame-time cost, shown against hidden** (about 3 min, at 02:00, both hidden at the start):
   - The call:
     ```json
     {"tag": "c6", "secs": 30, "settle": 10, "strict": true,
      "states": [{"label": "hidden", "actions": [{"console": "qinhide(true)"}, {"console": "qihide()"}]},
                 {"label": "shown",  "actions": [{"console": "qinote()"}, {"console": "qishow()"}]}]}
     ```
   - The order is the default: hidden, shown, hidden, shown.
   - Window 1 measures the game as it is, so `hidden` goes first.
   - quen's own measuring notice shows before window 1, and the window starts 9 s after it, when it is gone. The `shown` lines run only later, at the switches.
   - The run ends in `shown`, so quen runs `hidden`'s lines again at the end.
   - After it: `game_scriptlog {channel: "QuenInd", lines: 60}`: each switch's `show` and `hide` lines must hold `visible=true` / `visible=false`.
   - **No measurable cost** means all of these:
     - the verdict is not `outside drift`;
     - `|diff|` of the mean frame time ≤ 0.05 ms, and of p99 ≤ 0.5 ms (the design's sampler cap; `measure_ab` gives p95, so p99 comes from each window's CSV);
     - CPU busy's diff inside its drift;
     - one PresentMode.
   - The verdict is inside drift but a drift is over 0.05 ms: "no cost seen, resolution <drift> ms". That is not a pass of the cap.
   - It is a claim of one method (PresentMon); a Tracy boot's `CBaseEngine_Tick` zones are the second.

7. **A window 0.5 s after a hide** (about 1.5 min, both hidden at the start):
   ```json
   {"tag": "c7", "secs": 10, "settle": 3, "strict": true,
    "states": [{"label": "plain", "actions": [{"console": "qinhide(true)"}]},
               {"label": "after_hide", "actions": [{"console": "qiblink(2.5)"}]}]}
   ```
   - `qiblink(2.5)` shows both elements, then hides them 2.5 s later. The window starts after the 3 s settle, about 0.5 s after the hide.
   - Check it two ways:
     - the log's `blink hid .. t=` and the watch lines (the notification gone about 0.3 s after its hide);
     - each window's `apply` / `settled` / `presentmon_start` UTC.
   - **Pass:** the verdict not `outside drift`, and every `after_hide` window's mean inside the `plain` windows' range ± their drift.

8. **The saving spinner** (optional, about 1.5 min):
   - `game_exec qisave(true)`, then `game_screenshot {tag: "c8_t0"}`;
   - `game_keys {tokens: ["wait:20000"]}`, then `game_screenshot {tag: "c8_t20"}` and `game_exec qistate()`;
   - `game_exec qisave(false)`, then `game_screenshot {tag: "c8_off"}`.

9. **Watermark** (optional, only if check 3 passed):
   - `game_exec qiadd(1)`, then `game_exec qistate(1)`;
   - `game_exec qishow(none, 1)` (Watermark ignores the text), then `game_screenshot {tag: "c9_watermark"}`;
   - `game_exec qihide(1)`.

**Teardown:**
1. `game_exec qinhide(true)`, `game_exec qihide()`, `game_exec qisave(false)`, then `game_screenshot {tag: "c_end"}`: nothing of the probe's.
2. `game_scriptlog {channel: "QuenInd", lines: 500}`: keep it for the report.
3. `session_end`. It recycles the staged copy. The revived module lives only in this run of the game; nothing is saved (the boot plan writes no save).

**The fallback boot** (only after check 3's "no module loads", about 8 min more):
1. `session_end`, then `session_launch` with the same arguments and `stage_mods` holding both folders: `..\modQuenProbeIndicator` and `..\modQuenProbeIndicatorWrap`.
   - Not `again`: that replays every exec of the first boot, `qiadd` included.
2. The pose and the time as before, then `game_exec qistate()`.
   - Expect `script object=true` with a sprite already. In the script log, at the load, the wrap's `entry AreaInfoModule hud_areainfo.swf depth=14 added: entries 29 -> 30`.
3. Then checks 4, 5 (the module part) and 6 as above.

## Pass / fail

| Check | Pass | Fail |
|---|---|---|
| 1 lasting | `c1_t10` and `c1_t60` show quen's text; `qistate` `visible=true` at +60 s | gone before 60 s, or the text cut |
| 1 hide | the watch's `t` ≤ 0.5 s, and `c1_hide` shows none (with 0.1.12: `c1_hide05`) | `t` over 0.5 s, or the watch gives up |
| 2 replacement | as expected: vanilla's text replaces quen's, which doesn't come back; the hub's close clears it | quen's comes back (then a queue is in play: note it) |
| 3 entry | `entry .. added`, then `qistate` shows a sprite, with no crash | a refusal before `AddEntry` (no script route), no module after `Create` (the fallback), or a crash |
| 4 look and hide | `c4_shown` readable; the hide `in the same call: visible=false`, the watch `ticks=1`, and `c4_hide` shows none | the module draws nothing while `visible=true`, or the watch's `t` is over 0.1 s |
| 5 where | each screenshot answers one cell of the design's §3.5 table; nothing to fail | — |
| 6 cost | no measurable cost, as defined in check 6 | outside drift, or over the cap |
| 7 after a hide | inside the plain windows' drift | outside it: a longer wait before windows |
| 8, 9 | answered, or skipped | — |

## What the results mean

**Phase 1** (the lasting notification and quen's states) needs checks 1, 2, 6 and 7:
- **1 passes:** `notice` takes a `duration_ms` up to 3,600,000, and the hide is a new verb.
  - The hide wait before windows and captures becomes the watch's `t` plus a margin (the design proposes 0.5 s), instead of 9 s.
  - Use `ClearNotificationsQueue` for that hide: the same tween, and a queued notification can't follow it into a window.
- **2:** confirms re-sending on quen's next transition only. The game's own notifications and the hub menu's close end quen's line until then.
- **6:**
  - No measurable cost: the line may stay up between windows.
  - Over the cap: it still shows between windows, since it is hidden in every window anyway. The budget's "may it stay up" stays no.
- **7 passes:** the 0.5 s wait holds. If it fails: lengthen the wait, then check 7 again.
- **1 fails** (the timer cuts a long duration short, or the hide doesn't work): the notification stays transition-only (today's behaviour), and phase 2 or the design's route 5 carries the lasting line.

**Phase 2** (the HUD look) needs checks 3, 4 and 5:
- **3 passes:** route B is open from script alone. quen.ws gets `QuenIndEntry`'s calls, `UpdateHUD` and the two-call hide.
  - The same entry serves route C (quen's own movie under `WatermarkModule`).
  - Check 9 shows that slot's chain.
- **3 passes only with the wrap:**
  - route B needs a wrapped `CR4ScriptedHud.OnConfigUI` in `modQuen`: one more conflict surface. No installed mod wraps it (a grep of the installed mods, 2026-10-21);
  - the module exists from the HUD's creation, not on first use.
- **3 fails before `AddEntry`:** no script route into the HUD's table.
  - Phase 2 then needs another way in (a movie of quen's loaded otherwise), or stops at the notification.
  - Route 5 stays the fallback for the look.
- **4:** the module's look decides between B as is and C (quen's own movie, styled). Its hide latency (one call, one tick) sets phase 2's wait at 0.1 s.
- **5:** fills the design's §3.5 table. The expected answer: hidden with the HUD in photo mode and menus, and by its state in combat and scenes, coming back after. The notification covers those, as the design proposes.

## Cleanup

`session_end` recycles the staged copies. This folder is source: nothing the boot produces is written here.
