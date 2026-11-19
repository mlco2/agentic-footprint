# Pi collector

Pi collection is experimental. `af-collector.ts` is a Pi extension that turns
Pi's in-process lifecycle events into Contract #1 facts and appends them to
`$AF_STATE_DIR/spool/pi.<session-id>.jsonl`, the same file transport the
Claude Code hook and the OpenCode collector use. `af watch` / `af report`
ingest the spool; nothing is estimated in the extension.

`af` recognises the `pi` collector as the Pi agent (in `af status` and
`af summary`) only in a build with `--features experimental-pi`. Without it,
Pi's events are still ingested and counted, under "Other".

## Event boundaries

| Pi event | Contract #1 fact |
|---|---|
| `session_start` | one `session_meta` (stable `event_id`, so a resumed session is stored once) |
| `message_end` with `role: "assistant"` | one `llm_call` |
| `tool_execution_start` + `tool_execution_end`, paired by `toolCallId` | one `action_span` |
| `session_shutdown` | closes still-open spans as `cancelled` |

The same assistant message is repeated in `turn_end` and `agent_end`; the
collector deliberately ignores both, or every model call would be counted
two or three times. Tool ends arrive in completion order in Pi's parallel
tool mode, so spans are paired by `toolCallId`, never by order.
`before_provider_request` only records when the request started, for
`llm_call.duration_ms`.

The extension never reads prompts, message content, tool arguments or tool
results. Every handler is wrapped so a collector failure can never break a
Pi turn; failures are appended to `$AF_STATE_DIR/tmp/pi-collector-errors.log`.

## Use it

```sh
pi --extension /absolute/path/to/collectors/pi/af-collector.ts
```

or copy `af-collector.ts` into Pi's global extensions directory
(`~/.pi/agent/extensions/`). `af setup` does not install it yet.

## Tests

```sh
collectors/pi/test_collector.sh
```

It runs the unit tests (`node --test collectors/pi/af-collector.test.ts`,
Node >= 23.6 runs TypeScript directly), replays `test-data/session.jsonl`
through the extension with a fake Pi (`test-data/replay.ts`), validates every
spooled line with `af validate-line`, ingests the spool with `af report` and
checks that `af summary` reports it as Pi.

The fixture is hand-written from Pi's documented event shapes
(`docs/extensions.md`, `docs/session-format.md` in `@mariozechner/pi-coding-agent`)
and the metadata `capture-events.ts` records. It should be replaced by a
capture of a real Pi session.

## Live check

Checked against a real Pi 0.73.1 session on 17 Nov 2026 (provider
`openai-codex`, model `gpt-5.6-luna`, prompt: run `ls`, read a file,
answer). Pi fired three assistant `message_end`s, each repeated in its
`turn_end`; the spool held exactly 1 `session_meta`, 3 `llm_call` and 2
`action_span` (`bash` → `bash`, `read` → `file_op`), all valid, and
`af report` ingested 6 events with 0 rejected. A provider error (an
unsupported model) produced one `llm_call` with `status: "error"`, zero
tokens and its duration.

What the live run showed that the fixture did not:

- With `openai-codex`, `responseModel` is absent, so `model_id_served` is
  omitted; reasoning tokens are not reported.
- Codex tool call IDs look like `call_…|fc_…`; `span_id` keeps them as is,
  and the `event_id` drops the `|`.
- An errored message has no `responseId`; its `event_id` falls back to the
  message timestamp.
- Pi 0.73.1's built-in `openai-codex` model list is out of date: every listed
  model is refused for a ChatGPT account (`400 … not supported when using
  Codex with a ChatGPT account`). Pass a current one explicitly, e.g.
  `--model openai-codex/gpt-5.6-luna`.

## Known gaps

- Compaction and branch summarisation call the model; whether they fire an
  assistant `message_end` is unverified, so their tokens may be missed.
- Whether each retried provider attempt gets its own `message_end` is
  unverified.
- Open tool spans are held in memory: if Pi crashes mid-tool, that span is
  lost (a clean exit closes it as `cancelled`).
- `capture-events.ts` records a separate `cacheWrite1h` count; only `cacheWrite` maps to
  `cached_write_tokens` until the schema decides how to carry it.
- No Pi version in `session_meta.agent_app` (the extension context does not
  expose one).

## Research capture

`capture-events.ts` is the earlier PI-1 evidence extension. It records event
ordering and structural metadata while excluding prompts, tool arguments, tool
outputs, and message content:

```sh
AF_PI_CAPTURE=/tmp/pi-events.jsonl \
  pi --extension /absolute/path/to/collectors/pi/capture-events.ts \
  --print "Run one harmless bash command and then stop."
```
