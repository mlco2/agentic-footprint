# Coding-agent protocol and collector affinity — 2026-07-26

- **Status:** research, implementation, and historical decision record
- **Implemented integrations:** OpenCode and Codex CLI
- **Deferred pending adoption:** Pi coding agent
- **Deferred candidate:** Pi, pending adoption/traction
- **Scoped-out historical research:** Antigravity CLI and archived Gemini CLI
- **Reference implementation:** Claude Code hooks plus native OTLP
- **Evaluation target:** Contract #1 `session_meta`, `action_span`, `llm_call`,
  `process_sample`, and local-energy correlation

This note records the protocol research behind future coding-agent collectors.
It intentionally separates supported public contracts from private persistence
formats and reverse-engineered implementation clues. A collector that works
only because a vendor happens to serialize an internal protobuf or SQLite row
in a particular shape is a fallback, not a native integration.

## 1. Evaluation criteria

The current implementation needs more than token totals. A high-affinity agent
surface should provide:

1. a stable session identifier and honest session boundaries;
2. tool start and completion events with stable call identifiers;
3. tool status, execution locus, timestamps, and enough process context for
   local-energy attribution;
4. authoritative model identity and input/output/cache/thought usage;
5. request or turn identifiers that allow lifecycle and usage sources to be
   joined without double counting;
6. a documented, versioned, machine-readable transport;
7. non-blocking observation that does not make the agent depend on the
   collector for correctness;
8. replayable fixtures and an explicit source-precedence policy.

Affinity labels used below:

- **A — native:** maps directly to Contract #1 with supported identifiers and
  authoritative usage.
- **B — complementary:** strong supported surfaces exist, but two sources must
  be correlated or some fields remain incomplete.
- **C — adapter:** useful SDK, plugin, event bus, or JSON stream exists, but the
  integration owns more lifecycle or compatibility logic.
- **D — transcript fallback:** durable local records can be parsed after the
  fact, with limited live semantics or vendor guarantees.
- **E — unsuitable:** only human-oriented output or private formats are known.

## 2. Summary matrix

| Agent | Supported observation surface | Usage fidelity | Tool lifecycle | Session identity | Current affinity | Main risk |
|---|---|---|---|---|---|---|
| Claude Code | Native OTLP plus lifecycle hooks | Exact input/output/cache usage from OTLP | Pre/post/failure hooks | Stable hook and OTLP session ID | **A** | Two-source deduplication and hook latency |
| Codex CLI | Native OTLP, app-server v2, hooks, `exec --json` | Exact per-response OTLP token breakdown | Completed native tool results; typed app-server item lifecycle | Stable conversation and tool-call IDs | **A-** | Child PID absent; app-server remains optional and experimental |
| Antigravity CLI, scoped out | Five documented hooks, statusline JSON, print mode | No supported exact per-call usage stream found | Pre/post tool hooks | No documented `SessionStart`; conversation identity depends on hook payloads | **C-/D+ historical** | Usage gap and incomplete lifecycle boundary |
| Gemini CLI, archived | ACP plus native OTLP | Exact rich usage in OTLP; aggregate input/output in ACP response metadata | ACP tool-call updates | Stable ACP session ID | **A-/B+ historical** | Product archived; do not build a new collector |
| OpenCode | Typed server SSE stream, durable event log, plugin hooks, ACP | Exact step input/output/reasoning/cache usage and cost | Durable tool/shell start/progress/end events | Stable session, message, step, and call IDs | **A-/B+** | Native event route is experimental; child PID absent |
| Goose | Extension/provider architecture and OpenTelemetry support | Telemetry/provider dependent | Extension-visible tool execution | Session records available | **B/C** | Surface split across extensions and telemetry configuration |
| Pi coding agent | First-party in-process extension API and JSONL session model | Provider-normalized input/output/cache usage and cost on assistant messages | Rich session/turn/message/tool events | Stable session and tool-call IDs | **A-/B** | Collector is in-process; child PID needs spawn hook/wrapper |
| GitHub Copilot CLI | Hooks and command lifecycle integrations | Limited public exact-usage contract | Hooks cover agent/tool milestones | Session identifiers available to hooks | **C/D** | Hosted service hides authoritative token facts |
| Cursor Agent CLI | CLI JSON output and local transcripts, depending on mode | Partial/provider-managed | Some structured command output | Conversation identifiers exist | **D** | Human/product output is not an observability contract |
| Aider | Chat history, analytics, and model-cost accounting | Good post-hoc model/token/cost facts | No complete supported live tool-span protocol | Chat/session files | **D** | Primarily transcript-driven and weak process correlation |

The lower-priority rows are a planning inventory, not a commitment to implement
all of them. Before implementation, each requires a fresh capture against the
then-current release and its primary documentation.

Additional adapters named by the architecture's `ccusage` reuse review—Amp,
Droid, Qwen Code, Kimi CLI, and similar transcript-backed agents—remain
**unclassified beyond D/E** in this note. Their presence in an existing parser
does not prove a supported live protocol. Each needs the same primary-source and
real-capture review before being promoted into the main matrix.

## 3. Claude Code reference architecture

Claude Code remains the reference because its two supported surfaces divide
responsibilities cleanly:

```text
Claude hooks                         Claude native OTLP
session + local tool lifecycle       authoritative remote inference facts
            \                         /
             correlation by session
                       ↓
                  Contract #1
```

The current repository already implements this shape:

- `collectors/claude-code/af-hook.sh` emits session and action facts;
- `af-otlp` normalizes Claude and standard OTel GenAI logs into `llm_call`;
- the control plane deduplicates raw facts and joins local and remote impact.

This division should be reused where possible. Do not make an agent-specific
control protocol authoritative for remote usage when the agent also exports a
supported OTLP record carrying better usage facts.

## 4. Codex CLI

### 4.1 Researched surfaces

Research used installed `codex-cli 0.142.0`, its generated app-server v2 JSON
schemas, current CLI help, and official OpenAI documentation/source references.

#### App-server v2

The strongest Codex surface is the versioned app-server protocol. The installed
CLI generated typed schemas for:

- thread start, resume, status change, and close;
- turn start and completion;
- item start and completion;
- command execution, file change, MCP tool call, web search, agent message,
  reasoning, and delegated-agent item variants;
- stable `threadId`, `turnId`, and item identifiers;
- `ThreadTokenUsageUpdatedNotification` with last and cumulative:
  `inputTokens`, `cachedInputTokens`, `outputTokens`,
  `reasoningOutputTokens`, and `totalTokens`.

This is sufficient for high-fidelity session, turn, tool, and usage facts. It
also avoids scraping terminal rendering or local database internals.

#### Native OTLP

Codex supports OTLP export through its `[otel]` configuration. This is the
best long-term fit for inference telemetry when the exported events contain
model/request identifiers that can be correlated with app-server turns.
The exact emitted event set must be captured against the target release before
implementation; configuration support alone does not prove every needed
Contract #1 field is present.

#### Hooks

Codex supports hooks for selected lifecycle events. Hooks are useful as a
supplement for surfaces not exposed by app-server or for users running the
ordinary TUI rather than an `af`-managed app-server client. They should not be
assumed to replace the complete typed item stream or token-usage notifications.

#### `codex exec --json`

Noninteractive execution can emit JSONL events. This is useful for fixtures,
CI jobs, and a bounded adapter, but it does not cover ordinary interactive TUI
sessions and therefore cannot be the only collector.

#### Local state

Codex persists sessions in JSONL and SQLite-backed state. Existing project
research records synchronization and final-buffer failure cases. Treat these
formats as replay/cross-check sources only.

### 4.2 Proposed mapping

| Codex fact | Contract #1 mapping |
|---|---|
| Thread started/resumed | `session_meta` |
| Turn ID | `attribution.task_id` candidate |
| Command/file/MCP/web/delegation item | `action_span` |
| Item ID | `attribution.tool_call_id` |
| Thread token-usage **last** delta | `llm_call` |
| Thread token-usage cumulative total | Cross-check only; never emit directly |
| Command process information, if exposed | `action_span.pids` and sampling watch set |

Before emitting `llm_call`, verify whether one usage update corresponds to one
remote request, one model turn, or a cumulative agent turn containing retries.
Contract #1 currently models completed calls, not arbitrary cumulative counters.

### 4.3 Risk assessment

The original app-server-first plan was riskier than an OTel-primary
integration in operational coupling:

- app-server is a Codex-specific control plane, not a passive telemetry sink;
- an `af` client or proxy must correctly implement initialization, approvals,
  cancellation, streaming, and protocol-version changes;
- users running the normal TUI are not automatically observed by an
  app-server-only collector;
- token notifications may describe turn aggregates rather than individual API
  attempts, requiring careful semantics and deduplication.

It is nevertheless **less fragile than transcript parsing** because the schema
is generated, typed, and versioned. The correct description is therefore:

> high-fidelity data with higher integration coupling, not low-quality data.

The implemented topology avoids that coupling: native Codex OTel feeds the
existing receiver during ordinary CLI/TUI operation. App-server remains a
future higher-fidelity option for products that already embed Codex, not a
runtime dependency of agentic-footprint.

### 4.4 Recommended plan

1. Save the generated app-server v2 schemas as versioned test inputs, including
   the Codex CLI version and schema-generation command.
2. Capture one real app-server session covering commands, edits, MCP, failure,
   cancellation, delegation, and multiple model turns.
3. Capture the same session's OTLP output and determine correlation keys.
4. Write an offline app-server normalizer before building a live client.
5. Decide whether `af` should be an app-server client, a transparent proxy, or
   a passive plugin/hook integration for normal TUI sessions.
6. Emit action spans first; defer `llm_call` until delta and retry semantics are
   demonstrated.
7. Add `exec --json` as a separate noninteractive adapter, not an alias for the
   app-server collector.

## 5. Antigravity CLI — scoped out

### 5.1 Product and source status

Research used:

- official `google-antigravity/antigravity-cli` at commit
  `c6911187d1db55e4ae1d5fa4b6f40f7af5af7aee`, dated 2026-07-26;
- installed Antigravity CLI `1.1.7`;
- official statusline examples and changelog;
- official Antigravity hook and CLI documentation;
- local configuration and storage filenames, without reading conversation
  content.

The GitHub repository is not the CLI implementation source. It publishes
installation information, changelogs, and examples. The executable is a
closed-source binary sharing the Antigravity agent engine and settings.

### 5.2 Supported public surfaces

#### Hooks

At the researched release, Antigravity documents five hook events:

- `PreInvocation`
- `PostInvocation`
- `PreToolUse`
- `PostToolUse`
- `Stop`

The changelog confirms hooks are loaded from shared Antigravity configuration,
including workspace-local `.agents/hooks.json`, plugin hooks, and the shared
user hook path under `~/.gemini/config/`. It also confirms hooks are operational
enough to affect tool execution, so collector hooks must be fast, fail-open,
and silent.

No supported `SessionStart` hook was found for CLI `1.1.7`. A collector must not
fabricate an exact session start from the first observed tool. Possible honest
alternatives are:

- emit `session_meta` lazily on the first invocation/tool/stop payload carrying
  a conversation identifier;
- use the statusline/title payload as a best-effort earlier observation;
- add an explicit `af antigravity start` managed-launch boundary;
- request a first-party `SessionStart`/`SessionEnd` hook upstream.

Real hook payloads still need to be captured. Event names alone do not prove
the presence or stability of conversation ID, tool-call ID, timestamps,
arguments, results, status, or process IDs.

#### Statusline and title JSON

Antigravity invokes user scripts with JSON on stdin. The official examples use:

- `agent_state`: `initializing`, `idle`, `thinking`, `working`, or `tool_use`;
- `context_window.used_percentage`;
- `model.display_name`;
- `workspace.current_dir`;
- VCS and sandbox state;
- artifact, subagent, and task counts;
- terminal width.

This is a valuable health and session-discovery signal, but it is not an event
log and does not expose exact token deltas. Repeated redraw payloads must never
be converted directly into `llm_call` or action events.

#### Headless print mode

`agy -p`/`--print` is supported for noninteractive automation and conversation
resumption. No documented structured JSON or JSONL event mode was found.
Human-oriented stdout is unsuitable as a primary collector protocol.

#### Conversation storage

The changelog identifies SQLite as the CLI conversation format and records
resume/import behavior. The installed product also stores protobuf-backed
settings and conversation-related state. These are private persistence formats,
not supported observability contracts. Transcript parsing may become a fallback
for post-hoc usage if real captures prove stable and complete, but it should not
be the first implementation.

#### Telemetry and usage

No user-configurable OTLP exporter or supported exact per-call token telemetry
surface was found. Public statusline data exposes context-window percentage and
quota-oriented UI state, not authoritative input/output/cache/thought counts.

Private binary symbols indicate internal model-usage and token structures, but
those are evidence that the product knows the values—not permission to depend
on an undocumented wire or protobuf representation.

### 5.3 Proposed mapping

| Antigravity fact | Contract #1 mapping |
|---|---|
| First observed conversation-bearing hook | Lazy `session_meta` |
| `PreToolUse`/`PostToolUse` | `action_span` start/completion |
| `PreInvocation`/`PostInvocation` | Candidate prompt/task boundary; semantics require capture |
| `Stop` | Best-effort turn/session closure, depending on payload semantics |
| Statusline `agent_state` | Debug health only |
| Statusline model/context percentage | Session metadata/debug only, never token usage |
| SQLite conversation | Replay/cross-check fallback only |

### 5.4 Risk assessment

Antigravity is operationally similar to Claude hooks but materially weaker for
remote inference accounting:

- tool spans may be straightforward once real payloads are captured;
- missing `SessionStart` makes lifecycle boundaries incomplete;
- no supported exact usage exporter means EcoLogits requests cannot be built
  with Claude-level confidence;
- closed-source implementation and shared GUI/CLI state increase compatibility
  and privacy risk for any persistence parser;
- hooks execute in the agent path and must not add noticeable latency.

Current affinity is therefore **C-/D+**: promising for local action and process
measurement, inadequate for authoritative remote impact until an exact usage
surface is identified.

This target is scoped out after review. Preserve the findings for history, but
do not perform the capture spike or add an Antigravity collector unless a
supported exact-usage export and first-class session lifecycle become
available.

### 5.5 Recommended spike

1. Configure one no-op capture hook for all five events and record sanitized
   payloads from Antigravity CLI `1.1.7`.
2. Exercise a matrix covering normal tool success, failure, denial,
   cancellation, MCP, background tasks, subagents, resumed conversation, and
   print mode.
3. Measure hook invocation ordering, blocking behavior, timestamps, duplicate
   events, and whether tool IDs survive retries and subagents.
4. Record statusline payloads for startup, thinking, tool execution,
   compaction, subagents, and shutdown.
5. Determine whether hooks expose the Antigravity process or child PID. If not,
   use a managed launcher to establish the root PID honestly.
6. Inspect a sanitized SQLite conversation copy only after hook coverage is
   understood; determine whether exact usage exists and whether rows are
   durable before exit.
7. Ask Google for a supported OTLP/GenAI exporter or exact usage fields in
   `PostInvocation` before committing to remote-impact support.

Initial implementation, if the spike succeeds, should advertise capabilities
honestly:

```text
collector: antigravity-hooks
signals: session (partial), action lifecycle
emits: session_meta, action_span
remote usage: unsupported
lifecycle fidelity: no first-class session start
```

## 6. Gemini CLI historical record

Gemini CLI was previously selected as the second target because it combined
native OTLP with Agent Client Protocol (ACP). The consumer Gemini CLI service
ended on 2026-06-18 and the product/repository is archived according to the
retirement notice reviewed for this research; no new collector should be
implemented. Preserve the exact retirement notice with future archival copies
of this note because product redirects may change.

The researched historical design remains useful protocol evidence.

### ACP findings

At local Gemini CLI commit
`3818efbbfbf8ef029ef53a6ab1093db39971ce83` with ACP SDK `0.16.1`:

- `gemini --acp` used JSON-RPC over stdio;
- stable ACP session IDs and prompt request boundaries were available;
- session updates included user/agent/thought chunks and tool-call lifecycle;
- tool updates carried stable call IDs, kind, status, locations, and output;
- final prompt response `_meta.quota` carried aggregate input/output totals and
  per-model input/output totals;
- usage was not streamed as a session update;
- native OTLP carried richer per-request usage, cache/thought facts, errors,
  duration, `session.id`, and internal `prompt_id`.

The recommended historical architecture was ACP for lifecycle plus OTLP for
authoritative usage, with ACP quota metadata as fallback or cross-check. The
important lesson is that an agent-control protocol and a telemetry protocol can
be complementary, but their totals must never be added without deduplication.

## 7. OpenCode

### 7.1 Researched source and architecture

Research used local OpenCode commit
`7534d23551f665e65080809975b4ca5c7d63807b` dated 2026-07-25.

OpenCode has moved beyond a loose plugin event bus into a typed protocol/server
architecture:

- canonical event schemas live in `packages/schema`;
- durable events carry an event ID and aggregate sequence metadata;
- `/api/event` exposes native events as server-sent events;
- generated clients expose the same event union;
- plugins can also observe the event stream and tool execution hooks;
- OpenCode includes an ACP service, but ACP is not required for collection.

The native event route is currently described as experimental. Its typed schema
and durable sequencing are strong engineering boundaries, but compatibility
must still be pinned to an OpenCode version.

### 7.2 Exact useful events

The `session.next.*` event family is unusually close to Contract #1:

- all events carry `sessionID` and a timestamp;
- `Step.Started` carries assistant message ID, agent, and model reference;
- `Step.Ended` carries finish reason, cost, and exact token fields:
  input, output, reasoning, cache read, and cache write;
- `Step.Failed` carries a structured error;
- `Tool.Called`, `Tool.Progress`, `Tool.Success`, and `Tool.Failed` carry stable
  call ID, tool name, input/result, provider-executed flag, and metadata;
- `Shell.Started` and `Shell.Ended` carry stable call ID, command, and output;
- reasoning and text have started/delta/ended streams;
- retry events are explicit;
- durable event sequence numbers provide replay and deduplication boundaries.

`Step.Ended` appears to represent one completed model step and is the strongest
direct `llm_call` candidate found outside native OTLP. A real trace must confirm
how provider retries, server-executed tools, context compaction, and multi-model
routing affect step counts and token totals.

### 7.3 Proposed mapping

| OpenCode event | Contract #1 mapping |
|---|---|
| First event for a session plus session query | `session_meta` |
| `session.next.step.started/ended/failed` | One `llm_call` lifecycle; emit completed call from ended/failed evidence |
| `session.next.tool.called/success/failed` | `action_span` |
| `session.next.shell.started/ended` | User or agent shell `action_span` |
| Durable event `id` and sequence | Event ID/deduplication and replay cursor |
| Model reference | Provider/model resolution through OpenCode model registry |

Do not emit both a generic tool event and its nested shell event as two local
actions unless traces prove they represent distinct executions. Define a
classification rule such as “shell events enrich the corresponding tool call”
when call IDs match.

### 7.4 Process attribution gap

OpenCode's shell implementation has access to the child process handle, but the
public durable shell/tool events do not currently publish the child PID. The
collector therefore has three choices:

1. watch the OpenCode root process tree and attribute by overlapping tool spans;
2. add an OpenCode plugin/core hook that publishes the spawned PID;
3. upstream a minimal optional `pid` field on shell-start events.

Option 3 is the cleanest. Option 1 is sufficient for a first local-energy spike
but is less precise under concurrent tools and background descendants.

### 7.5 Recommended integration

Prefer an **external Rust collector** subscribing to the typed SSE stream:

```text
OpenCode server / TUI
       ↓ typed SSE, replayable durable events
af-opencode adapter
       ↓ Contract #1 spool
```

Benefits:

- the agent does not load or depend on `af` code;
- collection can reconnect using durable aggregate sequence numbers;
- exact usage and tool lifecycle come from one source, reducing deduplication;
- Rust can consume generated JSON schemas/fixtures without embedding Bun.

Spike order:

1. capture a sanitized SSE trace with normal completion, retry, error,
   cancellation, shell, file edit, MCP/server tool, subagent, and compaction;
2. confirm replay behavior after disconnect and sequence-cursor semantics;
3. map model references to provider and model IDs;
4. verify whether each `Step.Ended` is one billable provider response;
5. implement an offline normalizer;
6. add live SSE subscription;
7. pursue PID enrichment upstream.

Current affinity: **A-/B+**. This is the preferred first new external-agent
collector.

### 7.5 Addendum 2026-07-26 — default TUI mode has no server (v1.18.5)

Verified against installed OpenCode v1.18.5 on a live user session:

- The plain `opencode` TUI binds **no TCP listener at all** (lsof capture of
  the running process: only the sqlite state files and the log are open).
  The durable `/api/event` SSE surface — everything §7.2/§7.3 and
  `af collect opencode` are built on — exists only under `opencode serve`,
  `opencode web`, or a TUI attached to such a server.
- In non-server modes, plugins receive a **placeholder `serverUrl`**
  (`http://localhost:4096/`, the `serve` default) with nothing bound behind
  it; an HTTP probe from inside a plugin fails to connect. The SDK `client`
  handed to plugins points at the same dead URL. Empirical probe: a plugin
  that fetches `serverUrl + /api/session` at load, run via `opencode run`
  in a scratch project.

**Product constraint (decision):** users' default workflow must not change.
No wrapper command, no serve-plus-attach ritual, no config that silently
alters how OpenCode itself runs. Transparent collection for the default TUI
therefore has exactly one supported surface: the in-process plugin `event`
hook (`@opencode-ai/plugin`, `Hooks.event`), which observes the same typed
event union in every mode. Contract #1 normalization must stay in `af`; a
plugin may only capture and hand off raw events (dumb forwarder), never map
them.

**Affinity consequence:** the A-/B+ label applies to server mode only.
Default-TUI observation is plugin-hook-or-nothing, and the durable
sequence/replay guarantees of the SSE route do not apply to the in-process
hook — a collector fed by it is live-capture only.

**Native OTel (same-day finding):** OpenCode also ships an experimental
in-process OTel stack — `Observability.layer` (NodeSdk, BatchSpanProcessor,
OTLP HTTP JSON for logs and traces via standard `OTEL_*` env), plus AI SDK
`experimental_telemetry` gen-ai spans (token usage) gated behind config
`experimental.openTelemetry` (`packages/opencode/src/session/llm.ts`).
Because it is in-process instrumentation, it works in default TUI mode —
no server required. It is not yet a dependable surface: upstream issues
report `OTEL_*` env handling partially ignored (#25839, #33101) and spans
silently lost in `run` mode because the runtime is never disposed before
`process.exit()` (#30087); it exports *traces*, while the `af` receiver
currently accepts only logs and metrics. Long-term this is the preferred
surface (matches the "prefer native OTLP" principle); near-term the plugin
`event` hook remains the only surface that is both mode-independent and
lifecycle-reliable.

**ACP (same-day finding):** `opencode acp` is shipped and data-rich —
`src/acp/usage.ts` maps the full assistant token breakdown (input, output,
reasoning, cache read/write, cost) into the ACP SDK's first-class `Usage`
type, `service.ts` emits `usage_update` session notifications, and
`tool.ts` maps tool-call lifecycle updates; `newSession` and `loadSession`
are both implemented. It does not, however, help the default-TUI
constraint: the transport is stdio only (`ndJsonStream` over
`process.stdin`/`stdout`), the connection is client-driven (the ACP client
owns and prompts its sessions), and a TUI session running in another
process never streams into an ACP connection — instances share only the
at-rest sqlite state. ACP is therefore a *complementary* surface for the
editor-embedded persona (Zed-style clients), where the integration shape
is a one-time editor-config change pointing at an `af` stdio proxy that
relays the JSON-RPC stream and taps `usage_update` + tool updates — the
same pattern the archived Gemini research rated A-/B+. No `af` ACP
collector exists yet.

## 8. Pi coding agent

### 8.1 Researched source and architecture

Research used local Pi commit
`5bc1c2c0a6f07e00e8c240304182f213ab8d311f` dated 2026-07-25.

Pi's first-party TypeScript extension API is an explicit observability and
customization boundary. Extensions can subscribe to:

- session start, shutdown, switch, fork, compaction, and tree navigation;
- provider request headers and response status;
- prompt/agent start, agent end, and fully-settled boundaries;
- turn start/end;
- message start/update/end;
- tool execution start/update/end;
- lower-level typed tool-call and tool-result hooks;
- model and reasoning-level selection.

The extension context exposes the stable session ID and session file. Sessions
are versioned JSONL trees with stable entry IDs and migrations.

### 8.2 Usage fidelity

Pi normalizes provider responses into assistant messages carrying:

- provider and model identity;
- input and output tokens;
- cache-read and cache-write tokens;
- total tokens;
- detailed cost fields;
- stop reason and error state.

`message_end`, `turn_end`, or `agent_end` can therefore produce authoritative
completed `llm_call` facts without separate OTLP. The spike must confirm which
event fires exactly once for retries, compaction helper calls, and failed or
aborted provider requests.

### 8.3 Tool and process lifecycle

`tool_execution_start` and `tool_execution_end` carry stable tool-call ID,
tool name, arguments, result, and error status. This maps directly to
`action_span`.

The normal tool event does not expose a child PID. Pi does, however, provide a
supported bash spawn-hook and pluggable bash operations. An `af` extension can:

- inject a session/tool correlation environment variable before spawn;
- wrap the local bash operation to observe the spawned PID;
- leave non-bash tools on logical lifecycle events;
- classify remote/custom tools separately.

The wrapper should reuse Pi's built-in execution behavior rather than replace
shell semantics, output buffering, cancellation, and process-tree cleanup.

### 8.4 Proposed mapping

| Pi event | Contract #1 mapping |
|---|---|
| `session_start` | `session_meta` |
| `session_shutdown` | Close session observation window |
| `before_agent_start` / `agent_settled` | Prompt/task boundaries |
| Completed assistant `message_end` or `turn_end` | `llm_call` |
| `tool_execution_start/end` | `action_span` |
| Bash spawn hook/wrapper | `action_span.pids` and watched process tree |
| Session ID and tool-call ID | Contract session and attribution IDs |

### 8.5 Recommended integration

Ship a small first-party-style Pi extension that writes Contract #1 JSONL
directly to the spool:

```text
Pi agent loop
  ↓ extension events + normalized usage
af-pi extension
  ↓ append-only Contract #1 spool
```

The extension should be intentionally thin:

- no SQLite access;
- no EcoLogits or methodology code;
- no network listener;
- bounded synchronous work and fail-open append errors;
- raw-event fixtures for every subscribed event;
- version and capability descriptor stamped into `collector` metadata.

Spike order:

1. install a capture-only extension and record the full event ordering;
2. cover provider retry, compaction, abort, custom tools, background bash, and
   session fork/resume;
3. identify the exactly-once completed assistant boundary;
4. prove the spawn wrapper exposes PIDs without changing behavior;
5. implement Contract #1 emission with golden fixtures;
6. test extension latency and failure isolation.

Current affinity: **A-/B**. Pi is the preferred first in-process collector and
may be simpler to implement than OpenCode, but deployment requires installing
an extension into the agent runtime.

## 9. Other agent families

### Goose

Goose has an extension/provider architecture and documented telemetry options.
It likely fits a combined telemetry-plus-extension model. The integration must
verify which lifecycle events are visible to extensions, whether telemetry
contains exact usage, and whether session IDs are shared across both surfaces.

### GitHub Copilot CLI

Hooks can expose useful lifecycle and tool events, but the hosted inference
service does not provide a clearly supported exact token-usage contract for
local accounting. Treat it as action-span capable and remote-usage incomplete
until primary evidence says otherwise.

### Cursor Agent CLI

Structured CLI output and transcripts can support automation and post-hoc
parsing, but a human/product JSON mode is not automatically a stable telemetry
contract. Prefer a documented hook or event API if one becomes available.

### Aider

Aider's model usage, cost accounting, and chat history are useful for post-hoc
`llm_call` reconstruction. Its live local-tool/process semantics are weaker,
so it fits a transcript adapter better than the current Claude-style live
collector.

## 10. Cross-agent architecture recommendation

Do not build one universal protocol proxy. Build small source adapters behind a
shared lifecycle record model:

```text
Claude hooks ───────┐
Claude/agent OTLP ──┤
Codex app-server ───┤
Codex OTLP ─────────┤       ┌─ source precedence
Antigravity hooks ──┼──────▶│  correlation
OpenCode events ────┤       │  capability gaps
Transcript adapters ┘       └─ Contract #1 envelopes
```

The internal record should retain:

- source protocol and source version;
- session, turn/prompt, subagent, and tool identifiers;
- source timestamps and observation timestamps;
- lifecycle phase and completion status;
- model and usage breakdown with completeness flags;
- process IDs and execution locus;
- `authoritative`, `fallback`, or `cross_check` provenance;
- raw-record digest for deterministic deduplication.

Contract #1 should not be expanded merely to mirror every vendor protocol.
First capture real traces and identify information that is required for impact
accounting but cannot be represented without ambiguity.

## 11. Priority order

1. **OpenCode evidence spike:** typed SSE capture, replay, usage semantics, and
   PID-enrichment decision.
2. **Codex evidence spike:** app-server plus OTLP capture and runtime-topology
   decision.
3. Keep Pi's partial extension evidence and revisit it if adoption increases.
4. Keep Antigravity and Gemini findings as historical research only.

## 12. Decision gates

### OpenCode implementation gate

Proceed if the SSE route replays durable events reliably, `Step.Ended` is shown
to be an honest completed inference unit, and version drift can be detected.
PID absence may remain an explicit local-attribution gap for the first spike.

### Pi implementation gate

Proceed if one extension event yields exactly-once completed assistant usage,
tool events remain stable under retries/compaction, and a spawn hook or wrapper
captures bash PIDs without altering Pi's execution behavior.

### Codex implementation gate

Proceed with app-server integration only after proving that observation does
not require replacing ordinary TUI workflows, or after explicitly accepting an
`af`-managed launch mode. Emit usage only after distinguishing per-turn deltas,
retries, compaction, and cumulative updates.

### General gate

Every collector must publish its capability descriptor and explicit gaps. A
partial collector that reports measured local tools and says remote usage is
unknown is preferable to a complete-looking impact total assembled from
context-window percentages, UI quota numbers, or inferred private fields.

## 13. Source ledger

Sources are grouped by confidence. Local revisions make later drift audits
possible even when vendor documentation changes.

### Primary local captures

- `google-antigravity/antigravity-cli` commit
  `c6911187d1db55e4ae1d5fa4b6f40f7af5af7aee` dated 2026-07-26:
  `README.md`, `CHANGELOG.md`, `examples/statusline/statusline.sh`, and
  `examples/title/title.sh`.
- Installed Antigravity CLI `1.1.7`: CLI version, public filesystem layout,
  and binary symbol inspection. Symbols were used only to identify research
  questions; they are not treated as supported contracts.
- `google-gemini/gemini-cli` commit
  `3818efbbfbf8ef029ef53a6ab1093db39971ce83` dated 2026-07-24:
  `packages/cli/src/acp/acpSession.ts`,
  `packages/cli/src/acp/acpRpcDispatcher.ts`,
  `integration-tests/acp-telemetry.test.ts`, and `docs/cli/acp-mode.md`.
- Installed Codex CLI `0.142.0`: `codex --help`, `codex exec --help`, and
  JSON schemas generated by
  `codex app-server generate-json-schema --out <directory>`.
- This repository's Claude Code collector, OTLP normalizers, fixtures, and
  Contract #1 schemas.
- OpenCode commit `7534d23551f665e65080809975b4ca5c7d63807b` dated
  2026-07-25: `packages/schema/src/session-event.ts`,
  `packages/core/src/event.ts`, `packages/protocol/src/groups/event.ts`,
  `packages/server/src/handlers/event.ts`, and plugin/ACP usage code.
- Pi commit `5bc1c2c0a6f07e00e8c240304182f213ab8d311f` dated
  2026-07-25: extension event types and runner, normalized AI message types,
  session manager, bash spawn hook, and tool implementations.

### Primary vendor documentation and repositories

- Antigravity CLI repository:
  `https://github.com/google-antigravity/antigravity-cli`
- Antigravity CLI overview:
  `https://antigravity.google/docs/cli/overview`
- Antigravity hooks:
  `https://antigravity.google/docs/hooks`
- Antigravity statusline documentation, linked by the official example:
  `https://antigravity.google/docs/cli-statusline`
- Codex configuration reference:
  `https://developers.openai.com/codex/config-reference/`
- Codex advanced configuration and OpenTelemetry:
  `https://developers.openai.com/codex/config-advanced/`
- Codex app-server documentation:
  `https://developers.openai.com/codex/app-server/`
- OpenAI Codex source repository:
  `https://github.com/openai/codex`
- Gemini CLI archived source repository:
  `https://github.com/google-gemini/gemini-cli`
- Agent Client Protocol:
  `https://agentclientprotocol.com/`
- OpenCode documentation and source:
  `https://opencode.ai/docs/` and `https://github.com/anomalyco/opencode`
- Goose documentation and source:
  `https://block.github.io/goose/` and
  `https://github.com/block/goose`
- Aider documentation and source:
  `https://aider.chat/docs/` and `https://github.com/Aider-AI/aider`

### Confidence limitations

- Antigravity is closed-source; only documented hooks/statusline contracts and
  observed release behavior are suitable implementation dependencies.
- The Codex app-server assessment is exact for generated schema version
  `0.142.0`, not a promise that future versions retain every field unchanged.
- Goose, Copilot, Cursor, and Aider entries are comparative planning
  classifications. They must be refreshed before implementation.
- No latency, ordering, or durability claim should be promoted from this note
  without a sanitized real-session fixture and a reproducible test.
