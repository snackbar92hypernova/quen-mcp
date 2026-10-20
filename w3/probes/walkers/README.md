# modQuenProbeWalkers: PROBE BUILD, not for play

A throwaway script mod. It answers one question in one boot: **do spawned commoners, not a community's, walk where a script tells them, the same way twice?** quen builds its walker-scenario verbs (spawn, walk, despawn by tag) only if they do. Community NPCs refused `ActionMoveToAsync` in boot 178 (0 of 8, 0 of 2); spawned ones are untested.

- One file: `modQuenProbeWalkers\content\scripts\local\quenProbeWalkers.ws`. It goes in with quen's `stage_mods`; no resources.
- No wrapped vanilla method. It adds a class, one field and one timer function to `CR4Player` (`@addField`, `@addMethod`, the way installed mods add their timers).
- Nothing runs until a console line is typed.

## What it spawns, and how it orders the walk

- **Template:** `characters\npc_entities\crowd_npc\novigrad_citizen\novigrad_citizen.w2ent`, in `bob.bundle` and `ep1.bundle` (quen `data_read`, 2026-10-20).
  - It is a `CNewNPC` on `gameplay\trees\npc_base.w2behtree`, group `ENGT_Commoner` (`CAINPCGroupTypeRedefinition`), with the commoner reaction trees.
  - Those are the same AI trees as `crowd_npc\novigrad_citizen\community\novigrad_citizen_community.w2ent`, the copy that communities spawn. So the walkers are townsfolk in all but their spawner.
  - Created as vanilla's animation test spawner creates test NPCs (`game\animations\animationTestSpawner.ws:173`): `CreateEntity(template, pos, rot, true, false, false, PM_DontPersist, tags)`, with the tag `quen_walker` set at creation.
- **The lane:** walker *i* starts `dist` m ahead of Geralt, at a lateral offset `side + (i - (n-1)/2) × 1.2` m to his right, and walks to the mirror point `dist` m behind him.
  - The walkers come towards the camera.
  - Both Z values come from `NavigationComputeZ` (±5 m), as the scene player corrects a walk target (`game\scenes\scenePlayer.ws:424`). A point off the navmesh skips that walker, and the log says so.
  - `lane_clear` is `NavigationLineTest(start, target, 0.4)`: whether a straight 0.4 m lane exists on the navmesh.
  - The lane depends only on Geralt's position and heading: a `game_teleport` to the same pose gives the same lane.
- **Method 0 (default):** a `CAIMoveToPoint` tree (`resdef:ai\scripted_actions/move_to_point`), walk speed, destination and heading set, then `ForceAIBehavior(tree, BTAP_Emergency)`.
  - That is how vanilla walks non-player actors: the scene player (`scenePlayer.ws:417-457`) and the `MovePlayerFwd` exec (`game\temp.ws`) build the tree the same way.
- **Method 1:** `ActionMoveToAsync(target, MT_Walk, 1.0, 1.0)`, the call community NPCs refused.
  - The latent `ActionMoveTo` is left out: an exec can't wait on a latent call, and it would need an entity state of its own.
- **The walk's clock:** a timer on the player, every 0.2 s. Per walker it records:
  - path length and the largest distance from the start-target line, both horizontal;
  - arrival: within 1.0 m of the target, horizontally (`CAIMoveToPoint`'s default `maxDistance`);
  - failure: the timeout, or the walker gone.

## Console commands

Exec functions need the console on (quen's launch turns it on). Every line goes to the log channel `QuenWalk`: read it with `game_scriptlog` and `channel: "QuenWalk"` (needs `debugscripts`).

| Command | What it does | Log lines |
|---|---|---|
| `qwspawn(n, dist, side)` | Clears first (as `qwclear`), then spawns `n` walkers (default 3, at most 12) in the lane, `dist` m each way (default 10), its middle `side` m to Geralt's right (default 0: through his spot) | `spawn i=.. lane=.. at=x,y,z to=x,y,z lane_clear=true/false` per walker; `spawn lane=.. skipped/failed: ..`; then `spawned K of n player=x,y,z heading=h template=..` |
| `qwgo(method, timeout, delay)` | Orders every walker that isn't walking or waiting. The walks start `delay` s later (default 0), all on one tick, each from where its walker stands then. A walk fails `timeout` s after its start (default 60). A failed or arrived walker can be ordered again, by either method | `ordered K method=.. timeout=.. delay=..`; at the start `order i=.. method=.. id=.. from=x,y,z dist=..`. `id` is ForceAIBehavior's action id, or 1/0 for ActionMoveToAsync's true/false |
| (the tick) | | `arrived i=.. t=<s since start> dist=.. path=.. straight=.. dev=..`, `failed i=.. why=timeout\|gone ..`; when no walker walks or waits, `qwstatus`'s lines |
| `qwstatus()` | Changes nothing | `status i=.. state=spawned\|waiting\|walking\|arrived\|failed t=.. dist=<to target now> path=.. straight=.. dev=.. at=x,y,z` per walker (a waiting walker's `t` is negative: the seconds until its start), then `status tracked=.. spawned=.. walking=.. arrived=.. failed=.. waiting=.. tagged=<entities in the world with the tag>` |
| `qwclear()` | Destroys every entity tagged `quen_walker` (`GetEntitiesByTag`, `Destroy`), stops the tick, forgets the list. Idempotent | `clear destroyed=N` |

After a load the list is empty (it lives on the player), so `qwstatus`'s `tagged` is the persistence count.

## Offline compile (2026-10-20)

`script_compile` refused: quen's config has no `redkit`. So the compile ran by hand, as `w3/README.md` describes:
- wcc_lite `compilescripts <tree> -out=<dir> -agreetoterms`, run from REDkit's `bin\x64_RedKit`;
- the tree a scratch copy of `content\content0\scripts`, with every installed `Mods\*\content\scripts` over it (quen's package included) and then this mod;
- the mods' `import function`s replaced by plain stubs;
- the four `Documents\The Witcher 3` settings files backed up before each run and put back after, bytes and write times checked.

| Run | Result |
|---|---|
| clean | `Success! Scripts cooked`, exit 0, 362 warnings (the package's own count), none in this file |
| control: an unknown call planted in `qwclear`'s body | exit 1: `local\quenProbeWalkers.ws[274]: Could not find function 'QuenNoSuchCall'`, the planted line |

The file compiled: `quenProbeWalkers.ws` sha256 `5a2fdc723ecf049d0a2f2626b1f48e5ef7202f2ab997b1c12c034e37a82930a8`.

What the compiler taught on the way: `state` is a keyword; an exec function can't call another exec; a field can't be assigned through `thePlayer` from outside the class.

## Boot plan (about 10 minutes of game time)

The owner and the main session schedule the boot. Nothing here launches the game.

**Launch:**
- Needs quen-w3 0.1.12, for `world_actors`' and the trace's `tag`. Install no NPC mod and stage none: the Crowds brain probe swaps `npc_base.w2behtree`, which these walkers run on.
- `session_plan`, then `session_launch` with:
  - `save`: a save in Novigrad (the Crowds probes' Hierarch Square daytime save will do);
  - `stage_mods: ["<this checkout>\\w3\\probes\\walkers\\modQuenProbeWalkers"]`;
  - `debugscripts: true`, so `LogChannel` reaches `scriptslog.txt`;
  - `photo: false`, because photo mode pauses the world;
  - `devkit: false`;
  - the owner's usual `park` for clean tests;
  - `user_ok: true`, after the owner's yes.

**Where to stand:** Hierarch Square, Novigrad, at Geralt's pose from the ledger's poses (`hierarch-geralt-front`):
- `game_teleport {pos: [668.99, 1875.87, 9.94], rot: [0, 52.3, 0]}`.
- If any `spawn` line says `skipped` or `lane_clear=false`, run `qwclear()`, teleport again with yaw 142.3 (then 232.3, then 322.3), and use the first heading whose lanes are all clear for every run.

**Before the first spawn:** `game_listsaves`. Note the newest AutoSave, for the persistence check.

**Each run.** One run is the same six calls; `<lane>` and `<method>` are in the table below.
1. `game_teleport`, the pose above.
2. `game_console "qwspawn(3, 12, <lane>)"`.
3. `world_actors {tag: "quen_walker", radius: 30}`: 3 found, each with an `id`; note their positions.
4. `game_console "qwgo(<method>, 60, 15)"`: the walks start 15 s later, so the trace (the next call) covers them from standing.
5. `measure_trace {tag: "<run>", secs: 50, trace: {tag: "quen_walker", radius: 30, max: 12}}`. quen runs one game call at a time, so the trace starts only after the console line.
6. `game_scriptlog {channel: "QuenWalk", lines: 60}`: the `order`, `arrived` / `failed` and `status` lines.

| Run | `<lane>` | `<method>` | Asks |
|---|---|---|---|
| `walk_a1` | 3 (lanes at 1.8, 3.0 and 4.2 m right of Geralt) | 0 | do they arrive, straight? |
| `walk_a2` | 3 | 0 | the same paths again? |
| `walk_b` | 0 (the middle lane through Geralt's spot) | 0 | the Crowds case: walkers meeting the player |
| `walk_c` | 3 | 1 | does ActionMoveToAsync move spawned NPCs? |

If method 0 fails in `walk_a1`, skip `walk_a2` and `walk_b`. Instead order the same walkers by method 1 where they stand: `qwgo(1, 60, 15)`, then the trace and the log as in steps 5 and 6.

**Teardown:**
1. `game_console "qwclear()"` twice: `clear destroyed=3`, then `clear destroyed=0`.
2. `game_console "qwstatus()"`, then `game_scriptlog`: `tagged=0`.
3. `world_actors {tag: "quen_walker", radius: 100}`: `found: 0`.

**The persistence check:**
1. `game_listsaves`.
2. `game_load` the newest AutoSave.
3. `game_console "qwstatus()"`, then `game_scriptlog`: `tracked=0 ... tagged=0`.
4. `world_actors {tag: "quen_walker", radius: 100}`: `found: 0`.

The check is decisive for `PM_DontPersist` only if that AutoSave is new since the first spawn, that is, written while walkers lived. If it is older, the check only shows that a load brings back none. Then report `PM_DontPersist` as untested. The boot plan writes no save. Testing it fully needs one `game_save` while walkers stand, and that is the owner's call.

Then `session_end`.

## What counts as "walkers obey"

Each criterion needs both methods to agree: the probe's own 0.2 s samples (its log) and quen's 10 Hz trace (the CSV). Match a walker to its trace track by `id`: its first trace rows sit at its `spawn ... at=` point.

1. **Arrive:** every walker of `walk_a1` logs `arrived` with `t` ≤ 40 s (24 m at walking pace takes about 15-20 s). Its last trace sample is within 1.0 m of its `to=` point.
2. **Straight-ish:** `path` ≤ 1.2 × `straight`, and `dev` ≤ 1.0 m, in the log; the trace's largest lateral offset from the lane is ≤ 1.0 m.
3. **Twice the same:** for `walk_a1` and `walk_a2`, lane by lane:
   - `t` within 2 s, `path` within 1.0 m and `dev` within 0.5 m (log);
   - the lateral offset at each metre of progress along the lane within 0.5 m (trace CSVs).

**Obey** means all three for one method. The town's own crowd isn't controlled, so a single bump can break 3. Then run `walk_a2` once more at night (`game_settime`, hour 2: fewer townsfolk) before calling it.

## What each outcome means

| Seen | Meaning, next step |
|---|---|
| Method 0 meets 1-3 | **Obey.** Build `spawn` / `walk` / `despawn` on `CAIMoveToPoint` + `ForceAIBehavior` |
| Method 0 refuses (`id` < 0, or no motion), method 1 meets 1-3 | Obey by ActionMoveToAsync: build the verbs on it |
| They arrive but miss 2 or 3 | They walk, but not reproducibly: shorter legs (waypoints), or night runs. The owner decides before any verb |
| Neither method moves them | Spawned commoners refuse too: no scenario verbs as designed. Next: another template, or a forced tree from a resource |
| `walk_b` walkers stop or detour at Geralt | Expected vanilla avoidance: the Crowds mod's subject, not a probe failure |
| `tagged` > 0 after `qwclear` | Destroy by tag fails: blocking, cleanup needs another way |
| `tagged` > 0 after the AutoSave load | `PM_DontPersist` lets walkers into saves: a scenario run must forbid saves |

## Cleanup

`session_end` recycles the staged copy. A crash takes the walkers with the game. This folder is source: nothing it produces is written here.
