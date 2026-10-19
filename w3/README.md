# quen's Witcher 3 package: `W3Quen.asi` + `modQuen`

quen's only game-side part for The Witcher 3 (5.01, DX12): one native ASI and one script mod, always installed together. It gives quen nine things:
- **Game state:** seven on/off values (gameplay, scene, cutscene, photo mode, fade or loading, dialogue, a menu open), written to `W3QuenState.log` next to `witcher3.exe`.
- **A command channel:** typed commands from quen over a named pipe, run by the script on the game's tick, with replies.
- **Camera paths:** a whole camera path from quen, played on the photo-mode camera with one set per game tick, in real time or frame by frame.
- **Console commands** to load a save by name, which quen types only when the command channel isn't up.
- **Tracy instrumentation** (from 0.1.3), only in quen's `tracy` launches: Tracy zones on 75 engine functions, ported from the W3Trace ASI (section Tracy, below). In any other launch none of it runs.
- **A new manual save** (from 0.1.4) on quen's `save` verb, as the game's menu writes one into its "Empty save slot".
- **An on-screen notice** (from 0.1.8) on quen's `notice` verb: quen's own short text, shown for 8 s.
- **Photo mode's own interface** (from 0.1.9): photo mode opened and closed by the game's own functions, and its weather, time, environment and grading set as its menu sets them, each read back (`photo.*` verbs).
- **Items** (from 0.1.10): an item added to the player's inventory by its name, as the console's `additem` adds one, equipped if asked, its count read back (`items.add`).

Status 2026-10-19: 0.1.0 ran in live boot 2 (the natives registered, the state matched the W3 Game State Bridge's at gameplay, but photo mode never showed). 0.1.1 fixed photo mode and ran from live boot 3. 0.1.2 adds the camera paths (`cam.path`, `cam.frame`, `cam.stop` and the native `QUEN_CamAt`); it is built and tested offline (below), not yet run in the game. 0.1.3 adds the Tracy instrumentation; it too is built and tested offline only. Its `quen.ws` is 0.1.2's, byte for byte. 0.1.4 (2026-10-19) adds the `save` verb to both files; built and tested offline only. 0.1.5 (2026-10-19) adds the read-only `world.get` verb to both files; built and tested offline only. 0.1.5 ran in the game in feature tests 4 (2026-10-19): Tracy, `save`, `world.get`. 0.1.6 (2026-10-19) adds the menu bit (2048) to the state, and W3Quen.log's natives line names the release; built and tested offline only, and never published. 0.1.7 (2026-10-19) supersedes it with the same behaviour: only comments and two reply texts (`cam.set` and `cam.path` outside photo mode) are reworded, in both files; built and tested offline only. 0.1.8 (2026-10-19) adds the `notice` verb to both files (quen shows it when it hands the controls to the user); built and tested offline only. 0.1.9 (2026-10-19) adds the `photo.*` verbs, and rain, snow and the sky flag to `world.get`, in both files; built and tested offline only. 0.1.10 (2026-10-19) adds the `items.add` verb to both files; built and tested offline only.

The sources moved here from the maintainer's `witcher3-mods` repo (its `asi\quen` and `content\modQuen`, commit d74d3a5), where modQuenRpc was a separate script mod. It is now folded into modQuen.

## This folder

| Path | What |
|---|---|
| `asi\W3Quen.cpp` | the ASI: registers `QUEN_Next`, `QUEN_Reply`, `QUEN_State` and `QUEN_CamAt` through the Witcher Native Bridge, writes the two logs, serves the pipe, checks and interpolates camera paths |
| `asi\trace.h`, `asi\hooks.inc` | the Tracy instrumentation, and the functions it hooks (quen reads that list too) |
| `asi\build.bat`, `asi\test.cpp`, `asi\wnb_stub.cpp` | build and offline test |
| `third_party\` | Tracy 0.14.1's client and Microsoft Detours 4.0.1, compiled into the ASI (versions and licences: its README) |
| `mod\modQuen\content\scripts\local\quen.ws` | the one script: state, channel, camera paths, photo mode, console commands |
| `pack.py` | builds, tests and packs the release zip |
| `out\` | build output and zips (ignored by git, never committed) |

## Release

- Releases are GitHub Release assets of this repo: `quen-w3-<version>.zip`, built by `pack.py`. Its root mirrors the game folder:
  ```
  bin\x64_dx12\W3Quen.asi
  bin\x64_dx12\W3Quen-licences.txt
  Mods\modQuen\content\scripts\local\quen.ws
  ```
- `W3Quen-licences.txt` (from 0.1.3) holds the notices of the code compiled into the ASI: Tracy's client (3-clause BSD) and the libraries inside it (moodycamel::ConcurrentQueue, SPSCQueue, LZ4), and Detours (MIT). Each licence asks for its notice to come with the binary.
- Each release's notes give the sha256 of both files and of the zip. quen's doctor pins both files exactly, so a half from another version is refused before a launch.
- The zip bundles neither the Native Bridge nor an ASI loader: a bundled copy could overwrite a newer one on patch day.

| Version | `W3Quen.asi` | `quen.ws` | zip |
|---|---|---|---|
| 0.1.10 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `e0d17b5657b10846db4694d9390bec6c06d964be0572f2a908c4dbcc14c3d3e3` | `45326db9258223da00ac84910414b59ffb18f83b2fee0123732a52c54e53db47` | `82333d27a13809481421f1a3f87a2a1ed6b9ee7f351ce598e86266b2ee23b344` |
| 0.1.9 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `4a9e4c2436fe1dd13e70ffd67ebb6a35e84f8484cb3b3f8846f5fe494927fe0b` | `c2a4da337f4f626234144590e153751a41184ba23268279543e1a1cf0e3918a3` | `850eb2da73f31cea0ccb72fbc016b23c02b7ca60d8e4b692ca30747441914269` |
| 0.1.8 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `f5939b857a2f04f83783d91f05f367563c205296bbc1482e4fe772695b2a4aca` | `b254797300c39f78fe5b1ca1a14559d7988f77121232d99d118ddce6f599af39` | `e08b63dffb932d77d23445e5c3f4b53bf97b6f1c1be882691fd340dc8cbc803a` |
| 0.1.7 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `f4db50a46480acd0bcf2c813383b12ec5a0ad53a5559f92f17ec90b13f30eb0d` | `9fc78e8715f52447194ec25652e4ab8898c83b80c512743f44b05ddbdee30730` | `8231a99249c5688773db47bf94b8644d2f83f8ee398801ed6bb6ac58fd1f2cce` |
| 0.1.6 (never published: 0.1.7 supersedes it, with the same behaviour; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `58b1691992926683a262831b915511e927c879cc0be93199a93f449d540aa3ae` | `e71c3d00b2b48b7bdafe4034f7a9a49ff3a75d61acfaaa9342195b2f201fa5d6` | `7b0640f6538ae835bfb8034b0b75f42ff8fc8b034f6502819c3355ff76dd251d` |
| 0.1.5 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `76b3d4dbb7bd2b7336f14175bd0de60d9e5b04885b5262328ec94533a8cdb811` | `063ff51f9a5ce56fc6fa919881b6d821d2d7cb178a4806ff6e1f42595d7a2a6e` | `d3dc0b65b15e29dd49c03dda1b9e3e99c54589850d328f7edc106fd8af0c4a29` |
| 0.1.4 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `99f340b66208bdd481ae3c20d62da52d63ab21c11e713f594a0cf35205ac8d66` | `4ccbb9319f9f7d83f34133ea30df2b5cdbea403a5a3a6d8360ba7a7aa3f8fa61` | `52f876bc0aa14447930b7f27fb6982ff0a86231c6bf39408b131b9414d1666fa` |
| 0.1.3 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`) | `5e33b4f4ad47a1003d5664863c24f8851959ae888f6aa72c1eeb1f2b483f8a28` | `11c6aed2d755382cc9f8ae2b90586fef10cbd3c6076a2d7d5bca8740e84806dc` (0.1.2's) | `4c7fa454c066cbfaa301318bc33544783624aaaa5ebf6425190bda316b63d338` |
| 0.1.2 (not published yet) | `c14d291cf14724ee7b374c80edd449e196c49a975c9cdf286b04f383e0f8bb1e` | `11c6aed2d755382cc9f8ae2b90586fef10cbd3c6076a2d7d5bca8740e84806dc` | `8003362ea725964d700b6a9f15cd560bd488d9a9b4f416aebb0fb3272fa14b61` |
| 0.1.1 (not published yet) | `87b86bc273690255269891634d9197396e254cbf18bf934bdc6919bc213729c7` | `ee271b148fa516cc3957931948c382d516ff4ba73cda8957a4cbb180642ff308` | `bd1e993fbd240c3dab44b728a376fd0ea257007746632896173c396ab8fe854a` |
| 0.1.0 (never published; the doctor refuses it: no photo mode in its state) | `e55d1e03753b4f903150be140948c187a8c32ff8714657c19a6de8d289a85d91` | `1756d31af4d0eb4a219fedf464f13ff1399ace57a3940b93af44513eb3d8d05c` | `d291b66efc865b636a9e479db48d394b95b862e534ec07f983b7f22800838a15` |

## Install (by hand, once per version)

- **Needs first:**
  - the public **Witcher Native Bridge 2.0** (Nexus mod 13743), including the ASI loader in its download (`dinput8.dll`; any Ultimate ASI Loader x64 named `dinput8.dll` or `winmm.dll` works);
  - the Microsoft VC++ 2015-2022 x64 runtime, which the Native Bridge needs. `W3Quen.asi` itself needs no runtime: it is built with the static CRT.
- **Install:** unpack the zip into `%GAME_DIR%` (the folder holding `bin\` and `Mods\`). Replace an older `Mods\modQuen`, and remove any `Mods\modQuenRpc`: its functions are now in modQuen, and two copies are a compile error.
- **The first launch recompiles the game's scripts** once (it takes longer). A script error shows a `witcher3.exe` dialog or closes the game.
- **Removal order:** `Mods\modQuen` first, then `bin\x64_dx12\W3Quen.asi` (and `W3Quen-licences.txt`). The script imports the ASI's natives, so a script without its ASI stops the script compile at boot.
- **Never install anything from `out\test`:** its `WitcherNativeBridge.asi` is the test stand-in and would break every native mod.
- **Outside quen sessions it is quiet:** without quen's launch token the ASI writes no file, opens no pipe, and `QUEN_Next` always returns "". The script still polls the state about 4 times a second and calls one native per frame.
- **Outside quen's tracy launches the Tracy part is dormant:** no hook, no thread, no socket, no allocation (section Tracy).

## Game state

- `CR4Game.OnTick` polls at most 4 times a second, timed by `GetLocalTimeAsMilliseconds()`, and calls `QUEN_State(flags)` only when the flags change. 0.1.0 timed it by `GetEngineTimeAsSeconds()`, which likely stands still in photo mode: live boot 2's state log never showed photo mode (128).
- It also polls at once when a game starts (`OnGameStarted`, `OnAfterLoadingScreenGameStart`: live boot 2's first state came 0.33 s after the W3 Game State Bridge's, which polls there) and when the photo-mode menu opens or closes (`CR4PhotomodeMenu.OnMenuShown` / `OnClosingMenu`; photo mode switches on before the menu opens and off before it closes).
- An `OnGameEnded` wrap sends 0 at once. That is the "a load began" signal: the tick doesn't run through a load's loading screen (live boot 1 saw 17-18 s without any state).
- The flags use the W3 Game State Bridge's numbers, so quen's `bridge_log` parser reads both logs; 2048 is quen's own. The values come from the same vanilla script calls, written fresh under quen's own names:

  | Value | Meaning | Vanilla call |
  |---|---|---|
  | 0 | no player: main menu, or between saves | `thePlayer` is null |
  | 1 | valid (a game session runs) | `thePlayer` is set |
  | 16 | a non-gameplay scene plays | `theGame.IsCurrentlyPlayingNonGameplayScene()` |
  | 32 | the player is in a non-gameplay cutscene | `thePlayer.IsInNonGameplayCutscene()` |
  | 128 | photo mode | `theGame.GetPhotomodeEnabled()` |
  | 256 | blackscreen, fading or the loading video | `theGame.IsBlackscreenOrFading()`, `IsLoadingScreenVideoPlaying()` |
  | 512 | dialogue or cutscene script flag (can go stale) | `theGame.IsDialogOrCutscenePlaying()` |
  | 2048 | a menu is open: the pause menu the game opens when it loses the front, among others (from 0.1.6; W3GSB has no such bit) | `theGame.GetGuiManager().IsAnyMenu()`, the check vanilla makes before photo mode and the pause menu open |

- **`W3QuenState.log`** (next to `witcher3.exe`, written only when quen launched the game): one line per change, `<tick> push=<n> flags=<f>`, where the tick is `GetTickCount64` in ms. That's the W3 Game State Bridge's line form.
- **`W3Quen.log`** names the release on its natives line (from 0.1.6): `W3Quen 0.1.7: natives queued=1 (WNB API <n>)`. In a tracy launch the 75 hook lines and their total come before it (0.1.7, 2026-10-19). `pack.py` takes the zip's version from the same constant (`kVersion`), so each release's ASI differs from the last: quen's doctor tells a release by its ASI's sha256.
- **`W3Quen.log`** records how many `QUEN_Next` calls came before the first state, and the first call's tick: `first QUEN_State (flags F) after N QUEN_Next calls, the first at tick T`. More than 0 means the game tick runs before gameplay, at the main menu or during the first load; T against the launch and the main menu's time says which (live boot 2: about 2,670 calls in either boot).
- **Living beside the W3 Game State Bridge:** quen reads nothing it writes and reuses none of its names (every name here starts with `QUEN` or `quen`). Both mods wrap `CR4Game.OnTick`, `OnGameStarted`, `OnAfterLoadingScreenGameStart` and `OnGameEnded`, and `CR4PhotomodeMenu.OnMenuShown` and `OnClosingMenu`; every wrap here calls `wrappedMethod()` first and depends on no other mod's wraps.

## Command channel

### What quen must do at launch
- Put `W3_QUEN_TOKEN` (16-64 characters of `A-Z a-z 0-9 - _`, new for every launch) and `W3_QUEN_PID` (quen's own pid, decimal) in the game's environment. The pipe is `\\.\pipe\quen-w3-<token>`.
- Connect as that pid. The ASI drops any other client (`GetNamedPipeClientProcessId`). Before sending, check that `GetNamedPipeServerProcessId` is the game's pid in quen's job.
- With a bad token or pid there is no pipe, and `W3Quen.log` says why.

### The pipe
- One instance, `FILE_FLAG_FIRST_PIPE_INSTANCE` (a name someone else created first is refused, never joined), `PIPE_REJECT_REMOTE_CLIENTS`, and a DACL with one entry: the current user. Overlapped I/O on one worker thread; the game thread only touches two queues under a mutex.
- Wire format: JSON lines as in quen-core's `protocol.rs`. A request line holds at most 4,096 bytes, a reply line 65,536.
- The ASI checks every request as `decode_request` does: unknown verbs and fields, wrong types, duplicate keys, invalid UTF-8, non-finite numbers, an empty name, hour past 23 and fov not above 0 are all refused; from 0.1.9 also a `photo.time` minute outside 0 to below 1440, a `photo.grade` or `photo.tick` without a value, a `photo.tick` time outside 0 to 10 s, and a `photo.list` filter that is empty or over 200 bytes. It also refuses a control character in a name, and a `settime` day above 2^31-1, the script's int range. A refusal is a reply with `"ok":false`, carrying the request's id, or id 0 if the id itself was unreadable.
- Each request's `ttl_ms` is checked when the script polls. An older command is answered `expired: waited N ms` and never runs.
- At most 16 commands wait. A disconnect drops what the client left queued.
- A command handed to the script that gets no `QUEN_Reply` (a script error) is answered `modQuen gave no reply` at the next poll.
- Reply `data`:
  - `listsaves`: an array of `SSavegameInfo.filename`;
  - `cam.get`, `cam.set`, `cam.path` and `cam.frame`: `{"camera":"photomode"|"game","pos":[x,y,z],"rot":[pitch,yaw,roll],"fov":f}`. `rot` is the scripts' `EulerAngles` order, in degrees, the same order the requests use. After a set it is the photo-mode camera's own read-back.
  - `world.get`: `{"player":{"pos":[x,y,z],"heading":heading},"time":{"day":day,"hour":hour},"rain":r,"snow":s,"sky_clear":b,"weather":"<name>"}`, from the script's `x y z heading day hour rain snow skyclear weather` (floats with 4 decimals; skyclear 0 or 1). `rain`, `snow` and `sky_clear` are from 0.1.9. Anything else, an hour past 23 included, is the read-back that did not parse.
  - `photo.list`: an array, as `listsaves`'s (from `a|b|c`).
  - `items.add` (from 0.1.10): `{"before":b,"after":a,"equipped":e}`, from the script's `<before> <after> <0|1>`: the item's count in the player's inventory before and after the add, and whether the first one added reads back as equipped. Anything else is the read-back that did not parse.
  - `photo.get`, `photo.weather`, `photo.env`, `photo.time`, `photo.grade` and `photo.reset` (from 0.1.9): `{"weather":"<name>","minute":m,"env":"<path>","grading":{..},"grading_at_open":{..},"rain":r,"snow":s,"sky_clear":b,"world_weather":"<name>"}`, from the script's `<minute> <8 grading> <8 grading at open> <rain> <snow> <skyclear>|<weather>|<env path>|<world weather>`. `grading` and `grading_at_open` hold `exposure`, `contrast`, `highlights`, `saturation`, `temperature`, `vignette`, `grain` and `chromatic_aberration`. Anything else is the read-back that did not parse.

### Verbs
The script polls from `CR4Game.OnTick`. On 5.01 that tick runs about once per frame in gameplay, in photo mode, under `theGame.Pause` and `SetTimeScale(0)`, and behind the focus-loss pause menu (measured in the probe boot, below). Whether it runs at the main menu is open; commands sent while it doesn't run wait and expire.

Each call was checked in the game's own scripts (content0, 5.01). The "Probe boot" column says what an experiment boot (2026-10-17, before this package existed) ran in the game from a probe mod. The status above says which package versions have run in the game.

| Verb | Game call | Probe boot | State |
|---|---|---|---|
| `load` | `QUEN_LoadSave` → `theGame.LoadGameInit` | loads from gameplay and photo mode, ~33 s to gameplay | real; the reply says only "matched" or "not exactly one save matches" (the count shows on screen) |
| `loadlast` | `QUEN_LoadLast` → `theGame.LoadLastGameInit` | loads the newest save | real |
| `listsaves` | `theGame.ListSavedGames` | newest first, bare lowercase names without `.sav`; a cloud-only entry can keep case and `.sav` | real |
| `cam.get` | photo mode (`GetPhotomodeEnabled`): `GetPhotomodeCamera()` `GetCameraWorldPosition/Rotation`, `GetFov`; otherwise `theCamera.GetCameraPosition/Rotation`, `GetFov` | the photo camera's pose; `theCamera` agrees one frame later | real |
| `cam.set` | photo mode only: `SetFov`, `SetCameraWorldPosition/Rotation`, `UpdateWithoutInput(true)`, as vanilla places it (photomodeManager.ws). The reply's data is the read-back | position and rotation held 120 menu ticks with 0 m deviation, and a typed pose reproduced a reference image to the noise floor. `SetFov` on the photo camera wasn't tried | real |
| `teleport` | `thePlayer.TeleportWithRotation` | XY and yaw exact, Z snaps to the ground; the pose reads back from the next tick | real |
| `settime` | `theGame.SetGameTime(GameTimeCreate(day, hour, 0, 0), true)`, as `exec settime` | — | real |
| `weather` | `RequestWeatherChangeTo(name, 1.0, false)`, as `exec changeweather`. Only names in `GetWeatherNames` (the area's list), which the refusal lists | — | real |
| `pause` | `on`: `theGame.Pause("quen")` unless quen's pause is already on; `off`: `theGame.Unpause("quen")` if it is. The reply states both quen's pause and the game's (`IsPausedForReason("quen")`, `IsPaused()`) | `Pause` froze the world, `Unpause` from script undid it, and the game tick kept running (616 ticks in ~11 s) | real |
| `cam.path` | photo mode only: loads the path (below) and sets its first pose; the reply's data is the read-back | per-tick sets from `CR4Game.OnTick` held exactly (a free-camera path) | new in 0.1.2 |
| `cam.frame` | a frame-step path's frame `n`: set, then held on every tick; the read-back is the data | — | new in 0.1.2 |
| `cam.stop` | unloads the path; the camera stays where it is | — | new in 0.1.2 |
| `save` | no fields. Refused with no player or `!theGame.AreSavesInitialized()`, in photo mode, in a scene, cutscene, fade or the loading video, with `IsDialogOrCutscenePlaying()`, when `theGame.AreSavesLocked()` (the menu's own check, `ingameMenu.ws`), and when the menu would show no empty slot (`GetNumSaveSlots(SGT_Manual)` not -1 and the manual saves `ListSavedGames` gives fill it, `igmUtilities.ws`). Otherwise `theGame.SaveGame(SGT_Manual, -1)`, the menu's "Empty save slot" call, and the reply "save requested": the native returns nothing, so quen judges the save by the files | — | new in 0.1.4 |
| `notice` | `text`, 1 to 160 bytes, no control character (the ASI checks it); changes nothing. `GetGuiManager().ShowNotification(text, 8000)`, as `QUEN_Say` shows its messages; refused without a GUI manager | — | new in 0.1.8 |
| `world.get` | no fields; changes nothing. Refused with no player. Otherwise `thePlayer.GetWorldPosition()` and `GetHeading()` (`CNode`, `engine\node.ws`), `GameTimeDays` and `GameTimeHours` of `theGame.GetGameTime()` (`engine\gameTime.ws`; vanilla `GetDayPart` reads `GameTimeHours` as the hour of the day, 0-23), `GetWeatherConditionName()` (`engine\environment.ws`); from 0.1.9 also `GetRainStrength()`, `GetSnowStrength()` and `IsSkyClear()` (`engine\environment.ws`), the three the HUD's weather icon reads (`environmentManager.ws` `GetCurWeather`) | — | new in 0.1.5; rain, snow and the sky flag in 0.1.9 |
| `photo.mode` | `on`. On: `PhotomodeManager.EnablePhotomode()` (private; through an added method), as photo mode's key calls it, after vanilla's own conditions for it (a dialogue or cutscene, a dead player, a cutscene intro, a blackscreen or fade, a non-gameplay scene, a menu open, a tutorial), which the refusal names: `EnablePhotomode` returns without a word on any of them. Off: the menu's `OnConfirmExit()` (it clears what the user spawned, then `DisablePhotomode()`, which restores the weather), or `DisablePhotomode()` without the menu; never the "Exit Photo Mode?" dialog. Already in the mode asked: ok, nothing done | — | new in 0.1.9 |
| `photo.get` | no fields; changes nothing. The photo state (Reply `data`) | — | new in 0.1.9 |
| `photo.weather` | `name`, a name of `GetWeatherNames` (the area's list, as `weather`; the menu leaves out 14 quest weathers). `CR4PhotomodeEffects.SetWeather(name)`, the menu's call | — | new in 0.1.9 |
| `photo.env` | `path`, any case, one of `GetAllEnvironmentDefinitions` (sent in its spelling). `SetEnvDefUsingPath`, the menu's call (its list shows only paths with a localized name) | — | new in 0.1.9 |
| `photo.time` | `minute`, 0 to below 1440. `SetTimeOfDay(minute)`, then `GetCityLightManager().SetUpdateEnabled(true)` and `ForceUpdate()`, as the menu does. Its time slider holds 1,440 entries, hour × 60 + minute, and passes the entry | — | new in 0.1.9 |
| `photo.grade` | any of `exposure`, `contrast`, `highlights`, `saturation`, `temperature` (kelvin), `vignette`, `grain`, `chromatic_aberration`, at least one; the menu's setters. Depth of field is left out: `SetDofEnabled` has no getter, so a set of it can't be read back | — | new in 0.1.9 |
| `photo.reset` | `part`: `weather` (`RestoreWeather()`, the menu's "none"), `env` (`SetEnvDefNull()`, the menu's "none") or `grading` (the 8 sliders back to what they read as the menu opened: the menu takes its own slider defaults from the same getters then, but vignette's and grain's from 0) | — | new in 0.1.9 |
| `photo.tick` | `tick_s`, `cloth_s`, 0 to 10, at least one: `SetPhotomodeTickTime`, `SetPhotomodeClothsSimulationTime` (the menu passes 0.00001 and 1). Neither has a getter: no read-back | — | new in 0.1.9 |
| `photo.list` | `what` (`weathers` or `envs`), an optional `filter` (any case): `GetWeatherNames` or `GetAllEnvironmentDefinitions`, the names holding it. Refused over 60,000 characters, with the count | — | new in 0.1.9 |
| `items.add` | `name` (the item's own name, as the console's `additem` takes it; spaces allowed), 1 to 200 bytes, no control character; `count`, 1 to 1000; `equip` (the ASI checks them). Refused with no player. As vanilla `exec function additem` (`game\temp.ws`): the name made by the Native Bridge's `WNB_StringToName`, `thePlayer.inv.AddAnItem(name, count)`, and with `equip` `thePlayer.EquipItem` on the first id when `IsIdValid`, read back by `GetWitcherPlayer().IsItemEquipped`. `GetItemQuantityByName` before and after: refused when nothing was added (`AddAnItem` returns an invalid id for a name no item has, `inventoryComponent.ws`). Reply `data` | — | new in 0.1.10 |

Every `photo.*` verb but `photo.mode` is refused unless photo mode is on and its menu is open: the menu turns `CR4PhotomodeEffects` on as it opens (`SetEnabled(true)` in `OnMenuShown`) and off as it closes, and modQuen keeps the menu from its `OnMenuShown` wrap to its `OnClosingMenu` wrap. Every set reads the whole photo state back after it. The menu's sliders don't show what quen sets.

### Camera paths

- **quen sends the whole path once:** `cam.path` with quen-core `sweep.rs`'s canonical keyframes (protocol.rs's message). The ASI checks it as protocol.rs does (`check_playback`, then `Path::new`'s checks, then canonical keyframes) and precomputes it, on the pipe thread. Its port of `Path` is line for line and in double precision, so quen's own sample of a frame and the game's are the same numbers.
- **The script gets** `<handle> cam.path <step 0|1> <frames>` and sets the first pose. From then `CR4Game.OnTick` asks `QUEN_CamAt(frame)` for the tick's pose (`x y z pitch yaw roll fov`, 6 decimals) and sets the photo-mode camera to it, as `cam.set` does (`SetFov`, `SetCameraWorldPosition/Rotation`, `UpdateWithoutInput(true)`).
  - **Real time** (`frame` -1): the time since the path's first pose, by the performance counter. Not `GetEngineTimeAsSeconds()`, which moved in 31.25 ms steps in the probe boot. After the end the last pose holds.
  - **Frame step:** frame n is the path at n / fps, computed and never summed. The pose is set again on every tick, so it holds while quen captures.
- **What stops a path:** a new `cam.path` (it replaces it), `cam.set`, `cam.stop`, leaving photo mode. quen leaving the pipe doesn't: a real-time path plays on between quen's verbs.
- The yaw of a pose is unwrapped along the path (a 360° pan runs to 530°): the game takes any yaw.

**Why `Pause` and not `SetTimeScale(0)`** (both froze the world in the probe boot):
- `Pause` is keyed by reason, so `Unpause("quen")` lifts only quen's pause; menus and other reasons keep theirs.
- `IsPausedForReason("quen")` reads it back exactly.
- A time scale is one entry in a priority stack, and its `dontSave` flag means a scale can end up in a save.

## Console commands

Exec functions need `DBGConsoleOn=true` (in `bin\config\base\console.ini`).
- `quenload(FILENAME)` loads the save whose filename is FILENAME. Case doesn't matter, and `.sav` is optional on both sides. Exactly one match loads it; 0 or more than 1 match loads nothing and says why on screen (`quen: 0 of N saves match X, not loading`). Leave `.sav` out: whether the console takes a dot (or spaces) in an unquoted argument is untested.
- `quenlast()` loads the newest save, like the menu's Continue.
- `quensaves()` writes every save filename to the log (channel `Quen`) and shows the count and the first 20 filenames on screen.

How the loads work:
- The list is `theGame.ListSavedGames(saves)` (content0 `game\commonGame.ws:106`), the same list the Load Game menu shows. The match follows the vanilla `exec function StartNewGamePlus` (`game\temp.ws:11328-11341`), with `.sav` stripped.
- A load is `theGame.LoadGameInit(save, false)` or `theGame.LoadLastGameInit()` (`commonGame.ws:89-90`).
- **What it skips:** the menu's load (`game\gui\main_menu\ingameMenu.ws:3581-3724`) runs two steps before the same `LoadGameInit`, and these functions run neither:
  - `CheckModdedSave`, the mod.io check that refuses a save made with a mod.io mod that's now missing;
  - the load confirmation popup.
- Messages use the game's notification box, `theGame.GetGuiManager().ShowNotification(text, 8000)`, and also go to `LogChannel('Quen', ...)`.
- Typed console text also reaches gameplay: W A S D move Geralt while you type. That doesn't matter for a load.
- Saves migrated from 4.04 still show a notice that waits for Enter after the load.

## Tracy (from 0.1.3; quen's `tracy` launches only)

The W3Trace ASI's Tracy instrumentation, moved into this ASI (`asi\trace.h`, the hook list `asi\hooks.inc`). quen's `tracy:NAME` launch profile turns it on (crates/quen/src/profile.rs).

**The trigger.** Only quen's tracy launch profile sets it: `W3_QUEN_TRACY=1` in the game's environment, beside `W3_QUEN_TOKEN`, `W3_QUEN_PID` and `TRACY_PORT`, the loopback port Tracy listens on. quen passes the game no other variable of its own environment, and none from the agent. The ASI reads it once, as it loads: `W3_QUEN_TRACY` exactly `1` and a `W3_QUEN_TOKEN` set.

**Without the trigger** none of it runs:
- no hook: no function's bytes change;
- no Tracy profiler and no thread: the client is built with `TRACY_DELAYED_INIT` and `TRACY_MANUAL_LIFETIME`, so they exist only from `tracy::StartupProfiler()`, which only the triggered path calls;
- no socket: Winsock isn't loaded (the ASI delay-loads `ws2_32.dll`, `user32.dll` and `secur32.dll`, which only Tracy uses; its load-time imports are 0.1.2's: `WitcherNativeBridge.asi`, `ADVAPI32.dll`, `KERNEL32.dll`);
- no allocation: the ASI's `operator new` uses `malloc`, as the default one does. Tracy's and Detours's object files have no load-time initializers (no `.CRT$XC*` / `.CRT$XI*` section), and the ASI's own are 0.1.2's four;
- thread notifications are switched off as in 0.1.2 (`DisableThreadLibraryCalls`).

**With the trigger**, still in `DllMain`:
- **The gates:** the exe's PE stamp must be witcher3.exe 5.01's (`0x6AC555A3`), else `tracy: REFUSED: ...` in `W3Quen.log` and nothing is hooked. Each function's first 16 bytes must be 5.01's, or a jmp rel32 (another hook, which Detours then chains: ours runs first); else it is skipped, with its live bytes in the log.
- **The hooks:** the 75 functions in `hooks.inc`, in one Detours transaction with every other thread of the process suspended. The ASI's `operator new` (Detours's allocations included) uses a private heap, so a suspended thread holding the process heap's lock can't deadlock the install (W3Trace's design). `W3Quen.log` gets one line per hook and `tracy: N of 75 hooks installed`.
- **Tracy** starts on a thread of its own: `tracy: started`, then `tracy: a capture connected` once a capture tool connects. quen's boot waits for that line.
- **The client:** Tracy 0.14.1, records only while a capture is connected (`TRACY_ON_DEMAND`), listens on 127.0.0.1 only (`TRACY_ONLY_LOCALHOST`), sends no broadcast (`TRACY_NO_BROADCAST`); no ETW, call stacks, frame images, code transfer or crash handler (`asi\build.bat` names each define and what it does in 0.14.1's source). It is never shut down.
- Thread notifications stay on.

**What it records**, as W3Trace did:
- one zone per call of each hooked function, named after it, except:
  - `CBaseEngine_Tick`: also the `game` frame mark and, once per tick, the plots `RenderRing_Alloc calls`, `RenderRing_Alloc us` and `engine fence wait (raw)`;
  - `RenderCmd_EndFrame_Exec`: also the `render` frame mark after the zone, then the per-frame counter plots;
  - `RenderDispatcher_JobPop`: the pop is a zone, the job after it a `Job` zone with its name and invoker as text;
  - `JobCounter_Wait`: `JobCounter_Wait_FrameJoin` from the end-of-frame join, else `JobCounter_Wait`;
  - `RenderRing_Alloc`: no zone per call, its time summed; calls of 20 us or more get a `RenderRing_Alloc_slow` zone;
  - `PT_LightSortLoop`: one zone for the outermost call of its recursion;
- zone texts from W3Trace's round-3 probes (`PT_LightList`, `PT_LightSort`, `LightBlock_Upload`, `Submit_Phase`, `Submit_Fixups`, `GpuCtx_Close`, `RTSBT_Build`, `RTCull_ProximityWalk`, `RTQueue_PushReady`), and the counters five of them keep, plotted per render frame;
- four call counters without a zone (`RTMat_SlotPush`, `RTSkin_ChunkDispatch`, `RTCollect_MeshProxy`, `Barrier_Add`), plotted per render frame.
- Thread names `main`, `render`, `dispatcher_N` (also the Windows thread descriptions).
- A zone's source file reads `W3Quen.asi`, not a path.

**Left out of W3Trace:** its GPU debug markers (and their three hooks, `GpuScope_Begin`, `GpuScope_End`, `GpuCtx_Reset`), its round-3 hash probes (its README: they cost time on the render thread), its ini (every hook is on) and its host-name gate.

**Other mods on the same functions.** Each line of `hooks.inc` cites the KB's names.tsv row and, where another mod changes the same site, its hooks.tsv row. A tracy launch parks those mods (it reads hooks.tsv when it plans). In hooks.tsv as of witcher3-mods 1af384a those are W3EngineTweaks (3 sites) and W3LightCache (`PT_LightSort`, 0x352130); its W3Trace rows name only the W3EngineTweaks sites.

## Build and test

- Needs MSVC (the newest Visual Studio with the C++ x64 tools, found with vswhere, or a `cl` already on PATH) and `WNB_SDK`: the folder of the Witcher Native Bridge release zip, holding `include\WitcherNativeBridgeApi.h` and `lib\WitcherNativeBridge.lib`. The SDK is not vendored here: its licence is unclear.
- `asi\build.bat` builds `out\W3Quen.asi`, compiling Tracy's client and Detours from `third_party\`; `asi\build.bat test` also runs the offline test; `python -I pack.py` does both and packs the zip.
- **Reproducible:** `/Brepro`, `/MT`, a zip with fixed dates and no compression. The same MSVC version gives the same bytes from any folder. 0.1.0, 0.1.1 and 0.1.2 were built with MSVC 14.50.35717 (Visual Studio 18.4): another version may give other bytes, so build releases with the version named here, or name the new one. 0.1.2's toolchain first rebuilt 0.1.1's ASI byte for byte, and 0.1.2's ASI built byte-identical in a second folder. 0.1.3 was built with MSVC 14.50.35726, which rebuilt 0.1.2's ASI byte for byte; 0.1.3's ASI built byte-identical in a second folder. 0.1.4 was built with MSVC 14.50.35717, which rebuilt 0.1.3's ASI byte for byte; 0.1.4's ASI built byte-identical in a second folder. 0.1.5 was built with MSVC 14.50.35717, which rebuilt 0.1.4's ASI byte for byte; 0.1.5's ASI built byte-identical in a second folder. 0.1.6 was built with MSVC 14.50.35717 (Visual Studio 18.4), which rebuilt 0.1.5's ASI byte for byte; 0.1.6's ASI built byte-identical in a second folder. 0.1.7 was built with MSVC 14.50.35717 (Visual Studio 18.4), which rebuilt 0.1.6's ASI byte for byte; 0.1.7's ASI built byte-identical in a second folder. 0.1.8 was built with MSVC 14.50.35717 (Visual Studio 18.4), which rebuilt 0.1.7's ASI byte for byte; 0.1.8's ASI built byte-identical in a second folder. 0.1.9 was built with MSVC 14.50.35717 (Visual Studio 18.4), which rebuilt 0.1.8's ASI byte for byte; 0.1.9's ASI built byte-identical in a second folder.
- **No build path in the ASI:** Tracy records a zone's source file, so `trace.h` names it `W3Quen.asi` (`TracyFile`) in place of `__FILE__`, and Tracy's asserts, which would hold their files' paths, are off (`NDEBUG` for the code built with Tracy's defines).
- **CI doesn't build or test this folder:** it needs MSVC and the SDK. Build and test it locally before a release.

The test (`test.exe`) loads the real `W3Quen.asi` next to a stand-in `WitcherNativeBridge.asi` (`wnb_stub.cpp`) that runs the natives on a fake frame. It plays both ends, quen's pipe client and the script, in five modes, each in its own process:
- `channel`:
  - the camera paths: `cam.path`'s checks (each refusal of protocol.rs's and sweep.rs's tests), the longest path (24 keyframes, 3,261 bytes), and `QUEN_CamAt` against all 21 of `sweep::tests::reference_vectors`, played as frame step at 4 fps so each vector's time is a frame (0.1.2: every value equal at 6 decimals), real time by the clock and holding at the end;
  - every verb in `protocol.rs`'s wire shapes, to the script line and back, the photo state's read-back to JSON included (from 0.1.9);
  - the refusals from its tests and the ones above;
  - a request exactly at the cap;
  - ttl, the queue cap, a missing reply, stale handles, unparsable read-back, an oversize or non-UTF-8 reply;
  - reconnects, and a client dropped for an over-long line;
  - the server pid and the DACL;
  - `QUEN_State` lines in `W3QuenState.log`, and the poll count before the first.
- `notoken`: no `W3_QUEN_TOKEN` gives no pipe, an empty `QUEN_Next`, and neither log written.
- `badtoken`: a token with path characters gives no pipe.
- `wrongpid`: a client that isn't `W3_QUEN_PID` is dropped.
- `squat`: a pipe name created first is refused.

The Tracy modes (from 0.1.3). `test.exe` has an empty 41 MiB region (`.bss`, no file bytes) that spans every hook's RVA, and each mode first writes each hook's 16 entry bytes at its RVA there, so a triggered ASI hooks them all. At `CRenderFence_Signal`'s RVA the test writes a whole small function (its 16 entry bytes, then the rest of `return a0 + 0x10`). `mkhost` writes `tracyhost.exe`: `test.exe` with witcher3.exe 5.01's PE stamp. Each mode runs in its own process:
- `tracy-stamp` (`test.exe`, whose stamp isn't 5.01's): the full trigger is refused, `tracy: REFUSED` in the log, and it stays dormant.
- `tracy-none` (no variable), `tracy-token` (a token and pid, no trigger: a plain quen launch) and `tracy-notoken` (`W3_QUEN_TRACY=1` without a token), in `tracyhost.exe`. Dormant, checked after the ASI loaded:
  - every hook site's 16 bytes as written;
  - no thread whose description starts with `Tracy`;
  - `ws2_32.dll` not loaded;
  - no TCP or UDP endpoint of the process, IPv4 or IPv6;
  - no new heap;
  - the function at `CRenderFence_Signal` returns the right value;
  - no `tracy:` line in `W3Quen.log` (with a token), or no log written (without).
- `tracy-on` (`tracyhost.exe`, full trigger, `TRACY_PORT` a free port), the positive control of each check:
  - `75 of 75 hooks installed` and `tracy: started` in the log;
  - all 75 sites changed, each starting with a jmp (E9);
  - a new heap (the private one);
  - one `Tracy` thread (`Tracy Profiler`: no symbol or compression thread);
  - one endpoint: TCP, listening on 127.0.0.1:`TRACY_PORT`, nothing on another address, no IPv6, no UDP;
  - 1,000 calls of the function at `CRenderFence_Signal` through the ASI's wrapper, Tracy's zone code and Detours's trampoline, each with the right value.
- `tracy-capture` (only with `TRACY_CAPTURE`, Tracy 0.14.1's `tracy-capture.exe`): as `tracy-on`, then the real capture tool connects, `tracy: a capture connected` is logged, 100 calls are made, and the tool writes a capture after 3 s. With `tracy-csvexport.exe` beside it, its zone list holds `CRenderFence_Signal`.
- 2026-10-19: every mode passed, `tracy-capture` with Tracy 0.14.1's tools (`tracy-capture 0.14.1 / 30997d5`): 100 `CRenderFence_Signal` zones in the capture.

**Mutation checks.** Each copy had one safety point broken, and each failed the test:
- 2026-10-19 (0.1.10): with `items.add`'s count of 0 accepted, `channel` failed.
- 2026-10-17, in witcher3-mods: without `FILE_FLAG_FIRST_PIPE_INSTANCE` the ASI joined the squatter's pipe; without the pid check the wrong client was served; with an Everyone DACL the ACL check failed; without the ttl check the expired command ran.
- 2026-10-17, here: without the token check in `QUEN_State`, `notoken` failed (the state log was written); without the poll count, `channel` failed; without the first poll's tick (0.1.1), `channel` failed.
- 2026-10-18 (0.1.2): with squad's blend weight `2u(1-u)` changed to `u(1-u)`, `channel` failed 82 checks against the reference vectors.
- 2026-10-19 (0.1.3): with the trigger's token check removed, `tracy-notoken` failed every dormancy check (the hook bytes, a `Tracy` thread, Winsock loaded, a new heap, a listening socket, a log written). With the trigger always on, `notoken` failed (its log written), and `tracy-none` and `tracy-token` failed every dormancy check (Tracy listening on 127.0.0.1:8086, its default port without `TRACY_PORT`).
- 2026-10-19 (0.1.9): each of these failed `channel`: the photo read-back's `grading_at_open` made from `grading`'s numbers; `photo.tick` taking a negative time; `photo.mode` taken without its `on`.

## Offline script compile

REDkit's `wcc_lite compilescripts <dir> -out=<dir>` compiles a script tree without the game. Run it with `The Witcher 3 REDkit\bin\x64_RedKit` as the working folder, on a scratch copy of the game's `content\content0\scripts` plus the mods' scripts. Never point it at the game folder.

Checked 2026-10-17 for 0.1.0 and 0.1.1, 2026-10-18 for 0.1.2 and 2026-10-19 for 0.1.3, whose `quen.ws` is 0.1.2's, 0.1.4, 0.1.5, 0.1.6, 0.1.7, 0.1.8, 0.1.9 and 0.1.10 (the same four results; as shipped, seven imports can't bind, `QUEN_CamAt` the new one in 0.1.2), with `quen.ws`, the Native Bridge's `Mods\modNativeBridge\content\scripts\imports.ws` and the W3 Game State Bridge's `w3gsb.ws`, about 40 s per run:
- **As shipped:** exit 1, and the only errors are the six `import function`s that can't bind (`QUEN_Next`, `QUEN_Reply`, `QUEN_State`, `W3GSB_Push`, `WNB_GetApiVersion`, `WNB_StringToName`). wcc_lite has no Native Bridge, so import binding can only be checked in the game.
- **Imports replaced by plain stub functions:** `Success! Scripts cooked`, exit 0, 362 warnings, none in these files. So there are no duplicate names, and quen's wraps compile beside W3GSB's, which wraps the same six events.
- **The same plus every other script mod installed on the test machine:** `Success`, exit 0, 362 warnings (for 0.1.8, one more installed mod's own `import function`s stubbed the same way). No other mod wraps any of them.
- **Control** (an unknown call planted in `quen.ws`): exit 1, reported at its line, so the file is type-checked.

## Known limits and open questions

- **Settled only by the parity boot** (quen's package beside the W3 Game State Bridge, comparing their states):
  - does quen's state match W3GSB's from the same boot?
  - does any tick run at the main menu (the poll count)?
  - do two `OnGameEnded` wraps chain like two `OnTick` wraps?
  - do the natives register (`native registration: QUEN_State=OK` and the others in `WitcherNativeBridge.log`) and do strings move both ways?
- **WNB leaks string parameters:** it has no call to free them, so each `QUEN_Reply` leaks its text and data. The `listsaves` list leaks once per call.
- **Per-frame cost:** `QUEN_Next` runs on every game tick (one native call and a 1-byte engine string), and the state poll 4 times a second. Not measured.
- **Photo mode:** `settime` with photo mode open moved the sun at once (2026-10-19, hours 08 to 16). `weather` with photo mode open is untested.
- **Items, untested in the game (0.1.10):** whether `WNB_StringToName` makes the name the item tables know, what `AddAnItem` does with a count over one of a non-stackable item, and whether `IsItemEquipped` reads an equip back on the same tick.
- **The notice, untested in the game (0.1.8):** whether `ShowNotification` shows in photo mode, and over the focus-loss pause menu.
- **Photo mode's interface, untested in the game (0.1.9):**
  - whether the added method can call `EnablePhotomode` (private: the offline compile accepts it), and the menu's `OnConfirmExit` from the channel's tick;
  - what `GetWeather` and `GetTimeOfDay` read back (the name and minutes are what the menu passes; `GetWeather` after `RestoreWeather` isn't compared), and what `GetPath` gives for an env;
  - whether photo mode's env outlasts it (quen journals it as tainting; its time and weather didn't, 2026-10-19), and whether the weather blend runs in photo mode;
  - what `SetPhotomodeTickTime` does with more than the menu's 0.00001 s;
  - how many paths `GetAllEnvironmentDefinitions` holds (`photo.list` refuses over 60,000 characters).
- **The environment's blend:** nothing in the game's scripts reads the blend weight (`engine\environment.ws` has `GetWeatherConditionName`, the blended `GetRainStrength` and `GetSnowStrength`, `IsSkyClear`, the blend-time setting `GetWeatherBlendTime` and `GetPredictedTimeUntilWeatherChange`), so no verb can say "settled". The weather's name flips at once; rain and snow ramp, and `world.get` reads them.
- **No blend guard on `cam.set`:** a location-preset blend that the user starts by hand in photo mode would move the camera away. Guarding it needs the menu instance, whose `m_cameraBlendInProgress` is private. No blend fought a set in the probe boot.
- **Pause across a load:** whether quen's pause survives loading a save is untested. Send `pause off` before a load.
- **Camera paths, untested in the game (0.1.2):** whether photo mode's menu (its FOV slider, at its own 40 Hz tick) overrides a per-tick `SetFov`; whether touching the mouse or pad during playback fights the per-tick set; the per-tick cost of `QUEN_CamAt` (one native call, a few slerps, a 7-number string the script splits).
- **`save` in the game** (0.1.5, feature tests 4, 2026-10-19): `SaveGame` from the channel's tick wrote one new manual save per call (its 3 files 2.1 s after the reply), also while the focus-loss pause menu was open behind the client; that save's `.png` shows the scene, not the menu. Untested: whether the stale dialogue flag (512) refuses saves that should work.
- **The menu bit, untested in the game (0.1.6):** whether `IsAnyMenu()` is true while the focus-loss pause menu is open (quen's `focus` closes the menu by it), and whether it is in photo mode (quen sends no escape there either way).
- **Tracy, untested in the game (0.1.3):** its hooks and wrappers are W3Trace's, which ran in the game (W3Trace's logs); this build of them has run offline only. The cost of the zones in the game is not measured: W3Trace's README gives about 30 ns per recorded zone (offline, a capture connected), and estimates of tens to thousands of calls per frame per function. The capture runs from the launch to `session_end`, so its size grows with the session.
