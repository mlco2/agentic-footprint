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

## Known gaps

- Not yet checked against a live Pi session (needs a provider login).
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
