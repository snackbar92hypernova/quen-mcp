# modQuenProbeFreecam: PROBE BUILD, not for play

A throwaway script mod. It answers one question in one boot of about 10 minutes: **does the game's own developer free camera work in the retail 5.01, with the world running?** quen builds a `game_freecam` tool only if it does. The use case: watch roads and squares for the Crowds Remastered mod while Geralt stands out of the way, where `game_camera` and `game_frame` need photo mode, which pauses the world.

- One file: `modQuenProbeFreecam\content\scripts\local\quenProbeFreecam.ws`. It goes in with quen's `stage_mods`; no resources.
- No wrapped vanilla method, no added field or class. Five exec functions and four plain helpers.
- Nothing runs until an exec is called.

## What it rests on

Read in the game's scripts (`content\content0\scripts`) and `bin\config\base`, 2026-10-23:
- **The natives,** `CR4Game` in `engine\game.ws` 158-167: `IsFreeCameraEnabled() : bool`, `EnableFreeCamera(flag : bool)`, `GetFreeCameraPosition() : Vector`, `MoveFreeCamera(position : Vector, rotation : EulerAngles)`. Nothing reads the free camera's rotation: the probe reads `theCamera` (the rendered view) beside it.
- **Their callers:** vanilla's `moveCamera` exec (`game\player\playerCheats.ws` 340) passes its six floats straight to `MoveFreeCamera`. Nothing in the scripts turns the free camera on.
- **The one other call,** `game\player\r4Player.ws` 1701: in `CR4Player.OnDeath`, after the death's action blocks, no-save lock and aiming are set, the player's death turns the free camera off. So the game expects the free camera on during live gameplay, and a death ends it on its own: a free-camera tool must read `IsFreeCameraEnabled` back, not assume it.
- **`EulerAngles`** (`core\math.ws` 14): `Pitch, Yaw, Roll`, the order of `moveCamera`'s `rx, ry, rz` and of `game_camera`'s `rot`.
- **Its keys,** `bin\config\base\freecamera.ini` `[FreeCamera/InputKeys]`: W/S/A/D move, Q/E up and down, R/F roll, LShift `SpeedBoost`, P `TakeScreenshot`, **the right mouse button `TeleportPlayerAtScreenCenter`**; on the pad only: `TeleportPlayerAtCameraPosition`, `ToggleAttachedToPlayer`, `ExitFreeCamera`, FOV. The keyboard has no exit key: `qfcoff()` is the way out. Whether the retail game reads this ini is unknown.
- These are developer natives: whether they do anything in the retail 5.01 is the probe's main question.

## Console commands

Every line goes to the log channel `QuenFC`: `game_exec`'s `log` returns it (as the indicator probe's lines in boot 190), and `game_scriptlog {channel: "QuenFC"}` reads it (needs `debugscripts`).

| Command | What it does | Its line |
|---|---|---|
| `qfcon()` | `EnableFreeCamera(true)` | `on ...`, the status fields below |
| `qfcoff()` | `EnableFreeCamera(false)` | `off ...`, the status fields |
| `qfcmove(x, y, z, pitch, yaw, roll)` | `MoveFreeCamera` to that pose (metres; degrees) | `move sent=x,y,z rot=p,y,r on=.. read=<GetFreeCameraPosition at once> off_m=..` |
| `qfcrel(fwd, right, up)` | the free camera `fwd` m along Geralt's heading, `right` m to his right (both 0: fwd 10), `up` m (default 2) above the navmesh there (`NavigationComputeZ` within 30 m of his z, else his z), aimed at 1.7 m above him: yaw `VecHeading` (forward (−sin, cos), as `game_camera`), pitch the drop's angle, negative looking down (quen's photo-mode framing). Whether the free camera reads angles that way is part of the question | `rel fwd=.. right=.. up=.. ground=nav\|player sent=.. rot=.. on=.. read=.. off_m=..` |
| `qfcstatus(radius)` | changes nothing | `status ...` |

**The status fields:** `on` (`IsFreeCameraEnabled`), `fc` (`GetFreeCameraPosition`), `cam` and `cam_rot` (`theCamera`'s pose: does the rendered camera follow the free one?), `player` and `heading`, `dist` (free camera to player, 3-D), `paused` (`theGame.IsPaused()`), `game_s` (game time in seconds), `engine_s`, `r` (default 30 m) and the alive actors within `r` of the free camera (`near_fc`) and of the player (`near_player`), both by `FindGameplayEntitiesInSphere` with `FLAG_OnlyAliveActors`, the player counted when in range.

A move's `read` comes in the same tick as the move: `qfcstatus()` after it reads the settled pose.

## Offline compile (2026-10-23)

Through quen's `script_compile` (`stubs: true`, wcc_lite with `-agreetoterms`). The tree: the game's scripts, every installed `Mods\*\content\scripts` (modQuen among them) and the probe. No session was live.

| Run | `extra` | Result |
|---|---|---|
| clean | `modQuenProbeFreecam` | stubbed run: exit 0, 0 errors, 724 warnings (the installed mods' count), none in the probe's file; "Success! Scripts cooked" |
| control | a scratch copy with `QuenNoSuchCall();` planted in `qfcstatus` | exit 1: `local\quenProbeFreecam.ws[103]: Could not find function 'QuenNoSuchCall'`, the planted line |

`quenProbeFreecam.ws` sha256 `2bea98d9b85ec51eca3c1168ccb2f2d74f5de83b83af9327496516c63a7d5445`.

## Boot plan (about 10 minutes, one look from the owner)

The owner and the main session schedule the boot. Nothing here launches the game.

**Needs:** quen-w3 0.1.13 or later (`game_exec`, `world_frame`, `world_nav`, `game_place`), PresentMon for step (f).

**Launch:** `session_plan`, then, after the owner's yes, `session_launch`:
```json
{"save": "<a daytime open-world save: the Crowds probes' Hierarch Square save will do>",
 "stage_mods": ["<checkout>\\w3\\probes\\freecam\\modQuenProbeFreecam"],
 "debugscripts": true, "photo": false, "devkit": false, "user_ok": true}
```
- `photo: false`: photo mode pauses the world, which is what the free camera should avoid.
- `devkit: false`: step (f)'s frame times. No park: the probe touches no other mod's files.
- **Tell the owner at the launch:** about 10 minutes; hands off the mouse and keyboard the whole time (the right mouse button teleports Geralt in the free camera); one look at step (c), below.

**Where:** with the Hierarch Square save, the ledger's pose `hierarch-geralt-front`: `game_teleport {pos: [668.99, 1875.87, 9.94], rot: [0, 52.3, 0]}`. Then `focus {to: "game"}`: the world pauses while the game isn't in front.

**Reading the probe:** each exec goes as `game_exec {line: "..."}`; its line is in the result's `log`, else `game_scriptlog {channel: "QuenFC", lines: 20}`. If `game_exec` is refused, `game_console` types the same line. Note `state.game` (`in_front`, `menu`, `photo`) of every result.

**Steps:**
- **(a) Before:** `qfcstatus()`, `world_get`, `game_camera` (no pose: the game camera's). Expect `on=false`, `paused=false`, `cam` behind Geralt; note what `fc` reads while off.
- **(b) On, 10 m from Geralt looking at him:**
  1. `qfcon()`. `on=false` here: the native does nothing in retail. Go to (g) and the teardown.
  2. `qfcrel(10, 0, 2)`, then `qfcstatus()`: `fc` at the sent pose? `cam` equal to `fc` (within 0.1 m)?
  3. `game_camera` (no pose) and `world_frame {radius: 30}`: both read `theCamera`. If it follows the free camera, `world_frame` lists Geralt (`player: true`) near the frame's centre (`x`, `y` near 0): the aim's convention is right. Off-centre or behind (`x` null): note `cam_rot` against `sent rot`.
  4. `game_screenshot {tag: "fc_b"}`, and read the picture: Geralt from the front at about 10 m, or the usual view from behind him? The HUD? From the results: `photo`, `menu`.
- **(c) The world runs** (the owner's look): one `run_plan`, about 6 s:
  ```json
  {"tag": "fc_c", "steps": [
    {"world_get": {}}, {"world_actors": {"radius": 40}}, {"world_frame": {"radius": 30}}, {"game_exec": {"line": "qfcstatus(30)"}},
    {"wait": 5},
    {"world_get": {}}, {"world_actors": {"radius": 40}}, {"world_frame": {"radius": 30}}, {"game_exec": {"line": "qfcstatus(30)"}}]}
  ```
  `world_actors` reads around the player; at 10 m its 40 m covers the camera's surroundings. Read from `fc_c-result.json`: actors by `id` in both samples, how far each moved and its `velocity`; `paused`, `game_s` and `engine_s` in the two status lines; Geralt's `pos` in the two `world_get`s; `fc` the same in both.
- **(d) Input:** `game_keys {tokens: ["w:1000"], focus: true}`, then `qfcstatus()` and `world_get`. Did `fc` move (and `cam` with it), and Geralt not? Then `qfcrel(10, 0, 2)` again.
- **(e) Distance:**
  1. The line: `world_nav {segments: [8 segments from Geralt's pos, 150 m along yaw 0, 45, ..., 315: to = pos + 150 × (−sin yaw, cos yaw)]}`. The yaw with the farthest `reach` runs along a street. `game_place {pos: <Geralt's x, y>, yaw: <that yaw>}` turns him to it (it taints saves; this boot writes none).
  2. One `run_plan` `fc_e`, about 15 s; for each d of 50, 150 and 400: `{"game_exec": {"line": "qfcrel(<d>, 0, 3)"}}`, `{"wait": 3}`, `{"game_exec": {"line": "qfcstatus(30)"}}`, `{"world_frame": {"radius": 30}}`, `{"world_actors": {"radius": 30}}`, `{"game_screenshot": {"tag": "fc_e<d>"}}`.
  3. Per distance: around the camera `near_fc` (the probe, at `fc`) and `world_frame`'s `found` (at `theCamera`, if it follows), against around the player `near_player` and `world_actors`' `found`; `ground=` (is the navmesh there?); and the screenshot: people, LOD, holes in the world.
- **(f) A frame-time window:** `qfcrel(10, 0, 2)`, then `measure_presentmon {tag: "fc_pm", secs: 5}`. Does it give rows (the PresentMode)? Its `indicator` and `timing_s` (`status_hide`, `status_confirm`): did quen's line hide and show as usual? One window: no comparison and no claim.
- **(g) Off:** `qfcoff()`: `on=false`, `cam` back behind Geralt. `game_camera`, `game_screenshot {tag: "fc_g"}`: the usual third-person view. `world_get`: Geralt where (e) left him.

**Teardown, always,** after a failed step too: `qfcoff()`, then `qfcstatus()` reads `on=false`; then `session_end`. If the camera stays on, `session_end` still ends it with the game; the boot writes no save.

**The owner's one look,** told at the launch:
- **When:** about 2 minutes after Hierarch Square appears, the view jumps from behind Geralt to about 10 m in front of him, looking back at him (step b). Watch from the jump until the view glides forward by itself (step d's W key), about 15-20 s.
- **Where:** the whole game window, and quen's line (top centre, or bottom left).
- **What:** is the picture live (people walking, banners or smoke moving) or frozen? Is quen's line there?
- **Answer** in the chat after the boot: no question during it.

## Pass / fail

| Step | Pass | Fail |
|---|---|---|
| (b) on | `on=true`; `cam` within 0.1 m of `fc` after the move; the screenshot shows the free camera's view | `on=false`; or `on=true` with the normal view on screen |
| (b) flags | `photo: false`, `menu: false`, `in_front: true` | `menu: true`: modQuen sees the free camera as a menu |
| (c) live | `paused=false`; `engine_s` advances about 5 s and `game_s` advances; at least 2 actors moved over 0.5 m between the samples; Geralt moved under 0.5 m (a push counts against it: note it); the owner saw motion | `paused=true`, or no actor moved, or the owner saw a frozen picture |
| (d) input | `fc` moved, Geralt didn't | Geralt walked; or nothing moved |
| (e) reach | measured, not pass/fail: the counts and pictures per distance | — |
| (f) window | rows, and the line's hide confirmed | the call fails: its `reason` |
| (g) off | `on=false`, `cam` within 5 m of Geralt, the normal view | `on=true` after `qfcoff()` |

## What the results mean for `game_freecam`

| Seen | Meaning, next step |
|---|---|
| (b), (c) and (g) pass | **Build it** as modQuen verbs on these natives: on, pose, off, no ASI work. With `cam` following `fc`, `world_frame` and `game_camera`'s read work under it as they are; otherwise the tool reads `GetFreeCameraPosition` and has no rotation read |
| `on=false`, or the normal view with `on=true` | The natives are dead in retail: no tool on this path; the owner picks the next route |
| (c) frozen | The free camera pauses like photo mode: no gain, no tool |
| (b) `menu: true` | quen's tools that refuse in a menu (`game_console`, the measure tools' menu check) need the free camera named in modQuen's state first |
| (d) Geralt walks | Input goes to the player: the tool leaves keys alone, and the boot plan tells the user hands off |
| (d) the camera moves | The ini's keys work: the tool warns that the keyboard flies the camera and the right mouse button teleports Geralt |
| (e) `near_fc` falls to 0 while `near_player` stays | The world lives only around the player: the tool refuses or warns past the last distance with people, and a scene is watched with Geralt within that reach |
| (e) `ground=player` at a distance | No navmesh there: the tool can't set a height from the ground that far out |
| (f) fails, or the hide isn't confirmed | The measure tools need a change before they run under the free camera |
| (g) `on=true` after `qfcoff()` | No way out but closing the game: blocking |
| at any time `on=false` without `qfcoff()` | Something turned it off (a death does, `r4Player.ws` 1701): the tool reads `on` back in every result |

## Cleanup

`session_end` recycles the staged copy. The boot writes no save, so no save can carry the free camera. This folder is source: nothing it produces is written here.
