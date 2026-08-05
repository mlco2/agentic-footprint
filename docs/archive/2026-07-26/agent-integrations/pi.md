# Pi integration decisions

- **Status:** deferred
- **Current package:** PI-1
- **Source revision:** `5bc1c2c0a6f07e00e8c240304182f213ab8d311f`
- **Task list:** `docs/action-plan-agent-integrations.md`

This file is the append-only design and evidence checkpoint for the Pi
integration. Update it after every completed or blocked work package.

## Baseline decisions

- Use a thin Pi extension as the primary collector boundary.
- Prefer Pi's first-party extension events and normalized assistant usage over
  transcript parsing.
- Emit Contract #1 JSONL directly; do not access the `af` SQLite database.
- Use `session_start` for `session_meta` and stable Pi tool-call IDs for action
  attribution.
- Determine the exactly-once completed usage event from real captures before
  choosing `message_end`, `turn_end`, or `agent_end`.
- Use the supported bash spawn hook or built-in-operation wrapper for PID
  attribution; do not replace Pi shell semantics.
- If PID capture cannot be implemented safely, ship explicit PID-fidelity gaps
  rather than blocking all Pi usage/action collection.

## Open decisions

1. Which completed event is authoritative for one provider call?
2. Are compaction/summarization calls visible and should they be emitted as
   separate `llm_call` facts?
3. How are provider retries represented across message and turn events?
4. Does a custom/remote tool need a different execution locus classification?
5. Can the spawn hook expose the child PID directly, or is a wrapper required?
6. What installation mechanism and extension path are stable across Pi builds?

## Evidence log

### 2026-07-26 — PI-1 partial capture

- Installed Pi `0.80.6` loaded `collectors/pi/capture-events.ts` directly with
  `--extension`.
- A no-provider startup emitted `session_start` then `session_shutdown` with a
  stable UUID session ID and isolated JSONL session file.
- A real configured-provider attempt reached the provider but failed because
  the selected AWS SSO token was expired.
- The failed turn established this ordering:
  `session_start`, `before_agent_start`, `agent_start`, `turn_start`, user
  `message_start/end`, assistant `message_start/end`, `turn_end`, `agent_end`,
  `agent_settled`, `session_shutdown`.
- The same completed assistant error and zero usage appeared in
  `message_end`, `turn_end`, and `agent_end`; therefore only one may become the
  authoritative `llm_call` boundary.
- Full successful tool and retry captures were not completed.

### 2026-07-26 — prioritization decision

- **Status:** deferred
- **Reason:** the Pi integration requires an installed in-process TypeScript
  extension, provider fixture work, and a spawn wrapper for precise PIDs. The
  project will revisit it if Pi gains traction.
- **Resume from:** refresh/configure Pi provider auth or register its faux
  provider in a capture extension, then continue PI-1.

## Blocker record template

### YYYY-MM-DD — PI-N

- **Status:** blocked
- **Evidence:**
- **Why the smallest honest implementation cannot proceed:**
- **Options considered:**
- **Recommendation:**
- **Resume from:**

## Closeout checklist

- [ ] Raw event fixtures committed
- [ ] Contract #1 mapping documented
- [ ] Deterministic tests pass
- [ ] Live test implemented
- [ ] Manual live result recorded
- [ ] Capability gaps published
- [ ] User guide linked
