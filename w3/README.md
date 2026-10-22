# quen's Witcher 3 package: `W3Quen.asi` + `modQuen`

quen's only game-side part for The Witcher 3 (5.01, DX12): one native ASI and one script mod, always installed together. It gives quen twelve things:
- **Game state:** seven on/off values (gameplay, scene, cutscene, photo mode, fade or loading, dialogue, a menu open), written to `W3QuenState.log` next to `witcher3.exe`.
- **A command channel:** typed commands from quen over a named pipe, run by the script on the game's tick, with replies.
- **Camera paths:** a whole camera path from quen, played on the photo-mode camera with one set per game tick, in real time or frame by frame.
- **Console commands** to load a save by name, which quen types only when the command channel isn't up.
- **Tracy instrumentation** (from 0.1.3), only in quen's `tracy` launches: Tracy zones on 75 engine functions, ported from the W3Trace ASI (section Tracy, below). In any other launch none of it runs.
- **A new manual save** (from 0.1.4) on quen's `save` verb, as the game's menu writes one into its "Empty save slot".
- **An on-screen notice** (from 0.1.8) on quen's `notice` verb: quen's own short text, shown for 8 s; from 0.1.15 for the time quen gives (quen's lasting status line), taken down on `notice.hide`, beside a game sound event on `sound`; from 0.1.16 the lasting line in a HUD movie of quen's own where the HUD is drawn (section HUD line).
- **Photo mode's own interface** (from 0.1.9): photo mode opened and closed by the game's own functions, and its weather, time, environment and grading set as its menu sets them, each read back (`photo.*` verbs).
- **Items** (from 0.1.10): an item added to the player's inventory by its name, as the console's `additem` adds one, equipped if asked, its count read back (`items.add`).
- **Nearby actors** (from 0.1.10): the alive actors near the player, nearest first, each one's position, heading, velocity, names and flags (`world.actors`, read only).
- **The scene** (from 0.1.13): what the rendered camera sees, the ground at points, rays and sphere sweeps, navmesh lanes (`scene.frame`, `scene.ground`, `scene.ray`, `nav.line`, read only), and the photo-mode camera's no-clip (`cam.noclip`).
- **Exec functions** (from 0.1.13): a global `exec function` called by name with its arguments, as the console runs one, without typing: the ASI calls the game's console executor (`exec`, section Exec).

Status 2026-10-19: 0.1.0 ran in live boot 2 (the natives registered, the state matched the W3 Game State Bridge's at gameplay, but photo mode never showed). 0.1.1 fixed photo mode and ran from live boot 3. 0.1.2 adds the camera paths (`cam.path`, `cam.frame`, `cam.stop` and the native `QUEN_CamAt`); it is built and tested offline (below), not yet run in the game. 0.1.3 adds the Tracy instrumentation; it too is built and tested offline only. Its `quen.ws` is 0.1.2's, byte for byte. 0.1.4 (2026-10-19) adds the `save` verb to both files; built and tested offline only. 0.1.5 (2026-10-19) adds the read-only `world.get` verb to both files; built and tested offline only. 0.1.5 ran in the game in feature tests 4 (2026-10-19): Tracy, `save`, `world.get`. 0.1.6 (2026-10-19) adds the menu bit (2048) to the state, and W3Quen.log's natives line names the release; built and tested offline only, and never published. 0.1.7 (2026-10-19) supersedes it with the same behaviour: only comments and two reply texts (`cam.set` and `cam.path` outside photo mode) are reworded, in both files; built and tested offline only. 0.1.8 (2026-10-19) adds the `notice` verb to both files (quen shows it when it hands the controls to the user); built and tested offline only. 0.1.9 (2026-10-19) adds the `photo.*` verbs, and rain, snow and the sky flag to `world.get`, in both files; built and tested offline only. 0.1.10 (2026-10-19, rebuilt 2026-10-20 before its release) adds the `items.add` and `world.actors` verbs to both files; built and tested offline only. 0.1.12 (2026-10-20) adds `world.actors`' `tag` and `class` and each entity's id and actor flag, in both files; built and tested offline only. 0.1.13 (2026-10-21) adds the scene reads and `cam.noclip` to both files and the exec verb to the ASI; built and tested offline only. 0.1.14 (2026-10-21) changes `scene.frame`'s box for an actor with a head bone to one around its body (finding 89), in `quen.ws`; its ASI differs from 0.1.13's only in its version; built and tested offline only. 0.1.15 (2026-10-21) adds `notice`'s `duration_ms` and the `notice.hide` and `sound` verbs to both files, for quen's lasting status line; built and tested offline only. 0.1.16 (2026-10-22) adds quen's own HUD movie for the lasting line (modQuen's new `content\blob0.bundle` and `metadata.store`, and `README.txt`) and its use by `notice` and `notice.hide` in `quen.ws`; its ASI differs from 0.1.15's only in its version; built and tested offline only.

The sources moved here from the maintainer's `witcher3-mods` repo (its `asi\quen` and `content\modQuen`, commit d74d3a5), where modQuenRpc was a separate script mod. It is now folded into modQuen.

## This folder

| Path | What |
|---|---|
| `asi\W3Quen.cpp` | the ASI: registers `QUEN_Next`, `QUEN_Reply`, `QUEN_State` and `QUEN_CamAt` through the Witcher Native Bridge, writes the two logs, serves the pipe, checks and interpolates camera paths, calls the console executor for `exec` |
| `asi\trace.h`, `asi\hooks.inc` | the Tracy instrumentation, the functions it hooks (quen reads that list too), and the one the ASI calls (`W3_CALL`) |
| `asi\build.bat`, `asi\test.cpp`, `asi\wnb_stub.cpp` | build and offline test |
| `third_party\` | Tracy 0.14.1's client and Microsoft Detours 4.0.1, compiled into the ASI (versions and licences: its README) |
| `mod\modQuen\content\scripts\local\quen.ws` | the one script: state, channel, camera paths, photo mode, console commands |
| `mod\modQuen\README.txt` | the notice REDkit's licence asks of everything made with it, and what modQuen is |
| `hud\quen\QuenHud.as` | quen's HUD movie, ActionScript 3 only, no art (section HUD line) |
| `hud\extern\` | compile-time declarations of four Adobe easing classes the game's HUD classes import; never in a movie |
| `hud.py` | builds the HUD movie into modQuen's bundle (section HUD line) |
| `pack.py` | builds, tests and packs the release zip |
| `probes\walkers\` | a throwaway probe mod, never packed: do spawned NPCs walk where scripts tell them (its README) |
| `probes\indicator\` | a throwaway probe mod and its fallback, never packed: can the game's own notification and a revived HUD module carry quen's lasting status line (its README) |
| `out\` | build output and zips (ignored by git, never committed) |

## Release

- Releases are GitHub Release assets of this repo: `quen-w3-<version>.zip`, built by `pack.py`. Its root mirrors the game folder:
  ```
  bin\x64_dx12\W3Quen.asi
  bin\x64_dx12\W3Quen-licences.txt
  Mods\modQuen\content\scripts\local\quen.ws
  Mods\modQuen\content\blob0.bundle        (from 0.1.16)
  Mods\modQuen\content\metadata.store      (from 0.1.16)
  Mods\modQuen\README.txt                  (from 0.1.16)
  ```
- `W3Quen-licences.txt` (from 0.1.3) holds the notices of the code compiled into the ASI: Tracy's client (3-clause BSD) and the libraries inside it (moodycamel::ConcurrentQueue, SPSCQueue, LZ4), and Detours (MIT). Each licence asks for its notice to come with the binary.
- `README.txt` (from 0.1.16) starts with the notice REDkit's licence (`wcc_eula.rtf`) asks of anything made with it and distributed: the bundle is packed by its wcc_lite. The licence forbids selling what is made with it and gives CD PROJEKT RED a free licence to it; quen-w3 is free.
- Each release's notes give the sha256 of the pinned files and of the zip. quen's doctor pins `W3Quen.asi` and `quen.ws` exactly, so a half from another version is refused before a launch, and from 0.1.16 the bundle's two files of the ASI's release.
- The zip bundles neither the Native Bridge nor an ASI loader: a bundled copy could overwrite a newer one on patch day.

| Version | `W3Quen.asi` | `quen.ws` | zip |
|---|---|---|---|
| 0.1.16 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's; `blob0.bundle` `7592bfafef3c54ff258940f0c732a47e90903b3c4e5e74537a858d7269a1f2c1`; `metadata.store` `7d05a5ee2297d231e71afe10a567a499ff23d78a769d5fa2df01804d120dab7c`; `README.txt` `381f1c2ac52f9c872db5ef51b0bf0243a5f8b40bd14b0007c956462e571f6c1a`) | `75f2d862378e8f53dfd6290c906a0ed9a91c57c8a1a0838a1556faa44f1ecbb2` | `2589964c0deec1d9708b10951ad2160e914954e29c8a3a37a7c35a3581c78dcb` | `b01487fcdabf3e5023fc46cd5597fe0d488b09463dfb7af7f411343fc03e110e` |
| 0.1.15 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `2eea072f4da8705f07d3369b24cb4969023422feee33f8c0fb1631121b8efea3` | `a86e13513df2e80354684daaa598719b6330fde1ff4fd9ca222ca57fc7954160` | `a13324d1fb2a3aec6439f0f6ac0a6ffa6867f6f4043355181c289f6ca430867c` |
| 0.1.14 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `a12fb3b5d672af664ae7e31b3bf8bd79eab6d0b90df552d8f479bb394b425f1e` | `97040312eac5b83dab98eb639f61d6514804241e56f29ad38fdf6c53e9bd33f6` | `b2593d74452243310bc476036f882ad04aa6c36810f56546e46e53db34a979d4` |
| 0.1.13 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `4c2438d779d41b7f9ecdf7dc9906ceebd4eaaf1281f4fa2fc9fbd31a7cfd2225` | `c1c456b639f5f4167b4044385b5cbb2147b8a90171f4c64d279957fea0bd725a` | `bc443ec7666b96fc68bde7eef2437ab605a0fb1c329e2584c56b527e8374bdde` |
| 0.1.12 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `2fe14569a09db76a0c6445adb71db687044d6b9338b7f885b072522f191b3879` | `a2caa723d08d638e2d7c5afc648b9b25ecd121f9ad693a3de2c70f16b956ff5f` | `053b4a862e772b656e1072280e5a6433b4c8e9d377112c1edd331443f8024fec` |
| 0.1.11 (not published yet; `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's) | `7a54ad149cb4c339c6795798d3ca6a1b157f09954ce57327ddbd40e75fb08627` | `fd9622f088568ef45f1809773bd51a8b7a728ba7b0cd88de14ef0040a45fc957` | `37706b9e5a0d23b5da5e8ae1aec5056f2b07044cf717d405b8fa9c4fcf914a58` |
| 0.1.10 ( `W3Quen-licences.txt` `196a9a7d8e519db02a6dbfed3232ff1636f16051ea733a86aa9c9148cec51f07`, 0.1.3's; rebuilt with `world.actors` before its release, the earlier build never published) | `2f8c0359e7935bdca896107bb0f003ee7891325d26ae1fefd76a1d060021647d` | `f5b38b8bdf4ddb4f7c38366bff59aad4aeb662960695ac324442e678f9463f15` | `4e3b77c65e545da7d437ebdfdbd6a897b4ae87eb6e052a159ddc3e953f6d092b` |
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
- The ASI checks every request as `decode_request` does: unknown verbs and fields, wrong types, duplicate keys, invalid UTF-8, non-finite numbers, an empty name, hour past 23 and fov not above 0 are all refused; from 0.1.9 also a `photo.time` minute outside 0 to below 1440, a `photo.grade` or `photo.tick` without a value, a `photo.tick` time outside 0 to 10 s, and a `photo.list` filter that is empty or over 200 bytes; from 0.1.10 also an `items.add` name over 200 bytes or count outside 1 to 1000, and a `world.actors` radius not above 0 or over 100, or a max outside 1 to 50; from 0.1.12 also a `world.actors` `tag` or `class` other than 1 to 64 letters, digits and `_`; from 0.1.13 also a `scene.frame` radius not above 0 or over 100, a max outside 1 to 16, points outside 0 to 3, an `at` not 3 coordinates or an `entity_id` that isn't a whole number in int's range, a `scene.ground`, `scene.ray` or `nav.line` without 1 to 16 points or segments of coordinates below 100,000 in size, a range not above 0 or over 100, a radius over 10 (`scene.ray`'s over 2), a search over 50 or a width over 5, and an `exec` function other than 1 to 64 letters, digits and `_`, over 8 args, an arg over 100 bytes, with a quote or a byte outside printable ASCII, or a line over 512 bytes. It also refuses a control character in a name, and a `settime` day above 2^31-1, the script's int range. A refusal is a reply with `"ok":false`, carrying the request's id, or id 0 if the id itself was unreadable.
- Each request's `ttl_ms` is checked when the script polls. An older command is answered `expired: waited N ms` and never runs.
- At most 16 commands wait. A disconnect drops what the client left queued.
- A command handed to the script that gets no `QUEN_Reply` (a script error) is answered `modQuen gave no reply` at the next poll.
- Reply `data`:
  - `listsaves`: an array of `SSavegameInfo.filename`;
  - `cam.get`, `cam.set`, `cam.path` and `cam.frame`: `{"camera":"photomode"|"game","pos":[x,y,z],"rot":[pitch,yaw,roll],"fov":f}`. `rot` is the scripts' `EulerAngles` order, in degrees, the same order the requests use. After a set it is the photo-mode camera's own read-back.
  - `world.get`: `{"player":{"pos":[x,y,z],"heading":heading},"time":{"day":day,"hour":hour},"rain":r,"snow":s,"sky_clear":b,"weather":"<name>"}`, from the script's `x y z heading day hour rain snow skyclear weather` (floats with 4 decimals; skyclear 0 or 1). `rain`, `snow` and `sky_clear` are from 0.1.9. Anything else, an hour past 23 included, is the read-back that did not parse.
  - `photo.list`: an array, as `listsaves`'s (from `a|b|c`).
  - `scene.frame` (from 0.1.13): `{"found":n,"camera":{"pos":[..],"rot":[..],"fov":f},"entities":[{"id":i,"name":..,"entity":..,"entity_cut":b,"player":b,"pos":[..],"heading":h,"head":[..] or null,"torso":[..] or null,"in_front":b,"x":x or null,"y":y or null,"share":s,"box_whole":b,"distance":d,"visible":{"head":b or null,"torso":b or null,"origin":b or null},"visible_points":n}]}`, from the script's `<found> <camera x y z pitch yaw roll fov>` then per entity `|<flags> <id> <x> <y> <share> <distance> <x y z> <heading> <head x y z> <torso x y z>|<name>|<entity>` (flags 1 centre in front, 2 the whole box in front, 4 the player, 8 a head bone, 16 a torso bone, 32 / 64 / 128 a clear ray to head / torso / origin, 256 / 512 / 1024 that ray cast, 2048 entity cut, 4096 `IsInView()`), `"in_view":b` after `in_front`. At most 16 entities.
  - `scene.ground`, `scene.ray` and `nav.line` (from 0.1.13): an array, one object per point or segment, from the script's `<n>` then per one `|<flags> <numbers>`: `{"nav_z":z or null,"ground":{"pos":[..],"normal":[..]} or null,"water_level":w,"water_depth":d,"free":b or null,"safe":[..] or null}` (12 numbers; flags 1 navmesh z, 2 a ground hit, 4 free, 8 the circle tested, 16 a safe spot), `{"hit":b,"pos":[..] or null,"normal":[..] or null,"distance":d or null}` (7; flag 1 a hit), `{"clear":b,"clear_in_direction":b,"reach":[..],"edge_m":e}` (4; flags 1 clear, 2 in direction).
  - `exec` (from 0.1.13): `{"line":"<the line called>","log":"<what the engine logged on its main thread during the call>"}`, also when nothing was called (section Exec).
  - `items.add` (from 0.1.10): `{"before":b,"after":a,"equipped":e}`, from the script's `<before> <after> <0|1>`: the item's count in the player's inventory before and after the add, and whether the first one added reads back as equipped. Anything else is the read-back that did not parse.
  - `world.actors` (from 0.1.10): `{"found":n,"actors":[{"name":..,"entity":..,"pos":[x,y,z],"heading":h,"velocity":[x,y,z] or null,"distance":d,"combat":b,"hostile":b,"guard":b,"at_work":b,"action_point":b,"scene":b,"entity_cut":b,"id":i,"actor":b}]}` (`entity_cut` from 0.1.11, `id` and `actor` from 0.1.12), from the script's `<found>` then per actor `|x y z heading vx vy vz distance flags id|<name>|<entity>` (flags 1 combat, 2 hostile, 4 guard, 8 at work, 16 action point, 32 scene, 64 velocity known, 128 entity cut, 256 not an actor; `velocity` null without 64; `id` a whole number in the script's int range; 0.1.11's nine numbers don't parse). Anything else, more actors than found included, is the read-back that did not parse.
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
| `notice` | `text`, 1 to 160 bytes, no control character, and from 0.1.15 `duration_ms`, 1 to 3600000 (none: 8000); the ASI checks both and passes `<duration_ms> <text>`. Changes nothing. To 0.1.14: `GetGuiManager().ShowNotification(text, 8000)`, as `QUEN_Say` shows its messages; refused without a GUI manager. From 0.1.15: the overlay popup's `ShowNotification(text, duration, false)` (queue false: it replaces a notification on screen), refused without an overlay popup (the GUI manager's call would request one with the text, which could then show after a hide found none); "notice already shown" and nothing called while the popup's `notificationModule` is visible with its `tfMessage` holding the same text, so quen's re-send doesn't take its line down and up. From 0.1.16 a notice of a minute or more (quen's lasting line) goes on quen's HUD movie where the HUD draws it, with the same "notice already shown", the notification taken down when it holds quen's line; else, and for a shorter one, on the notification as before, the movie hidden; a game state change moves the lasting line (section HUD line) | `ShowNotification(text, 3600000)` showed at +9 s and +62 s, in gameplay, photo mode and over the pause menu; a vanilla notification and the inventory key cleared it (the indicator probe boot, 2026-10-21) | new in 0.1.8; the duration in 0.1.15; the HUD movie in 0.1.16 |
| `notice.hide` | no fields. The overlay popup's `ClearNotificationsQueue()`: the 0.3 s fade of `hideNotification`, and a queued notification dropped. No overlay popup: ok, nothing to hide. From 0.1.16 also quen's HUD movie, hidden within the call, and the lasting line forgotten | read hidden 0.312 s after the call; a screenshot ~0.8 s after showed none (the indicator probe boot) | new in 0.1.15; the HUD movie in 0.1.16 |
| `sound` | `event`, 1 to 64 letters, digits and `_` (the ASI checks it). `theSound.SoundEvent(event)`, as the GUI plays its sounds; an unknown name plays nothing, unseen by the script | — | new in 0.1.15 |
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
| `world.actors` | `radius` (above 0 to 100), `max` (1 to 50; the ASI checks both); from 0.1.12 `tag` and `class` (1 to 64 letters, digits and `_`; the script gets `<max> <radius> <tag> <class>`, `-` for the one not given, made names by `WNB_StringToName`); changes nothing. Refused with no player. With a class: `FindGameplayEntitiesInSphere(entities, the player's position, radius, 1000000, tag, 0, NULL, class)` (`gamectorsStorage.ws`: gameplay entities of that class, actors or not, no query flag), and a non-actor gets flag 256, no velocity and no actor flag. Else `GetActorsInRange(thePlayer, radius, 0, tag, true)` (`game\actorsStorage.ws`: alive actors, no count cap), the player left out, sorted by `SortNodesByDistance` from the player's position (`engine\node.ws`), the first `max`. Per actor: `GetWorldPosition()`, `GetHeading()`, `GetMovingAgentComponent().GetVelocity()` (`movingAgentComponent.ws`), `VecDistance` to the player, `GetDisplayName()` (`gameplayEntity.ws`) cut at 80 characters and `GetReadableName()` (`entity.ws`: the text after the last `::` of `ToString()`) at 255 (0.1.10: 80, without the flag), each with a `|` made `/`; `IsInCombat()`, `GetAttitude(thePlayer) == AIA_Hostile`, `IsInGameplayScene()` (`actor.ws`), and for a `CNewNPC` `GetNPCType() == ENGT_Guard`, `IsAtWork()`, `IsAPValid(GetActiveActionPoint())` (`npc.ws`, `actionPointManager.ws`); from 0.1.12 `GetGuidHash()` (`entity.ws`). Reply `data` | — | new in 0.1.10 |
| `items.add` | `name` (the item's own name, as the console's `additem` takes it; spaces allowed), 1 to 200 bytes, no control character; `count`, 1 to 1000; `equip` (the ASI checks them). Refused with no player. As vanilla `exec function additem` (`game\temp.ws`): the name made by the Native Bridge's `WNB_StringToName`, `thePlayer.inv.AddAnItem(name, count)`, and with `equip` `thePlayer.EquipItem` on the first id when `IsIdValid`, read back by `GetWitcherPlayer().IsItemEquipped`. `GetItemQuantityByName` before and after: refused when nothing was added (`AddAnItem` returns an invalid id for a name no item has, `inventoryComponent.ws`). Reply `data` | — | new in 0.1.10 |
| `scene.frame` | `radius` (above 0 to 100), `max` (1 to 16), `points` (0 to 3); optional `at` (the centre, default the rendered camera) and `entity_id` (only that entity's `GetGuidHash`; the request's own `id` is taken); changes nothing, refused with no player. `FindGameplayEntitiesInSphere(entities, centre, radius, 1000000, '', FLAG_OnlyAliveActors)`, the player added if missing, `SortNodesByDistance` from the centre, the first `max`. Per entity: `theCamera.WorldVectorToViewRatio` of its box's centre and 8 corners (`engine\cameraDirector.ws`; share: the corners in front, their rectangle clipped to -1..1, over 4); the box, from 0.14 for an actor with a head bone: from its position's z to 0.15 m above the head bone, 0.3 m each way along `theCamera.GetCameraRight()` flattened and the horizontal axis across it; else, and before 0.14, `CalcBoundingBox`'s, which holds every component of the entity (boot 190: 2 to 5 times the body); `GetBoneWorldPositionByIndex` of `GetHeadBoneIndex` and `GetTorsoBoneIndex` and `IsInView()` (actors; `actor.ws`, never called by vanilla); per point asked a `StaticTrace` from `theCamera.GetCameraPosition()` over the collision groups below, clear when it hits nothing | — | new in 0.1.13 |
| `scene.ground` | `xyz` (1 to 16 points, flat), `range` (above 0 to 100), `radius` (0 to 10), `search` (0 to 50); changes nothing. Per point `NavigationComputeZ(point, z - range, z + range)`, a `StaticTrace` from z + range down to z - range, `GetWaterLevel`, `GetWaterDepth` (`engine\world.ws`); from the navmesh z with radius `NavigationCircleTest(radius)`, with search `NavigationFindSafeSpot(radius, search)` | — | new in 0.1.13 |
| `scene.ray` | `segments` (1 to 16, flat: ax ay az bx by bz), `radius` (0 to 2); changes nothing. Per segment `StaticTrace(a, b)`, or with radius `SweepTest(a, b, radius)` | — | new in 0.1.13 |
| `nav.line` | `segments` (as `scene.ray`'s), `width` (0 to 5); changes nothing. Per segment `NavigationLineTest`, `NavigationClearLineInDirection` and `NavigationClosestObstacleToLine` (vanilla never calls the last), width as their radius | — | new in 0.1.13 |
| `cam.noclip` | `on`. Photo mode only. On: `GetPhotomodeCamera().SetNoclip(true)` (as photo mode's menu does before it blends to a saved location), kept until photo mode's menu closes: then, and on `on: false`, back to the menu's own Noclip setting (its `m_UIValues[PM_Camera_Noclip]`, through an added method; off without the menu) | — | new in 0.1.13 |
| `exec` | `function` (1 to 64 letters, digits and `_`), `args` (0 to 8 strings of printable ASCII without quotes, 100 bytes each; the line at most 512 bytes). Never to the script: the ASI calls the console executor (section Exec) | — | new in 0.1.13 |

The scene verbs' rays and sweeps test the collision groups `Static`, `Terrain`, `Foliage`, `RigidBody`, `Dynamic`, `Destructible`, `Door`, `Platforms`, `Fence`, `Boat` and `BoatDocking` (photo mode's own raycast list without `Phantom`, `Water` and `Debris`, with `Foliage`): no `Character`, so a ray to a body's point isn't stopped by that body. One scene verb is one tick: at most 48 rays (`scene.frame`) or 16 points or segments, under the runner design's caps of 64 rays and 16 queries a tick.

Every `photo.*` verb but `photo.mode` is refused unless photo mode is on and its menu is open: the menu turns `CR4PhotomodeEffects` on as it opens (`SetEnabled(true)` in `OnMenuShown`) and off as it closes, and modQuen keeps the menu from its `OnMenuShown` wrap to its `OnClosingMenu` wrap. Every set reads the whole photo state back after it. The menu's sliders don't show what quen sets.

### Camera paths

- **quen sends the whole path once:** `cam.path` with quen-core `sweep.rs`'s canonical keyframes (protocol.rs's message). The ASI checks it as protocol.rs does (`check_playback`, then `Path::new`'s checks, then canonical keyframes) and precomputes it, on the pipe thread. Its port of `Path` is line for line and in double precision, so quen's own sample of a frame and the game's are the same numbers.
- **The script gets** `<handle> cam.path <step 0|1> <frames>` and sets the first pose. From then `CR4Game.OnTick` asks `QUEN_CamAt(frame)` for the tick's pose (`x y z pitch yaw roll fov`, 6 decimals) and sets the photo-mode camera to it, as `cam.set` does (`SetFov`, `SetCameraWorldPosition/Rotation`, `UpdateWithoutInput(true)`).
  - **Real time** (`frame` -1): the time since the path's first pose, by the performance counter. Not `GetEngineTimeAsSeconds()`, which moved in 31.25 ms steps in the probe boot. After the end the last pose holds.
  - **Frame step:** frame n is the path at n / fps, computed and never summed. The pose is set again on every tick, so it holds while quen captures.
- **What stops a path:** a new `cam.path` (it replaces it), `cam.set`, `cam.stop`, leaving photo mode. quen leaving the pipe doesn't: a real-time path plays on between quen's verbs.
- The yaw of a pose is unwrapped along the path (a 360° pan runs to 530°): the game takes any yaw.

### Exec (from 0.1.13)

- **What runs:** the game's console executor, `RemoteConsole_RunCommand` at RVA 0x24DD370 (`hooks.inc`'s `W3_CALL` row; witcher3-mods `re\notes\console_exec.md`, static RE, not live-checked). It is the REDnet remote console's runner: it finds the global function by name among the exec functions, checks the argument count, converts each argument from text, calls it with no context, and copies the lines its own thread logged meanwhile (up to 2000 characters) into an out string. It reads no `-net` state, and no console switch.
- **The line:** built from `function` and `args` as quen-core's `Verb::exec_line` builds it: `function(a, b)`, an argument bare when the console's tokenizer reads it whole (letters, digits, `_ . + -`), else in `"`. The tokenizer ends a quoted token at the next `"` or `'`, with no escapes, so an argument holds neither.
- **Where:** in `QUEN_Next`, on the game thread, inside modQuen's script call from `CR4Game.OnTick` (the script gets "" that tick). The engine itself runs `CollectSoundStates()` through the same executor from native code inside its tick.
- **Refused, nothing called:** quen's state (the last `QUEN_State`) shows no game or a fade or load (not flag 1, or flag 256); the exe's PE stamp isn't 5.01's (`0x6AC555A3`); the executor's 16 entry bytes aren't 5.01's (the reply gives the live ones). Each is checked before every call.
- **The reply:** `ok` true when the executor called the function. False when it called nothing: no exec function of that name (names are case-sensitive, and each name tried stays in the engine's name pool), more arguments than it takes, or a required one missing. An argument its type can't parse keeps its default: the function runs, and `ok` is true. `data` holds the line and the captured log. The engine's out string is leaked (at most about 2 KB a call): its free function isn't pinned.
- **W3Quen.log** gets one line per exec: `exec <line>: called`, `not called`, or `exec refused: <why>`.

**Why `Pause` and not `SetTimeScale(0)`** (both froze the world in the probe boot):
- `Pause` is keyed by reason, so `Unpause("quen")` lifts only quen's pause; menus and other reasons keep theirs.
- `IsPausedForReason("quen")` reads it back exactly.
- A time scale is one entry in a priority stack, and its `dontSave` flag means a scale can end up in a save.

## HUD line (from 0.1.16)

quen's lasting status line in a HUD movie of quen's own, styled like the HUD, where the game draws the HUD; the notification carries it elsewhere.

- **The movie** (`hud\quen\QuenHud.as`, a `HudModuleBase`, text only, no art, no `.fla`): one line of white text in the game's `$NormalFont` (each language's font, from the game's font map), size 22, centred, wrapping at 1000 px, on a black rounded box at 60 %, at the HUD's opacity and size options as vanilla's modules are. Its box's top is 96 px down and its centre 300 px right of the vanilla anchor `mcAnchorWatermark` (660, 0): the top centre at 1920 x 1080. Placed by eye; the probe boot tunes it.
- **Its slot:** the HUD's dormant `WatermarkModule` (in the HUD resource, never created by vanilla, no `OnTick`). On the first lasting notice of a game modQuen adds its table entry on the HUD root's `moduleManager` (`AddEntry("WatermarkModule", "hud_quen.swf", 10)`), then `DiscardHudModule` and `CreateHudModule`, as the indicator probe boot did for AreaInfo (2026-10-21); a load makes a new HUD, and the next lasting notice adds it again. The movie loads on a later tick, the notification carrying the line until then; once it has, `UpdateHUD()` once, since a module that loads late comes up in its Flash Hide state.
- **Where it shows** (`QUEN_HudDrawn`): the HUD's root shown (the user's HUD toggle and a quest's `ShowHud(false)` hide it), no photo mode, no menu, and the module out of its Flash Hide state (scenes, the radial menu and death hide it: `Hud.as`'s state table). Elsewhere, and for a notice under a minute (the movie has no timer), the notification. When the state poll's flags change (photo mode, a menu, a scene, a fade), the lasting line moves to the other one. The radial menu changes no flag: the HUD hides the line there.
- **Which lines:** with the config's `indicator` on, quen sends every state's line (working, measuring, measure_ab's "Next", yours, waiting) for 3,600,000 ms, so in gameplay each goes on the movie: the notification is one slot the game's and other mods' notices share and take over (finding 94; boot 214's "Next" labels), the movie a slot of quen's own. With `indicator` off quen's 8 s notices stay on the notification.
- **The holds:** the movie answers a re-send of the same text "notice already shown" (visible, alpha above 0, `tfText` the same), as the notification does, so quen's minimum times on screen (5 s before a call's first window, 3 s for a "Next" label) count from the movie's own read-back (status.rs `confirm`).
- **The hide:** `ShowElement(false, true)`, then with `bIgnoreState`: within the call (AreaInfo in the probe boot: one tick).
- **The build** (`hud.py`; `pack.py` runs it; needs `JAVA_HOME`, `FLEX_HOME` and `REDKIT`):
  - Apache Flex SDK 4.16.1's compc and mxmlc (on Temurin JDK 21), with Adobe's `playerglobal.swc` 32.0 (11.2's is no longer served). The game's movies are SWF 15 (Flash Player 11.2): mxmlc writes 15 (`-swf-version=15`), and QuenHud uses 11.2's API only.
  - The game's classes come from REDkit's `gui_new\actionscript`, read in place, and stay external: the movie holds `QuenHud` alone (mxmlc's link report). `hud\extern` declares the four Adobe easing classes `HudModuleBase` imports, which neither Flex nor REDkit has. Nothing of CDPR's is in the repo or the release.
  - mxmlc's build date (Metadata, ProductInfo) is dropped. REDkit's GFx exporter (`gfxexport_mult4fix.exe`, GFxExport 4.01 for Scaleform SDK 4.3.27) runs with wcc_lite's import's arguments. It compresses a whole 4 KB buffer, and past the movie's own length come leftovers of its memory that differ per run: `hud.py` cuts the movie to the length its header gives.
  - `cooked()` writes the cooked `CSwfResource` the game's bundles hold. It rebuilt the game's own cooked `hud_watermark.redswf` and `hud_areainfo.redswf` (`startup.bundle`) byte for byte from their linkage names and GFx bytes (2026-10-22). wcc_lite's import and cook aren't used: this REDkit (the 2026-10-07 build) saves an import into its own `bin` folder whatever `-depot` or `-workspaceDir` says (an absolute `-depot` stops at "Remap is not supported for SplitEditorDepot"), and its cook loads only files in its own depot ("Failed to load ... is this a resource").
  - wcc_lite `pack` (LZ4HC) and `metadatastore`, with `-wcclog` in `w3\out\hud`. Like every wcc_lite run, each rebuilds REDkit's `r4data\x64.release.redscripts` (the same bytes in two runs, 2026-10-22) and rewrites the four Documents settings files (section Offline script compile).
  - Reproducible: the same sources gave the same bundle and store in two builds (2026-10-22). The bundle holds the one movie (1,429 bytes; 1,662 packed) and no path.
- **Untested in the game (0.1.16):** whether the movie loads from modQuen's bundle and registers, `$NormalFont` in a text field made at run time (vanilla's `HudModuleEnemyFocus` makes its damage numbers the same way), its look and place, `UpdateHUD()` giving it OnDemand, the hide's latency, and the moves between it and the notification. The probe plan: `probes\indicator\README.md`, checks 9 to 14.

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
- `asi\build.bat` builds `out\W3Quen.asi`, compiling Tracy's client and Detours from `third_party\`; `asi\build.bat test` also runs the offline test; `python -I pack.py` does both, builds the HUD bundle (`hud.py`, which also needs `JAVA_HOME`, `FLEX_HOME` and `REDKIT`: section HUD line) and packs the zip.
- **The one write into REDkit:** each wcc_lite run of `hud.py` (pack, metadatastore) rebuilds REDkit's compiled-scripts cache, `r4data\x64.release.redscripts`, as `script_compile`'s runs do; the owner accepted it (2026-10-22). Its log goes to `out\hud`, not REDkit's `bin`.
- **Reproducible:** `/Brepro`, `/MT`, a zip with fixed dates and no compression. The same MSVC version gives the same bytes from any folder. 0.1.0, 0.1.1 and 0.1.2 were built with MSVC 14.50.35717 (Visual Studio 18.4): another version may give other bytes, so build releases with the version named here, or name the new one. 0.1.2's toolchain first rebuilt 0.1.1's ASI byte for byte, and 0.1.2's ASI built byte-identical in a second folder. 0.1.3 was built with MSVC 14.50.35726, which rebuilt 0.1.2's ASI byte for byte; 0.1.3's ASI built byte-identical in a second folder. 0.1.4 was built with MSVC 14.50.35717, which rebuilt 0.1.3's ASI byte for byte; 0.1.4's ASI built byte-identical in a second folder. 0.1.5 was built with MSVC 14.50.35717, which rebuilt 0.1.4's ASI byte for byte; 0.1.5's ASI built byte-identical in a second folder. 0.1.6 was built with MSVC 14.50.35717 (Visual Studio 18.4), which rebuilt 0.1.5's ASI byte for byte; 0.1.6's ASI built byte-identical in a second folder. 0.1.7 was built with MSVC 14.50.35717 (Visual Studio 18.4), which rebuilt 0.1.6's ASI byte for byte; 0.1.7's ASI built byte-identical in a second folder. 0.1.8 was built with MSVC 14.50.35717 (Visual Studio 18.4), which rebuilt 0.1.7's ASI byte for byte; 0.1.8's ASI built byte-identical in a second folder. 0.1.9 was built with MSVC 14.50.35717 (Visual Studio 18.4), which rebuilt 0.1.8's ASI byte for byte; 0.1.9's ASI built byte-identical in a second folder. 0.1.12 was built with MSVC 14.50.35717 (Visual Studio 18.4); `pack.py` rebuilt its ASI byte for byte in the same folder. 0.1.13 was built with MSVC 14.50.35717 (Visual Studio 18.4); `pack.py` rebuilt its ASI byte for byte in the same folder. 0.1.14 was built with MSVC 14.50.35717 (Visual Studio 18.4); `pack.py` rebuilt its ASI byte for byte in the same folder. 0.1.15 was built with MSVC 14.50.35717 (Visual Studio 18.4); `pack.py` rebuilt its ASI byte for byte in the same folder. 0.1.16 was built with MSVC 14.50.35717 (Visual Studio 18.4); `pack.py` rebuilt its ASI byte for byte in the same folder, and `hud.py` its bundle and store (section HUD line).
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
  - `QUEN_State` lines in `W3QuenState.log`, and the poll count before the first;
  - the scene verbs (from 0.1.13): each to its script line and its read-back to JSON, their refusals, read-backs that don't parse, and an `exec` refused before any state.
- `exec` (from 0.1.13), in `test.exe` and in `tracyhost.exe`: the executor's 16 entry bytes are written at its RVA, followed by a stand-in that undoes the entry and jumps to a C++ function recording the line. In `test.exe` (not 5.01's stamp) an exec is refused by the stamp and nothing is called. In `tracyhost.exe`: called with the line as `exec_line` builds it (quoted args too), the log in the reply, the out string handed in zeroed; not called (`ok` false); refused under a fade and with no game; refused with the live bytes when the entry's first byte is changed; the `W3Quen.log` lines.
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
- 2026-10-20 (0.1.10): with `world.actors`' radius of 0 accepted, `channel` failed.
- 2026-10-20 (0.1.11): with `world.actors`' flags capped at 127 again (no `entity_cut`), `channel` failed.
- 2026-10-20 (0.1.12): with a space accepted in `world.actors`' tag and class, `channel` failed.
- 2026-10-21 (0.1.13): with the exec verb's entry-byte check off, `exec` in `tracyhost.exe` called the changed entry and crashed (`build.bat test` failed).
- 2026-10-17, in witcher3-mods: without `FILE_FLAG_FIRST_PIPE_INSTANCE` the ASI joined the squatter's pipe; without the pid check the wrong client was served; with an Everyone DACL the ACL check failed; without the ttl check the expired command ran.
- 2026-10-17, here: without the token check in `QUEN_State`, `notoken` failed (the state log was written); without the poll count, `channel` failed; without the first poll's tick (0.1.1), `channel` failed.
- 2026-10-18 (0.1.2): with squad's blend weight `2u(1-u)` changed to `u(1-u)`, `channel` failed 82 checks against the reference vectors.
- 2026-10-19 (0.1.3): with the trigger's token check removed, `tracy-notoken` failed every dormancy check (the hook bytes, a `Tracy` thread, Winsock loaded, a new heap, a listening socket, a log written). With the trigger always on, `notoken` failed (its log written), and `tracy-none` and `tracy-token` failed every dormancy check (Tracy listening on 127.0.0.1:8086, its default port without `TRACY_PORT`).
- 2026-10-19 (0.1.9): each of these failed `channel`: the photo read-back's `grading_at_open` made from `grading`'s numbers; `photo.tick` taking a negative time; `photo.mode` taken without its `on`.

## Offline script compile

REDkit's `wcc_lite compilescripts <dir> -out=<dir> -agreetoterms` compiles a script tree without the game. Run it with `The Witcher 3 REDkit\bin\x64_RedKit` as the working folder, on a scratch copy of the game's `content\content0\scripts` plus the mods' scripts. Never point it at the game folder. quen's `script_compile` does all of this.

`-agreetoterms` accepts REDkit's licence (`wcc_eula.rtf`) for the run and skips its dialog. Without it, wcc_lite shows the dialog on every run: the 2026-10-07 build reads its saved answer (`HKCU\Software\CD Projekt RED\Mod Tools\1.0\DoNotShowEula`) with a 0-byte buffer, the read fails, and it writes 0 over the answer.

wcc_lite rewrites `editoruser.settings`, `profile.settings` and their `.bak` in `Documents\The Witcher 3` on every run (checked 2026-10-20 with `-agreetoterms`: the same bytes, new write times; nothing else there, no registry value under `HKCU\Software\CD Projekt RED`). By hand, copy those four files before the run and put them back after; `script_compile` does it, journaled.

Checked 2026-10-17 for 0.1.0 and 0.1.1, 2026-10-18 for 0.1.2 and 2026-10-19 for 0.1.3, whose `quen.ws` is 0.1.2's, 0.1.4, 0.1.5, 0.1.6, 0.1.7, 0.1.8, 0.1.9 and 0.1.10, and 2026-10-20 for 0.1.10's rebuild with `world.actors`, for 0.1.11 and for 0.1.12 (the same results, 0.1.12 without the all-mods run; its control was planted in `world.actors`' class branch), and 2026-10-21 for 0.1.13 (the same results, without the all-mods run; its control was planted in `QUENRPC_Ray`, reported at its line 793; run by hand with `-agreetoterms`; as shipped, seven imports can't bind, `QUEN_CamAt` the new one in 0.1.2) and for 0.1.14 (stubbed only, through quen's `script_compile` over the game's scripts and every installed mod: exit 0, 0 errors, 724 warnings; its control, planted in `QUENRPC_Frame`'s body box, reported at its line 668) and for 0.1.15 (the same: stubbed only, through `script_compile`: exit 0, 0 errors, 724 warnings; its control, planted in the `notice.hide` case, reported at its line 1359), and 2026-10-22 for 0.1.16 (stubbed only, by hand as `script_compile` does it, since the game was running: "Success! Scripts cooked", 0 errors, 724 warnings, none in `quen.ws`; its control, planted in `QUEN_Show`, reported at its line 419; only the prefixed imports stubbed: `modGraphicsControl` imports the engine's own `SetCommunityRadius`, which wcc_lite binds, and a stub of it is an error), with `quen.ws`, the Native Bridge's `Mods\modNativeBridge\content\scripts\imports.ws` and the W3 Game State Bridge's `w3gsb.ws`, about 40 s per run:
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
- **The scene verbs and exec, untested in the game (0.1.13):** every `scene.*` and `nav.line` call, `cam.noclip` and its restore, and the exec verb have run offline only. Unverified: `IsInView()` (`in_view`) against the projection, `NavigationClosestObstacleToLine` (`edge_m`; vanilla never calls it) and what `NavigationClearLineInDirection`'s answer means; what `GetWaterLevel` gives where there is no water; whether the executor's captured log holds a script's `LogChannel` lines; whether a function called from inside the script VM (exec in `QUEN_Next`) behaves as one typed into the console; the frame-time cost of each verb.
- **`scene.frame`'s body box, untested in the game (0.1.14):** its 0.3 m and 0.15 m were fitted to Geralt on boot 190's two captures (gameplay and photo mode); whether they fit other actors, crouching or mounted ones among them, is unknown.
- **Photo mode:** `settime` with photo mode open moved the sun at once (2026-10-19, hours 08 to 16). `weather` with photo mode open is untested.
- **Nearby actors (0.1.10, boot 175, 2026-10-20):** a query took 0.02 to 0.04 s; `velocity` was world-space for walking NPCs; `GetReadableName()` gave text ending like a depot path, cut by 0.1.10's 80 characters; `GetDisplayName()` gave "" for some guards. Untested (0.1.11): whether the readable name holds the whole template path. Untested (0.1.12): `tag` and `class` in the game, whether a class matches its subclasses, whether `GetGuidHash()` differs between two entities, and `WNB_StringToName` for a tag or class name.
- **Items, untested in the game (0.1.10):** whether `WNB_StringToName` makes the name the item tables know, what `AddAnItem` does with a count over one of a non-stackable item, and whether `IsItemEquipped` reads an equip back on the same tick.
- **The notice, partly tested in the game:** the indicator probe boot (2026-10-21, with a probe mod's calls, not 0.1.15's) showed a 1 h notification in gameplay, in photo mode and over the pause menu, and its hide; dialogues and cutscenes untested. Untested (0.1.15): the verbs themselves, the "already shown" read-back (does a re-send leave the line still, or flicker), and `sound`.
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
