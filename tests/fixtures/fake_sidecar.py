#!/usr/bin/env python3
"""Fake sidecar fixture for `af-sidecar` framing tests
(crates/af-sidecar/tests/framing.rs) and golden-transcript replay tests
(e.g. crates/af-core/tests/estimate.rs).

Default mode reads newline-delimited JSON objects from stdin and reacts to
`op`:

  {"op": "echo", "id": N, ...}        -> {"id": N, "op": "echo", "echo": <original object>}
  {"op": "sleep", "id": N, "secs": S} -> sleeps S seconds, then {"id": N, "op": "sleep", "slept": S}
  {"op": "silent", "id": N}           -> never responds (simulates a hung sidecar; timeout test)

Anything else gets {"id": N, "error": "unknown op"}. Malformed lines are
skipped. Stdlib only, per the project's "Python sidecars: stdlib +
pinned deps only" constraint (this fixture ships no pinned deps at all).

`--replay <transcript.jsonl>` mode instead plays back a golden transcript:
alternating request/response lines (odd lines 1,3,5,... are the requests,
shown for human readability only and never inspected; even lines 2,4,6,...
are the responses to emit). For each line read from stdin (regardless of
its content — the incoming request's `id` is the only part that matters),
the next response line is emitted verbatim except its `"id"` is replaced
with the incoming request's `"id"`. This lets a real protocol client (e.g.
`af-sidecar::Sidecar`, which injects its own monotonic ids) drive a fixed
scripted conversation without the fixture needing to understand the
protocol it's replaying.
"""
import json
import sys
import time
from pathlib import Path


def replay(transcript_path: Path) -> None:
    lines = [line for line in transcript_path.read_text().splitlines() if line.strip()]
    responses = lines[1::2]
    next_response = 0

    for raw_line in sys.stdin:
        raw_line = raw_line.strip()
        if not raw_line:
            continue
        try:
            req = json.loads(raw_line)
        except json.JSONDecodeError:
            continue

        req_id = req.get("id") if isinstance(req, dict) else None
        if next_response >= len(responses):
            break
        resp = json.loads(responses[next_response])
        next_response += 1
        resp["id"] = req_id
        print(json.dumps(resp), flush=True)


def main() -> None:
    if len(sys.argv) >= 3 and sys.argv[1] == "--replay":
        replay(Path(sys.argv[2]))
        return

    for raw_line in sys.stdin:
        raw_line = raw_line.strip()
        if not raw_line:
            continue
        try:
            req = json.loads(raw_line)
        except json.JSONDecodeError:
            continue

        op = req.get("op")
        req_id = req.get("id")

        if op == "echo":
            resp = {"id": req_id, "op": "echo", "echo": req}
        elif op == "sleep":
            time.sleep(req.get("secs", 0))
            resp = {"id": req_id, "op": "sleep", "slept": req.get("secs", 0)}
        elif op == "zone_factors":
            resp = {
                "id": req_id,
                "status": "ok",
                "gwp_kg_per_kwh": {"min": 0.05, "max": 0.05},
            }
        elif op == "estimate":
            started_file = __import__("os").environ.get("AF_FAKE_ESTIMATE_STARTED_FILE")
            if started_file:
                Path(started_file).touch()
            time.sleep(float(__import__("os").environ.get("AF_FAKE_ESTIMATE_DELAY", "0")))
            resp = {
                "id": req_id,
                "status": "ok",
                "impacts": {
                    "energy": {"unit": "kWh", "total": {"min": 0.001, "max": 0.001}},
                    "gwp": {"unit": "kgCO2eq", "total": {"min": 0.00005, "max": 0.00005}},
                },
                "methodology": {"ecologits_version": "test"},
            }
        elif op == "silent":
            continue
        else:
            resp = {"id": req_id, "error": "unknown op"}

        print(json.dumps(resp), flush=True)


if __name__ == "__main__":
    main()
