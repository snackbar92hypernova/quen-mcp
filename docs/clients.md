# quen with MCP clients other than Claude Code

quen is a standard MCP server over stdio (`quen mcp`), so any local MCP client can start it. This page says what each one does with quen's long calls and tool schemas, and what quen does about it. Read from primary sources (official docs and source at a pinned commit) on **2026-10-18**; recheck before relying on a number. Setup snippets: [INSTALL.md](../INSTALL.md) §4.

## Summary

| Client | Starts quen on Windows | Tool-call timeout (default → how to change) | Progress | quen's job budget |
|---|---|---|---|---|
| Claude Code | yes | ~28 h (`MCP_TOOL_TIMEOUT`, per server `"timeout"` in ms) | shown; resets only the 30 min idle timer | none: waits for every call |
| Codex CLI | only with `quen serve --stay` running first ([below](#clients-that-hold-servers-in-a-job-object)) | source 300 s, docs 60 s → `tool_timeout_sec` | sends no token | 20 s |
| OpenAI Agents SDK, Python | only with `quen serve --stay` running first (same job) | **5 s** → `client_session_timeout_seconds` | sends no token | 20 s |
| OpenAI Agents SDK, JS | yes (Node) | 60 s → `timeout` (ms) | sends no token | 20 s |
| VS Code / Copilot agent mode | yes | none | shown | 20 s |
| Cursor | yes | IDE ~60 min, CLI/ACP fixed 60 s; not configurable | doesn't extend the timeout | 20 s |
| Gemini CLI | yes (trusted folders only) | 600 s → `"timeout"` (ms) | shown; doesn't reset the timeout | 20 s |
| MCP Inspector 2.x | yes | 60 s → `requestTimeout` in a config file | resets the timeout | 20 s |

## Long calls: the job pattern

A launch takes about 90 s, a save load about 33 s, `measure_ab` minutes. Claude Code waits that long; most other clients don't, and when they give up they cancel the call (quen then undoes what the call changed).

- Every call that runs on quen's session thread is a **job**. A client that isn't Claude Code (`clientInfo.name` other than `claude-code` at `initialize`) gets `{"job": "j3", "state": "running", ...}` once the call has run **20 s**, and at once with the input **`wait: false`** (offered by the tools that can run longer: `session_launch`, `session_end`, `capture_clean`, `game_load`, `game_save`, `game_keys`, `game_screenshot`, `measure_presentmon`, `measure_trace`, `measure_ab`, `mem_scan`, `mem_find`, `mem_patch_instances`, `sweep_play`, `game_frame`, `run_plan`): a launch under a profiler, a save load (through the command channel or the console), a RenderDoc capture and a frame-step sweep can each take minutes. The channel verbs (`game_listsaves`, `game_pause`, `game_settime`, `game_weather`, `game_teleport`, `game_items`, `world_get`, `world_actors`, `world_frame`, `world_ground`, `world_ray`, `world_nav`, `game_exec`, `game_camera`) answer within 5 s, and `game_weather` with `settle_s` that much later.
- **`job_status {job}`** waits up to that budget for the call to end, then returns **the call's own envelope**, unchanged, and forgets the job. Until then it returns the job's `state` (`queued` or `running`), `elapsed_s` and its latest progress `message`.
- **`job_cancel {job}`** sets the same flag an MCP cancel sets: the call stops at its next step and unwinds; `job_status` then returns that envelope.
- **`QUEN_JOB_BUDGET_S`** in the environment of `quen mcp` (each client's MCP config has an `env` block) sets the budget for that client: seconds, `0` to wait for every call. Use a value well below the client's timeout. `job_status` waits the same time.
- One call runs at a time, as before; journaling and refuse-before-acting are unchanged. Progress notifications stop once a call has answered a job (its request is done).
- Every step of a call is also a progress notification (`check done`, `natives done`, ...), so a launch reports progress to clients that ask.
- Not used: MCP's own experimental *tasks* (2025-11-25). VS Code supports them; the others don't yet. A tool the model polls works in every client.

## Tool schemas: the portable subset

What each client does with an `inputSchema`:
- **Codex** converts it to its own schema type. It keeps `type`, `description`, `enum`, `items`, `minItems`, `properties`, `required`, `additionalProperties`, `anyOf`/`oneOf`/`allOf`, `$ref`/`$defs`. It drops `pattern`, `format`, `minLength`/`maxLength`, `minimum`/`maximum`, `default`, `title`. It sends tools to the API with `strict: false`. **A schema over 5,000 bytes is compacted, starting by stripping every description.** ([json_schema.rs][codex-schema], [compaction.rs][codex-compact])
- **The Agents SDK** passes schemas through; strict conversion is off by default (`convert_schemas_to_strict`). ([agent.py][agents-py])
- **VS Code** drops a tool whose schema fails draft-07 validation, and errors on an array without `items`. For GPT models Copilot strips `pattern`, `format`, min/max and similar, and **cuts descriptions at 1,024 characters**. At most 128 tools per request. ([toolSchemaNormalizer.ts][vsc-norm], [tools doc][vsc-tools])
- **Gemini CLI** sends the schema as `parametersJsonSchema`, turning `["x","null"]` into `nullable`. It **adds a `wait_for_previous` boolean to every tool** and passes it on to the server when the model sets it. ([tools.ts][gem-tools], [mcp-tool.ts][gem-mcp])
- **OpenAI strict mode** (not on by default anywhere): every field required, `additionalProperties: false` everywhere, so free-form maps are impossible. ([structured outputs][oai-strict])
- **Names:** Codex shows `[A-Za-z0-9_]` only; VS Code `mcp_<prefix>_<tool>` cut at 64; Cursor server + tool name ≤ 60; Gemini `mcp_<server>_<tool>` cut at 63; OpenAI functions `[a-zA-Z0-9_-]{1,64}`.

So every quen tool keeps to this subset, checked by the unit test `every_tool_schema_is_portable` (`crates/quen/src/mcp.rs`):
- root `type: object` with `properties`, `additionalProperties: false`; no `anyOf`/`oneOf`/`allOf`/`$ref`;
- every property has one string `type` and a description; an `enum` only of strings, with `type: string`; every array has `items`;
- no free-form maps: every nested object names its properties and has `additionalProperties: false`;
- only `pattern`, `minimum`/`maximum`, `minItems`/`maxItems` beyond those, as hints: quen checks every value itself;
- tool names of `a-z` and `_`, at most 40 characters; a description of at most 1,024 characters; a schema of at most 5,000 bytes.

Changes this needed (2026-10-18): `session_launch.preset` is a `Key=number,...` string (it was a map), `extra_env` lists its four switches, `measure_ab` actions name their fields, `ledger_ingest` rows are JSON text (objects are still accepted), and inputs that took a number or text (`game_cvar.set`, the `mem_*` addresses, `ledger_query.params`) are typed as strings (numbers are still accepted). quen ignores `wait_for_previous`.

Also for every client (2026-10-18, after feature tests 1):
- **Launch consent:** `session_launch`'s entry in `tools/list` carries `"_meta": {"anthropic/requiresUserInteraction": true}`. Claude Code (2.1.214 or newer) then prompts on every call, in every permission mode; other clients ignore a `_meta` key they don't know, so there the user keeps `session_launch` on the client's own prompt. The dry run is its own tool, `session_plan`, which never prompts. `capture_clean` carries the same key (it moves capture files off the disk), and so does `mem_patch_instances` (it writes a field in every live instance of a class); `capture_usage`, which only reports, doesn't.
- **The agent's guide** comes from the server: `quen_guide` returns the playbook or the ledger's description, compiled in, so it works in every client and matches the installed version.
- **The focus switch** (`focus`, 2026-10-19): when a client connects, quen walks the parent chain from its `quen mcp` up to the first process with a visible top-level window that isn't a tool window: the client's own app, or the terminal a CLI client runs in. So `to: "client"` needs nothing from the client, and works in any client that has a window. A client with none in that chain (an SDK script started by a service, a CI runner) gets a refusal saying so. A classic console window reports its shell's process, so a CLI in one is found too (checked 2026-10-19, without a game); the walk has not yet run under each client listed above. No `_meta` mark: it moves windows, and on the way back sends the game up to 3 escapes to close the pause menu it opened when it lost the front, as `game_keys` can.
- **Claude Code's prompt behind the game** (2026-10-21): Claude Code shows its prompt for a call quen marks (above) before the call reaches quen, so quen can't bring the chat forward for it. A PreToolUse hook in `%USERPROFILE%\.claude\settings.json` can: matcher `^mcp__quen__(session_launch|game_save|hands_take|mem_patch_instances|capture_clean)$`, command quen.exe's full path, args `["focus", "--to", "client"]`, timeout 5 s (the whole entry: [INSTALL.md](../INSTALL.md) §4; `install.ps1` offers it). `quen focus --to client` needs no session and doesn't talk to `quen serve`. Only while the foreground window belongs to a process of the config's `game_exe`, it brings to the front the window of the console it shares with the CLI that ran it (a classic console's window, or the Windows Terminal window that owns a pseudo console's, which needn't be an ancestor), else the first process up its parent chain with a window, as `focus` finds a client at connect (the desktop app). It never answers the prompt: nothing on stdout, exit 0 on every path, at most one line on stderr. Checked 2026-10-21 without a game: exit 0 and no stdout on every path, about 0.1 s; not yet run live. quen sets up no such hook for Codex or the other clients here: there the agent calls `focus` `to: "client"` first.
- **A UTF-8 BOM** before the first line (a Windows PowerShell 5.1 / .NET Framework client writes one) is skipped (test `a_bom_before_the_first_line_is_skipped`).

## Clients that hold servers in a Job Object

quen's daemon (`quen serve`) must outlive the client process, so a closed client never kills the game mid-save. The proxy starts it with `CREATE_BREAKAWAY_FROM_JOB`. A client that puts its server in a Job Object that **kills on close and forbids breakaway** leaves no way out, so quen refuses to start the daemon there, loudly, and its error names `quen serve --stay` (test `a_job_that_refuses_breakaway_fails_loudly`):
- **Codex CLI** launches every stdio server suspended inside `JobObject::create_without_breakaway()`: `KILL_ON_JOB_CLOSE`, no `BREAKAWAY_OK` ([stdio_server_launcher.rs][codex-launch], [job.rs][codex-job]; in source since about 2026-08).
- **The Python MCP SDK** (the Agents SDK's stdio client) assigns the server to a job with `KILL_ON_JOB_CLOSE` only ([win32/utilities.py][py-job]).
- Node-based clients (VS Code, Cursor, Gemini CLI, the JS Agents SDK, the Inspector) use libuv's job, which allows silent breakaway: verified with the Inspector and the TypeScript SDK below.

`quen mcp` uses a daemon that already listens for the same config, and starts one only when none does. So with these clients, start the daemon yourself first: it then runs under your terminal, outside the client's job.

1. In a terminal, run the same `quen.exe` the client's config names: `quen serve --stay`. It prints one line: its pid, its pipe, its config file, and that Ctrl+C ends it. Leave the terminal open. Unlike a daemon `quen mcp` starts, it doesn't exit 5 s after its last client.
2. Open the client. Its `quen mcp` connects to that daemon. Codex's entry in `%USERPROFILE%\.codex\config.toml`:

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
   
   [mcp_servers.quen.tools.mem_patch_instances]
   approval_mode = "prompt"
   ```
3. When you're done, press Ctrl+C in that terminal: a live session ends the normal way (saves first, the game closed, everything put back), then the daemon exits. Closing the terminal window instead gives it only 5 s (Windows' limit for a closing console, [HandlerRoutine][handler]); a session end that takes longer is cut off, and the game closes with the daemon, as when any `quen serve` exits (the next `quen serve` replays the journal it left).

- Both ends check that the other is the same `quen.exe` and the same user, so the terminal's `quen` and the client's `command` must be one file. Both find the config the same way (`QUEN_CONFIG`, else `%USERPROFILE%\.quen\config.toml`); with another config, the client's `quen mcp` looks for another daemon.
- Its log goes to that terminal, not to `logs\serve.log` under the data root.
- A second `quen serve --stay` for the same config refuses and changes nothing. So does one started while a daemon another client started is still running: it exits 5 s after its last client.
- Tested against a simulated Codex job (kill on close, no breakaway; test `a_relay_in_a_no_breakaway_job_uses_a_stay_daemon`).
- With real Codex CLI 0.163.0-alpha.10.2 (2026-10-19), on a `quen serve --stay` started in PowerShell:
  - `session_plan`, then `session_launch`: it answered `{job}` after 20 s, and `job_status` returned the launch's result about 46 s after the call; then `world_get`, `game_screenshot`, `focus` both ways and `session_end`, which restored every file it had changed.
  - Closing Codex during a live session: quen ended it the normal way (the boot's outcome `ok`, every patched file back to its sha256 from before the launch), and the `--stay` daemon kept running.
  - Not tried with the Python SDK.
- **Codex's approvals.** Codex decides per tool call from the server's tool annotations ([mcp_tool_call.rs][codex-approval], `requires_mcp_tool_approval`): a tool needs approval unless it is marked read-only, or marked both not destructive and not open-world. From quen 0.1.8 quen marks its read-only tools `readOnlyHint` (`session_status`, `session_plan`, `quen_guide`, `quen_doctor`, `ledger_query`, `ledger_about`, `capture_usage`, `job_status`, `world_get`, `world_actors`, `game_listsaves`, `game_scriptlog`, the `mem_*` reads and `data_find`, `data_read`, `data_scan`) and no other, so by default Codex asks before every other quen call (before 0.1.8: every call), and during a launch the user switches from the game to Codex for each one (quen can't see a client's prompt, so `focus` can't bring it up). Under the approval policy `never` (Codex's Full access), a call that needs approval is refused ("MCP tool call requires approval, but approval policy is never"): checked 2026-10-19, `game_screenshot` and `focus` were refused. With `default_tools_approval_mode = "approve"` and `session_launch` on `"prompt"`, only the launch asked: Codex showed its approval card, and every other call ran without one (2026-10-19). The entry above also keeps `game_save`, `hands_take`, `capture_clean` and `mem_patch_instances` on `"prompt"`: the calls Claude Code asks for on every call (quen's `_meta` mark), which Codex ignores. A per-tool `approval_mode` wins over the server's default in Codex's code (`custom_mcp_tool_approval_mode`); those four weren't tried in Codex.

## Conformance (2026-10-18, no game)

Run before the command-channel tools and launch profiles were added; the unit test and the job tests in `crates/quen/tests/session.rs` cover them.

- **MCP Inspector 2.11.0** CLI: `tools/list --strict` (its schema-portability check): before the changes above, warnings for the type unions only; none after.
- **The official conformance suite** (0.1.16) tests servers over HTTP only, so it can't run against stdio-only quen.
- **The TypeScript SDK client** (`@modelcontextprotocol/client` 2.3.1, which validates every message against the spec schemas), over stdio against a synthetic game (the integration tests' fake game and hold points), 43 checks, all passing:
  - raw JSON-RPC: version negotiation (2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05; an unknown version gets the newest), `ping`, parse error −32700, unknown method −32601, unknown tool −32602, unknown notifications ignored;
  - `tools/list` passes the SDK's schemas and every `inputSchema` compiles (Ajv, JSON Schema 2020-12); a refused call (`isError`, nothing changed); a dry run;
  - progress: token echoed, values strictly increasing, each with a message;
  - cancel: the SDK's abort sends `notifications/cancelled`; the step in flight ends, the launch unwinds, the folder is restored, and no response comes for the cancelled request;
  - jobs: at the default budget a held launch answers a job after 20.0 s; `job_status` returns the launch's own envelope; `wait: false` answers at once; `job_cancel` unwinds;
  - Claude Code (`clientInfo` claude-code): a 23 s launch still answers its own envelope.
- **Fixed:** a JSON-RPC batch (removed from MCP in 2025-06-18) got no answer at all; it now gets −32600.

## ChatGPT's web and desktop apps

They can't start a local stdio server: they need a public HTTPS endpoint (streamable HTTP) or OpenAI's Secure MCP Tunnel ([connect ChatGPT][chatgpt]). quen deliberately offers no remote server: it drives a game on your PC and holds its lock.

[codex-schema]: https://github.com/openai/codex/blob/8d9f6466ff0294785d56f55b1fe363d132fe4133/codex-rs/tools/src/json_schema.rs
[codex-compact]: https://github.com/openai/codex/blob/8d9f6466ff0294785d56f55b1fe363d132fe4133/codex-rs/tools/src/json_schema/compaction.rs
[codex-launch]: https://github.com/openai/codex/blob/8d9f6466ff0294785d56f55b1fe363d132fe4133/codex-rs/rmcp-client/src/stdio_server_launcher.rs
[codex-job]: https://github.com/openai/codex/blob/8d9f6466ff0294785d56f55b1fe363d132fe4133/codex-rs/utils/pty/src/win/job.rs
[codex-approval]: https://github.com/openai/codex/blob/18d09e1eda6811effcb4acc110795eec7e970135/codex-rs/core/src/mcp_tool_call.rs
[handler]: https://learn.microsoft.com/en-us/windows/console/handlerroutine
[agents-py]: https://github.com/openai/openai-agents-python/blob/v0.24.0/src/agents/agent.py
[py-job]: https://github.com/modelcontextprotocol/python-sdk/blob/v2.3.0/src/mcp/os/win32/utilities.py
[vsc-norm]: https://github.com/microsoft/vscode/blob/1.142.0/extensions/copilot/src/extension/tools/common/toolSchemaNormalizer.ts
[vsc-tools]: https://code.visualstudio.com/docs/agents/run/tools
[gem-tools]: https://github.com/google-gemini/gemini-cli/blob/ed96faedfd5571f055c44f2c9509d62321d1138d/packages/core/src/tools/tools.ts
[gem-mcp]: https://github.com/google-gemini/gemini-cli/blob/ed96faedfd5571f055c44f2c9509d62321d1138d/packages/core/src/tools/mcp-tool.ts
[oai-strict]: https://developers.openai.com/api/docs/guides/structured-outputs#supported-schemas
[chatgpt]: https://developers.openai.com/plugins/deploy/connect-chatgpt.md

Other sources: Codex docs <https://learn.chatgpt.com/docs/extend/mcp?surface=cli>, timeouts in `codex-rs/codex-mcp/src/rmcp_client.rs` (same commit); Agents SDK timeout `src/agents/mcp/server.py` (v0.24.0); VS Code `mcpServerRequestHandler.ts`, `mcpServer.ts` (1.142.0); Cursor <https://cursor.com/docs/mcp.md> and staff answers on its forum (2026-03-07, 2026-07-02); Gemini CLI `tools/mcp-client.ts` (same commit); Claude Code <https://code.claude.com/docs/en/env-vars>; MCP Inspector `clients/cli/README.md` and `docs/v1-to-v2-migration.md` (2.11.0); conformance suite <https://github.com/modelcontextprotocol/conformance> (v0.1.16).
