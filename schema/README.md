# The quen ledger

A SQLite database of game boots and what was learned in them: captures (files on disk, by path and sha256),
measurements from quen and from other tools (PresentMon, Tracy, Nsight, RenderDoc, the RenoDX devkit, REA / Ghidra,
analysis scripts), and claims with the measurements that support or contradict them. Agents read it with
`ledger_query`: one `SELECT` or `WITH` statement, positional parameters `?1`, `?2`, … from `params`, on a read-only
connection with a time limit. They write through `ledger_ingest` (rows of the tables below, by table name),
`ledger_claim` (claims and their support) and `quen ledger add`; boots, games and mod sets are recorded by the session
tools. Game knowledge (entities, links, REA extractions, the KB index) is written by `ledger_import` and its CLI twins
(`quen re import`, `quen kb scan`, `quen kb claims`), and read in one call by `ledger_about`. Every query below runs as
is through `ledger_query`.

This text is the `quen_guide` tool's `ledger_schema` topic (`ledger::schema_description()`), compiled into the quen
you run, the same as `schema/README.md` in quen's source. The schema itself is the
numbered migrations beside it; `SELECT * FROM pragma_table_info('boots')` lists a table's or view's columns. Before a
newer quen migrates an existing ledger, it copies it to `backups/quen-v<old version>-<UTC time>.db` beside it.

## Conventions

- **Times** are UTC text `YYYY-MM-DDTHH:MM:SSZ`, so they sort and compare as text.
- **sha256** values are 64 lowercase hex digits. A build of a game or mod is its hash; version names are only labels.
- **JSON columns** hold JSON text: read them with `json_extract(col, '$.key')`, `json_each(col)` or `json_tree(col)`.
- **Flags** are 0 or 1. NULL means not recorded, unless a column says what NULL means.
- **Lists in views** are comma-separated text without spaces, `''` when empty. Test for one item with
  `',' || supported_by || ',' LIKE '%,tracy,%'`.
- **Nothing is deleted:** quen never deletes rows (a doc-reference scan only replaces the rows of the docs it read).
  Claims are withdrawn, not deleted. A file leaves the disk only through `capture_clean`, with the user's OK, to the
  Recycle Bin, or deleted on the user's Yes in Windows' own dialog; its row stays, with `removed_at` and `removed_to`.
- **Evidence tags** on measurements: `M` measured, `O` observed (in code, files, tool schemas), `I` inferred, `U`
  unverified, `static_re` a static fact from reverse engineering. Only `M`, `O` and `static_re` count towards proof.
- **Proven is derived, never stored:** a claim is proven when it is open, has support tagged M, O or static_re from at
  least two different `method_family` values, and no contradiction stands (`claims_status`).
- **Comparing numbers:** compare frame times inside one boot (`ab_runs`), and across boots only verdicts: raw frame
  times drift between boots (about 2 ms with path tracing on). Before putting two boots side by side, check
  `boot_comparison`: a different game build, the devkit, a profiler or a busy machine changes frame times more than most
  mods do. Tracy instrumentation (its ASI loaded) shows there as `mods`; check the mod set's entries.
- **Identifiers:** `kind` and `rel` are lowercase identifiers, like `method_family`, from the vocabularies under
  "Game knowledge: vocabularies" below, which grow only by adding to them.
- **Imported boots** (`harness_info` set) come from the history of an earlier tool: a NULL column on them
  means unknown, never off.

## Writing results from other tools

One measurement row per number, with the boot it was measured in (or none, for a static fact). `method_family` is one
lowercase identifier per independent method, not per tool, so two tools doing the same thing can't pass as two methods;
quen's agent playbook (the `quen_guide` tool's `playbook` topic, §2) keeps the list. Don't invent a family to make a claim proven:
`ledger_ingest` and `quen ledger add` refuse any other (rows an older quen took stay as written).

| Source | method_family | tag |
|---|---|---|
| PresentMon (quen's `measure_presentmon`, `measure_ab`) | `presentmon` | M |
| Tracy | `tracy` | M |
| Nsight Systems / Nsight Graphics (the boot's `profiler` says which) | `nsight` | M |
| the RenoDX devkit | `renodx_devkit` | M |
| quen's screenshots | `screenshot` | M |
| RenderDoc | `renderdoc` | M |
| REA / Ghidra, any disassembler, shader decompilers (DXC, the RenoDX devkit's) (no boot) | `static_re` | static_re |
| live memory reads (`mem_*`, `game_cvar` reads) | `mem` | M |
| game scripts, mod configs, logs, the game's data files (its bundles, REDkit's depot), read by an agent or quen's data tools | `files` | O |

A file a tool wrote (a CSV, a `.tracy` or Nsight report, an EXR dump) is a `captures` row, and the measurements taken
from it name it in `capture_id`. A knowledge-base page that backs a measurement or claim is a `re_refs` row.

## How the rows relate

```
games ──< boots >── saves
            │──< boot_mod_status
            │>── mod_sets ──< mod_set_entries ── mod_versions (by content_hash)
            ├──< captures >── poses
            │       └──< doc_refs
            └──< measurements ──< claim_support >── claims
                       └──< re_refs >────────────────┘
```

Game knowledge:

```
entities ──< entity_addresses >── games
   ├──< links >── claims | extractions | kb file@commit
   ├──< claim_subjects >── claims
   └──< kb_mentions >── kb_sections
extractions >── captures (the bundle file), >── extraction_profiles
measurements >── extractions
```

(`A ──< B`: many B rows point to one A; `A >── B`: many A rows point to one B.)

A boot belongs to one game (its exe's sha256) and loaded one mod set (after parking) and usually one save. Captures and
measurements point to their boot; a measurement can also point to the capture it was read from. Claims gather
measurements through `claim_support`.

## Tables

### games (table)

One row per game build. `id`; `name`; `exe_path` (where the exe was when first seen); `exe_sha256` (unique: the build's
identity); `build_stamp` (the exe's link time stamp, a label).

### saves (table)

A save file the game loaded or wrote. `id`; `game_id` → games; `file` (its name in the game's save folder); `sha256`;
`note`; `tainted_by_boot` → boots: the boot whose memory patch, cvar write or teleport came before the game wrote this
save, so its world state isn't the game's own; `made_by_boot` → boots: the boot whose `game_save` made it (NULL: quen
didn't make it, or it predates schema 0010). Unique per (game_id, file, sha256).

### mod_sets (table)

A set of mods exactly as a boot loaded them. `id`; `sha256`: the hash of the set's sorted entries, builds, configs and
parked flags, so two boots with the same `sha256` ran the same mods.

### mod_set_entries (table)

One mod in a set. Key (`mod_set_id` → mod_sets, `kind`, `name`): the mod's identity, e.g. kind `asi`, name
`W3ElementMap`. `kind` is `mods` (a Mods folder), `asi`, `addon`, `reshade` or `dlc` (rows imported from schema 1 may
keep an older free-text kind). `parked` 1: the mod was moved out for the boot, so it didn't load. `home`: where it
lives, relative to the game folder (rows written before 2026-10-18 hold the absolute path). `content_hash`: its build (a file's sha256, or a folder's tree hash). `file_count`, `bytes`.
`config_hash`: the hash of its config files; `config_values` (JSON object): the parsed config keys that change its
behaviour. `version_label` and `provenance` (`git_commit`, `nexus_file` or `unknown`): the label from mod_versions, if
the build has one.

### mod_versions (table)

Labels for builds. `content_hash` (key); `kind`, `name`: the mod; `version`; `provenance` (`git_commit` for our own
reproducible builds, `nexus_file` for downloads); `source`: the commit or the Nexus file name.

### boots (table)

One launch of the game. `id`; `game_id` → games; `t_start`, `t_end` (NULL while it runs); `save_id` → saves;
`mod_set_id` → mod_sets. The conditions:
- `mode`: `play` for a play session (session_launch mode play: the user played, and the agent drove only during
  hand-overs); NULL for a test launch or an imported boot (schema 0011).
- `settings_diff` (JSON object): the settings quen overrode for the launch, per section, e.g.
  `{"Rendering": {"SomeCvar": "0"}}`. Native launches also record here, until they get columns of their own: `build`
  (the exe's `sha256`, PE `stamp`, Steam's `steam_buildid` and the build gate's `status`: `known` with its `label`, or
  `allowed_unknown` with `why`), `profile` (a launch profile) and `conditions` (e.g. `devkit`, a profiler's tool,
  `unknown_build`).
- `devkit`: 1 when the RenoDX devkit was loaded; it roughly doubles frame time.
- `rebar`: 1 when Resizable BAR was on.
- `fg`, `vsync`: 0 when the launch forced them off, NULL when they were as the user set them.
- `profiler`: `renderdoc:NAME`, `nsys:NAME` or `ngfx:NAME` when the game ran under RenderDoc, Nsight Systems or Nsight
  Graphics, NULL without one.
- `display_state` (JSON object): the display's HDR mode, SDR white level, bit depth and the like.
- `busy_with` (JSON array): what the quiet-machine checks found running that skews the numbers, such as compilers or
  RE analysis; `[]` when every check found the machine quiet, NULL when none was recorded (every boot before schema 5,
  and every boot quen measured before its measure tools recorded their checks).
- `patches` (JSON array): every memory patch and data write of the boot, as its journal recorded them.

The end: `outcome` (`ok`, `failed`, or `recovered`: closed by a journal replay), `failure` (why, for failed),
`journal_path`. `reviewed_at`: when the user looked at the boot's outputs. `harness_info` (JSON object): the info
record of the earlier tool, for boots it ran or that were imported from its history; on such a boot, NULL means unknown.

### boot_mod_status (table)

What each mod did at load in a boot, from its log or state export. Key (`boot_id` → boots, `kind`, `name`); `status`:
`loaded`, `refused` (e.g. the game code differs; `reason` says why) or `not_present` (parked or missing); `reason`.

### poses (table)

A named camera position for repeatable captures. `id`; `name` (unique, e.g. `P1`); camera `cam_x`, `cam_y`, `cam_z`,
rotation `cam_pitch`, `cam_yaw`, `cam_roll` (degrees), `fov`; the player's `player_x`, `player_y`, `player_z` (all set
or all NULL) and `player_yaw` (degrees; never set without the position; since schema 0012 it may be NULL with one,
before it was set exactly when they were); `save_id` → saves: the save the pose is in; `ref_capture_id` → captures: its
reference image.

### captures (table)

A file on disk: a screenshot, a PresentMon CSV, a profiler report, a dump, a log. `id`; `boot_id` → boots (NULL for
files whose boot isn't known); `path` (relative to the capture root, `/` separators, unique ignoring case); `sha256`;
`bytes`; `kind`: the file's lowercase extension (`png`, `exr`, `csv`, `rdc`, `tracy`; `file` without one), the same for
every tool. Exceptions, from before this was the rule: the history import names five of the earlier tool's files by their role
(`boot_log`, `presentmon_csv`, `camera_ref`, `camera_cal`, `boots_jsonl`), and quen's own screenshot rows written before
`quen_guide` existed say `screenshot`. So tell what took a file by `method`, not `kind`. `method` (how it was taken,
e.g. `printwindow`, `wgc_fp16`, `presentmon`; a mem tool's name, e.g. `mem_walk`, for its result too big to answer
inline, kind `json`); `encoding` (a display image's pixels: `srgb8`, `pq_bt2020_u16`,
`pq_codes_f16`, `scrgb_f16`; any other buffer, e.g. scene-linear data or a G-buffer target, leaves it NULL and gives its
format in `frame_state`); `includes_overlay`, `includes_desktop`; `display_state` and `frame_state` (JSON objects: the
display and the frame when it was taken; quen's `display_state` names the monitor by `monitor_sha`, a short hash of its model name,
since 2026-10-18, by `monitor` before); `pose_id` → poses; `t` (when it was taken); `pinned` (1: never a cleanup candidate);
`removed_at` (when `capture_clean` took the file off the disk, NULL while it is there) and `removed_to` (`recycle_bin`,
`to_delete`: `<data root>\_to_delete\<date>\`, for the user to empty, until quen 0.1.5, `missing`: the file was gone
already, removed by hand, and `capture_clean` with `missing: true` marked the row, since 2026-10-18, or `deleted`: the
Recycle Bin couldn't take it whole and the user said Yes to Windows' own dialog, since schema 0013). The row and its
sha256 stay after a removal.

### measurements (table)

One number. `id`; `boot_id` → boots (NULL for a static fact); `capture_id` → captures (the file it was read from);
`metric` (e.g. `frame_ms_mean`, `gpu_busy_ms_p95`); `value`; `unit`; `state_label` (the A/B state it was measured in,
e.g. `A`); `method_family` (the tool, see above); `tag` (M, O, I, U, static_re); `run_id` (groups the rows of one run:
measure_ab's is `<session>-<tag>`, in a play session `<session>-h<n>-<tag>` with n the hand-overs (hands_take) before
it, and each of its windows is one row per metric); `world` (what the world did during the window, as quen saw it:
`user`, the user had the controls; `live`, the agent had them and the world ran; `paused`, quen's pause was on;
`photo`, photo mode; NULL: not recorded, every row before schema 0011); `note`. quen's presentmon rows' note
is `<csv>; <PresentMode>; <run>`: the CSV's path relative to the capture root (it has no captures row), and for
measure_ab each state's actions and `verdict {...}` (the run's verdict, diff_ms and drift_ms), then `a quiet machine` or
`warned: ...` (what the quiet-machine check said). Older rows hold the CSV's absolute path and the PresentMode only.
`extraction_id` → extractions: the REA record (or decompile) a static_re row was read from, NULL otherwise.

### claims (table)

A statement to prove or refute, e.g. "cvar X off saves 0.3 ms in scene P1". `id`; `text`; `scope` (where it holds,
e.g. a game version); `status` (`open` or `withdrawn`; proven is derived by claims_status); `withdrawn_reason`. `key`
(unique where set): a stable name tying the claim to its source; a claim imported from a KB table row has
`<note>#<anchor>/<first 12 hex of the sha256 of its text>`, so an edited row is a new claim under a new key.
`confidence` (`H`, `M`, `L`) and `review` (`V` verified by a second reading, `C` corrected, `U` unreviewed): the author's
grades as written, labels that never count towards proof.

### claim_support (table)

Links a measurement to a claim. Key (`claim_id` → claims, `measurement_id` → measurements); `supports` (1 supports, 0
contradicts).

### doc_refs (table)

References from documents (notes, scripts) to captures, from the last scan of each document. `id`; `doc_path`; `line`;
`raw` (the reference as written); `capture_id` → captures (NULL when it matches nothing); `dangling` (1: matches no
capture); `t_scanned`.

### re_refs (table)

Links a measurement or a claim to a page of the RE knowledge base. `id`; `measurement_id` → measurements or `claim_id`
→ claims (exactly one is set); `kb_path`.

### sweeps (table)

A camera sweep for cinematics: keyframed camera poses and the world state to hold. `id`; `name`
(unique); `sweep` (its canonical JSON: keyframes with pose, time and easing, plus mode and world state; at least 2
keyframes); `sha256` (of that JSON: the same path under two names shows); `save_id` → saves (the save its world
loads); `note`. A sweep row never changes, so two runs of it compare frame by frame.

### sweep_keyframes (table)

The pose each keyframe of a sweep was recorded as. Key (`sweep_id` → sweeps, `keyframe`, counting from 0);
`pose_id` → poses (the pose's reference capture is the screenshot it was checked on).

### sweep_runs (table)

One playback of a sweep. `id`; `sweep_id` → sweeps; `boot_id` → boots; `mode` (`realtime`, or `step`: the world
paused and every frame captured); `fps` (set only for `step`); `t`.

### sweep_frames (table)

The captured frames of a frame-step run: frame n shows the sweep at n / fps seconds. Key (`run_id` → sweep_runs,
`frame`); `capture_id` → captures (unique). Two runs of one sweep (e.g. a mod on, then off) compare frame by frame.

### entities (table)

Every named thing knowledge links to. `id` (a row number, never an identity outside the ledger); `game` (the title, as
`games.name`, not a build: a shader that survives a patch, or a named function, is the same entity in the next build);
`kind` and `key` (unique per game; the formats are under "Game knowledge: vocabularies"); `label` (a human name, which
may change, unlike the key); `t` (when first recorded).

### entity_addresses (table)

Where an entity sits in one build. Key (`entity_id` → entities, `game_id` → games). `rva` (image-relative) or `offset`
(a field's offset in its struct), exactly one; `size` (bytes); `bytes_len` and `bytes_sha256` (the first bytes at the
RVA, to re-find it after a patch); the basis, exactly one: `kb_source` (`<file>@<commit>`: a row of a KB table, replaced
by each KB scan), `extraction_id` → extractions or `claim_id` → claims; `confidence` (the KB table's O / I, or the
tool's word).

### extraction_profiles (table)

One analysis profile, stored once. `digest` (key: REA's `analysis_profile.digest`); `tool` (`rea`); `provider` (JSON
object: id, name, version); `parameters` (JSON object: everything the profile commits to, among them `analysis_seeds`,
the seed file's path and sha256), so a query can show what differs between two profiles.

### extractions (table)

One answered tool query: the cache of REA answers. `id`; `tool` (`rea`); `tool_version` (REA's version, given at
import since its records don't carry it; stored, not part of the key); `target_sha256` (the analysed file's sha256: a
new game build misses every entry); `profile_digest` → extraction_profiles; `operation` (e.g. `procedure_pseudo_code`,
`procedure_callers`, `xrefs`); `params` (canonical JSON object: keys sorted, no whitespace); `key_sha256`: the sha256
of the canonical JSON `{"operation", "parameters", "profile_digest", "target_sha256"}`, the same for the same query on
the same analysis; `evidence_id` (unique: REA's `ev_<64 hex>`, which hashes the record's content); `result_sha256` (of
the canonical raw result: one key with two result hashes gave two answers); `confidence` (`observed`, `derived` or
`inferred`, REA's word); `rva` (the record's first address location, else the `procedure` or `address` parameter, minus
the image base; REA 6.5.0 leaves locations empty for single-address queries, and rows quen 0.1.0 imported without an rva
get it at the next open); `entity_id` → entities (the function or label at that RVA in that build when the rva was
recorded); `capture_id` → captures (the bundle file holding the whole record, decompiled text included: a pinned capture
under the capture root); `t` (when imported). An exact hit is the same `key_sha256` with its file present
(`captures.removed_at` NULL); a near hit is the same target, operation and params under another profile.

### links (table)

A typed relation between two entities. `id`; `src_id`, `dst_id` → entities (never the same); `rel` (under "Game
knowledge: vocabularies"); the basis, exactly one: `claim_id` → claims (the link is exactly as proven as the claim),
`extraction_id` → extractions (a raw tool record) or `kb_source` (`<file>@<commit>`: a curated KB table row, replaced by
each KB scan); `confidence` (as the basis states it: REA's word, the table's column; NULL for a claim, whose standing is
derived); `t`. Unique per (src, rel, dst, basis). Only kb-backed links are ever deleted (by a KB scan).

### claim_subjects (table)

What a claim is about. Key (`claim_id` → claims, `entity_id` → entities).

### kb_sections (table)

The KB's prose index: one row per heading section of a note; the text stays in git. `id`; `path` (relative to the KB
root, `/` separators); `anchor`: the slug of the heading path (`## Q1` > `### Allocation` → `q1/allocation`; `''` for
the text before the first H2; a repeated one in a note gets `-2`, `-3`), unique per (path, anchor); `heading`; `level`
(1 for the text before the first H2, else 2 or 3); `parent_id` → kb_sections (an H3's H2); `line_start`, `line_end`
(1-based, inclusive); `sha256` (of the section's lines); `kb_commit` (the KB's git commit at the last scan: the text is
`git show <kb_commit>:<path>`, lines line_start to line_end); `scanned_at`; `gone_at` (set when a scan no longer finds
the anchor; the row stays, so a citation still resolves through git history).

### kb_mentions (table)

The known entities a section names, from the last scan of its note. Key (`section_id` → kb_sections, `entity_id` →
entities); `n` (times mentioned). Matched: keys as written, shader hashes with or without `0x`, the RVAs and VAs of
functions and labels (Ghidra's `FUN_…` names too), hex in any case. Stages and labels are not matched, an alias two
entities share matches neither, and unknown hex never becomes an entity.

## Game knowledge: vocabularies

**Entity kinds** and their keys:

| kind | key | e.g. | written by |
|---|---|---|---|
| `function` | its KB name; unnamed: `fn_<stamp>_<rva>` | `Post_Tonemap`, `fn_6AC555A3_34D4D0` | KB scan (names.tsv) |
| `label` | its KB name (a name inside code or data) | `CRenderSkinningData_vftable_slot10` | KB scan (names.tsv) |
| `hook` | `<mod>:<hook kind>:<function key>` at a function's start, else `<mod>:<hook kind>:<stamp>_<rva>` | `W3ElementMap:hook:ElementMap_AllocIndex` | KB scan (hooks.tsv) |
| `patch` | its name in patches.tsv | `cull_disable` | KB scan (patches.tsv) |
| `field` | `<struct>+0x<offset>`, or `<struct>.<name>` once named | `cvar_object.value` (cvar_layout.tsv's columns) | KB scan (cvar_layout.tsv) |
| `shader` | the RenoDX devkit's hash: `0x` + 8 uppercase hex | `0x382CDBDB` | KB claim import |
| `surface` | a render target, texture or buffer as a frame uses it | `RTN_PostProcessTempHalf3` | not yet |
| `stage` | a slug | `tonemap` | not yet |
| `cbreg` | `<shader>/<cbuffer>/<register>` | `0x382CDBDB/cb3/c16.x` | not yet |
| `cvar` | `Group/Name` as the game spells it | `PostProcess/ToneMappingFixedLuminance` | not yet |
| `setting` | the settings file's `Section/Key` | `Rendering/RT/PathTracer/PTEnable` | not yet |

Draw indices, per-run resource handles and row ids are never keys.

**Link relations** (`rel`, from → to): `runs` (stage → shader), `draws` (function → shader), `reads` / `writes`
(shader or stage → surface), `sets` (function → cbreg or field), `reads_cvar` (function → cvar), `calls` (function →
function: the REA import writes it from callers and callees records whose both ends are known functions of that
build), `in` (label, hook site or patch → the function holding it), `hooks` (hook → function: the KB scan writes it for
a hook at a function's start), `patches` (patch → function), `controls` (setting → cvar), `same_as` (any → the same
kind), `replaced_by` (shader → shader, across builds).

**Claim grades:** `confidence` H, M, L; `review` V, C, U. A KB table row's grades come from its Status or Conf column,
else from its own marks (`[V]`, `[C]`, `[U]`, `(H)`), else from its headings'. Mixed review grades are C when any part
was corrected, else U when any part is unreviewed; mixed confidences are the lowest.

## Views

### boot_conditions (view)

One row per boot with the conditions its numbers depend on. `boot_id`; `t_start`, `t_end`, `outcome`, `mode`; the game:
`game_id`, `game` (its name), `exe_sha256`, `build_stamp`; the mods: `mod_set_id`, `mod_set_sha256`; `save_id`;
`settings_diff`; `patches`; `devkit`; `profiler`; `busy` (1: a quiet-machine check found something running that skews
the numbers, 0: every check found the machine quiet, NULL: none recorded); `busy_with`; `rebar`, `fg`, `vsync`;
`display_state`. The columns are the boots and games columns of the same names.

### measurements_with_boot (view)

Each measurement beside its boot's conditions. The measurement's own columns: `id`, `boot_id`, `capture_id`, `metric`,
`value`, `unit`, `state_label`, `method_family`, `tag`, `strong` (1: its tag counts towards proof), `run_id`, `world`,
`note`; `capture_path` (its capture's path). Then its boot's, as in boot_conditions (NULL without a boot): `boot_t_start`,
`boot_outcome`, `boot_mode`, `game`, `exe_sha256`, `build_stamp`, `mod_set_id`, `mod_set_sha256`, `save_id`, `settings_diff`,
`patches`, `devkit`, `profiler`, `busy`, `busy_with`, `rebar`, `fg`, `vsync`, `boot_display_state`.

### captures_with_boot (view)

Each capture beside its pose and its boot's conditions. The capture's columns: `id`, `boot_id`, `path`, `sha256`,
`bytes`, `kind`, `method`, `encoding`, `includes_overlay`, `includes_desktop`, `display_state`, `frame_state`,
`pose_id`, `pose` (the pose's name), `t`, `pinned`, `removed_at`, `removed_to`. Then its boot's, as in
boot_conditions: `boot_t_start`,
`boot_outcome`, `boot_mode`, `game`, `exe_sha256`, `build_stamp`, `mod_set_id`, `mod_set_sha256`, `save_id`, `settings_diff`,
`patches`, `devkit`, `profiler`, `busy`, `busy_with`, `rebar`, `fg`, `vsync`, `boot_display_state` (the boot's
display_state; `display_state` is the capture's own).

### claims_status (view)

Whether each claim is proven. `id`, `text`, `scope`, `status`, `withdrawn_reason` as in claims; `families`: how many
method families support it with tag M, O or static_re; `contradictions`: how many contradicting rows stand; `proven`.
A contradiction (any tag) stands unless two families have strong support from boots that started after its boot; one
from no boot always stands. So after the newest contradiction, two independent methods must agree again.

### claims_evidence (view)

claims_status beside the methods behind each claim: `id`, `text`, `scope`, `status`, `withdrawn_reason`, `proven`,
`families`, `contradictions` as in claims_status; `supported_by`: the families whose support counts (the ones
`families` counts); `weak_support`: families whose only support is tagged I or U, which never counts;
`contradicted_by`: every family with a contradicting row, standing or outranked; `measurements`: how many rows are
linked; `boots`: how many boots they came from; `kb_paths`: the RE knowledge-base pages linked to the claim.

### links_evidence (view)

Each link with its basis's provenance and standing, one row per link. `id`, `rel`; `src_id`, `src_kind`, `src_key`,
`dst_id`, `dst_kind`, `dst_key`; `basis` (`claim`, `extraction` or `kb`); for a claim: `claim_id`, `claim_key`,
`claim_status`, `proven`, `families`, `contradictions`, `supported_by` (as in claims_evidence); for an extraction:
`extraction_id`, `tool`, `tool_version`, `evidence_id`, `target_sha256`; `kb_source`; `confidence`.

### boot_comparison (view)

Every ordered pair of distinct boots (`a_id`, `b_id`), and whether their numbers can be compared directly. Query it
for the pair: `WHERE a_id = ?1 AND b_id = ?2`.
- `differs`: conditions both boots recorded, with different values: `mode` (a play boot and a test boot; NULL is a
  value), `game_build`, `devkit`, `profiler` (the tool
  before `:`, so `nsys:run_1` and `nsys:run_2` are one profiler; none attached is a value), `rebar`, `fg`, `vsync`,
  `display_state`.
- `unknown`: conditions a side didn't record: `devkit`, `busy` (every boot before schema 5 has busy unknown).
- `busy`: the sides whose machine was busy, `a` and / or `b`.
- `failed`: the sides whose boot failed or was recovered, so may have stopped part-way (a running boot isn't).
- `varies`: what a test changes on purpose: `mods` (another mod set), `mod_status` (mods loaded or refused
  differently), `settings`, `save`, `patches`. It never makes a pair incomparable, but numbers can't be credited to
  one change while several vary.
- `comparable`: 1 only when `differs`, `unknown`, `busy` and `failed` are all `''`. Never mix boots that aren't
  comparable without saying which conditions differ or are unknown. Even comparable boots drift: compare their
  verdicts, not their raw frame times.

### ab_runs (view)

One row per run and metric of an A/B run (measure_ab), from any tool's rows that carry a `run_id` and
state labels: `boot_id`, `run_id`, `metric`, `method_family`, `unit`; `states`: how many state labels the run has; per
state, the first one recorded and the second: `first_label`, `first_windows` (rows), `first_mean`, `first_drift` (the
spread of its windows, max - min), `second_label`, `second_windows`, `second_mean`, `second_drift`; `diff`
(second_mean - first_mean); `verdict`: `outside drift` only if |diff| exceeds both drifts, `inside drift` otherwise,
NULL unless the run has exactly two states with two windows or more each; and the boot's `boot_t_start`, `devkit`,
`profiler`, `busy`. The view doesn't see PresentMon's present mode, which measure_ab also checks: its own result
names a run whose windows mixed modes.

## Example queries

**Did a cvar change frame time beyond drift, within one boot?** measure_ab's verdict per run. `?1`: text the run id or
a state label contains, such as the cvar's name (measure_ab's run id ends with its `tag`).

```sql
SELECT boot_id, run_id, first_label, first_mean, first_drift, second_label, second_mean, second_drift, diff, verdict,
  devkit, profiler, busy
FROM ab_runs
WHERE metric = 'frame_ms_mean'
  AND (instr(lower(run_id), lower(?1)) OR instr(lower(first_label), lower(?1)) OR instr(lower(second_label), lower(?1)))
ORDER BY boot_t_start, run_id;
```

**Does that verdict repeat on other boots, and can they be compared with the first?** Every A/B run as above, with
how its boot compares with the boot of the earliest one (verdicts, not raw frame times, carry across boots). `?1` as
above.

```sql
WITH runs AS (
  SELECT * FROM ab_runs
  WHERE metric = 'frame_ms_mean'
    AND (instr(lower(run_id), lower(?1)) OR instr(lower(first_label), lower(?1))
         OR instr(lower(second_label), lower(?1)))
),
earliest AS (SELECT boot_id FROM runs ORDER BY boot_t_start, run_id LIMIT 1)
SELECT r.boot_id, r.run_id, r.diff, r.verdict, coalesce(c.comparable, r.boot_id = e.boot_id) AS comparable,
  c.differs, c.unknown, c.busy, c.failed, c.varies
FROM runs r
CROSS JOIN earliest e
LEFT JOIN boot_comparison c ON c.a_id = e.boot_id AND c.b_id = r.boot_id
ORDER BY r.boot_t_start, r.run_id;
```

**Which claims are proven, and by which tools?**

```sql
SELECT id, text, scope, supported_by, weak_support, contradicted_by, kb_paths
FROM claims_evidence
WHERE proven
ORDER BY id;
```

**The evidence for one claim, with each row's boot conditions.** `?1`: the claim's id.

```sql
SELECT s.supports, m.id, m.method_family, m.tag, m.strong, m.metric, m.value, m.unit, m.state_label, m.boot_id,
  m.boot_t_start, m.devkit, m.profiler, m.busy
FROM claim_support s
JOIN measurements_with_boot m ON m.id = s.measurement_id
WHERE s.claim_id = ?1
ORDER BY m.boot_t_start, m.id;
```

**Captures of a pose across a mod's builds.** `?1`: the pose's name; `?2`, `?3`: the mod's kind and name (e.g. `asi`,
`W3ElementMap`). A NULL build: the boot's mod set doesn't have the mod (or isn't recorded); parked 1: it didn't load.

```sql
SELECT c.id, c.path, c.t, c.boot_id, e.content_hash AS build,
  CASE WHEN e.content_hash IS NOT NULL THEN coalesce(e.version_label, 'unknown build') END AS version,
  e.config_hash, e.parked, c.encoding, c.devkit, c.exe_sha256
FROM captures_with_boot c
LEFT JOIN mod_set_entries e ON e.mod_set_id = c.mod_set_id AND e.kind = ?2 AND e.name = ?3
WHERE c.pose = ?1
ORDER BY e.content_hash, c.t;
```

**Can two boots' numbers be compared?** `?1`, `?2`: the boots' ids.

```sql
SELECT comparable, differs, unknown, busy, failed, varies
FROM boot_comparison
WHERE a_id = ?1 AND b_id = ?2;
```

**Baselines for a boot: the boots its numbers can be compared with, newest first.** `?1`: the boot's id.

```sql
SELECT c.b_id, b.t_start, c.varies
FROM boot_comparison c
JOIN boot_conditions b ON b.boot_id = c.b_id
WHERE c.a_id = ?1 AND c.comparable
ORDER BY b.t_start DESC;
```

**Which mods differ between two boots, entry by entry?** `?1`, `?2`: the boots' ids. A NULL build: the mod isn't in
that boot's set.

```sql
WITH a AS (SELECT e.* FROM boots b JOIN mod_set_entries e ON e.mod_set_id = b.mod_set_id WHERE b.id = ?1),
  z AS (SELECT e.* FROM boots b JOIN mod_set_entries e ON e.mod_set_id = b.mod_set_id WHERE b.id = ?2)
SELECT coalesce(a.kind, z.kind) AS kind, coalesce(a.name, z.name) AS name, a.content_hash AS a_build,
  z.content_hash AS b_build, a.config_hash AS a_config, z.config_hash AS b_config, a.parked AS a_parked,
  z.parked AS b_parked
FROM a
FULL OUTER JOIN z ON z.kind = a.kind AND z.name = a.name
WHERE a.name IS NULL OR z.name IS NULL OR a.content_hash IS NOT z.content_hash OR a.config_hash IS NOT z.config_hash
  OR a.parked <> z.parked
ORDER BY 1, 2;
```

**What was measured in a boot, by which tools?** `?1`: the boot's id.

```sql
SELECT method_family, metric, state_label, count(*) AS rows, avg(value) AS mean, unit
FROM measurements
WHERE boot_id = ?1
GROUP BY method_family, metric, state_label, unit
ORDER BY method_family, metric, state_label;
```

**Has this been extracted before?** Every REA answer for one address of one build. `?1`: the exe's sha256; `?2`: the
RVA (`0x34C880`) or a function's or label's key; `?3`: an operation, or `''` for any. `seeds` is the seed file the
profile used: the same path and sha256 as the next session's is an exact hit. `results` over 1: the same query once gave
different answers. `removed_at` set: the bundle file is gone, so the answer must be asked again. `ledger_about` answers
the same in one call.

```sql
SELECT x.operation, x.params, x.evidence_id, x.confidence, x.tool_version, x.profile_digest,
  json_extract(p.parameters, '$.analysis_seeds') AS seeds, c.path AS bundle, c.removed_at,
  (SELECT count(DISTINCT y.result_sha256) FROM extractions y WHERE y.key_sha256 = x.key_sha256) AS results
FROM extractions x
JOIN extraction_profiles p ON p.digest = x.profile_digest
JOIN captures c ON c.id = x.capture_id
WHERE x.target_sha256 = ?1
  AND (printf('0x%X', x.rva) = '0x' || upper(substr(?2, 3))
       OR x.entity_id IN (SELECT id FROM entities WHERE kind IN ('function', 'label') AND key = ?2))
  AND (?3 = '' OR x.operation = ?3)
ORDER BY x.operation, x.t DESC, x.id;
```

**From one entity to the hooks and patches that touch it.** An undirected walk over the links, five hops at most; an
unproven claim-backed hop shows `?`, so the weakest hop is visible. `?1`: the game; `?2`, `?3`: the start's kind and
key.

```sql
WITH RECURSIVE walk(id, depth, path) AS (
  SELECT id, 0, kind || ':' || key FROM entities WHERE game = ?1 AND kind = ?2 AND key = ?3
  UNION ALL
  SELECT n.id, w.depth + 1,
    w.path || ' ' || l.rel || CASE WHEN l.basis = 'claim' AND NOT l.proven THEN '?' ELSE '' END || ' ' || n.kind || ':'
      || n.key
  FROM walk w
  JOIN links_evidence l ON w.id IN (l.src_id, l.dst_id)
  JOIN entities n ON n.id = CASE WHEN l.src_id = w.id THEN l.dst_id ELSE l.src_id END
  WHERE w.depth < 5
    AND coalesce(l.claim_status, 'open') = 'open'
    AND instr(w.path, n.kind || ':' || n.key) = 0
)
SELECT w.depth, w.path
FROM walk w JOIN entities e ON e.id = w.id
WHERE e.kind IN ('hook', 'patch')
ORDER BY w.depth, w.path;
```
