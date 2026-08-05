# Pi collector research

`capture-events.ts` is a temporary PI-1 evidence extension. It records event
ordering and structural metadata while excluding prompts, tool arguments, tool
outputs, and message content.

Run it explicitly:

```sh
AF_PI_CAPTURE=/tmp/pi-events.jsonl \
  pi --extension /absolute/path/to/collectors/pi/capture-events.ts \
  --print "Run one harmless bash command and then stop."
```

The capture is not a Contract #1 collector. It exists to choose the correct
exactly-once event boundaries before implementing `session_meta`, `action_span`,
and `llm_call` emission.
