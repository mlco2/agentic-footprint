#!/usr/bin/env python3
"""`af_estimator` sidecar: wraps `ecologits` (pinned in
`python/manifest.toml`) to estimate the environmental impact of remote LLM
calls, and to expose electricity-mix factors used elsewhere in the join
pipeline.

Newline-delimited JSON over stdin/stdout, one request per line, one
response per line (`af-sidecar::Sidecar` framing). Two ops:

  {"op":"estimate","provider":P,"model_name":M,"output_token_count":N,
   "electricity_mix_zone":Z,"request_latency":L}
    -> {"status":"ok","impacts":{...},"warnings":[...],
        "methodology":{"ecologits_version":V,"source":"bundled"}}
    -> {"status":"unknown_model"}   (ecologits.ModelNotRegisteredError)
    -> {"status":"missing_zone"}    (ecologits.ZoneNotRegisteredError)
  `request_latency` (seconds) is optional; when omitted, `None` is passed
  straight through to `ecologits.tracers.utils.llm_impacts`, which maps it
  to `math.inf` internally (its own token-driven generation-latency
  estimate, the methodology's native path) — and the response carries
  `"latency_missing": true`.

  {"op":"zone_factors","zone":Z}
    -> {"status":"ok","zone":Z,"gwp_kg_per_kwh":{min,max},"adpe":{...},
        "pe":{...},"wue":{...}}
    -> {"status":"missing_zone"}

Every response echoes the request's `"id"` (an unparseable line has none to
echo, so it's omitted). A per-request exception is caught and turned into
`{"status":"error","message":...}` rather than crashing the read loop —
the estimator must survive one bad request without taking the whole batch
down with it.
"""
import json
import sys

from ecologits import __version__ as ECOLOGITS_VERSION
from ecologits.electricity_mix_repository import electricity_mixes
from ecologits.tracers.utils import llm_impacts


def _range(lo: float, hi: float) -> dict:
    return {"min": lo, "max": hi}


def _value_range(value) -> dict:
    """`value` is either a plain float/int or an ecologits `RangeValue`
    (has `.min`/`.max`); either way, normalize to `{"min", "max"}`."""
    if hasattr(value, "min") and hasattr(value, "max"):
        return _range(value.min, value.max)
    return _range(value, value)


def _criterion(total_impact, usage_impact=None, embodied_impact=None) -> dict:
    crit = {"unit": total_impact.unit, "total": _value_range(total_impact.value)}
    if usage_impact is not None:
        crit["usage"] = _value_range(usage_impact.value)
    if embodied_impact is not None:
        crit["embodied"] = _value_range(embodied_impact.value)
    return crit


# The criteria of a response's `impacts` object, in wire order:
#
#   (name on the wire, ecologits attribute, does it have an embodied part?)
#
# The names differ in exactly one place: ecologits calls the water criterion
# `wcf`, while the schema (and Task 12's join) calls it `water`. `energy`
# and `water` are usage-only in ecologits' methodology — asking for an
# embodied part of either would raise, not return zero.
_CRITERIA = (
    ("energy", "energy", False),
    ("gwp", "gwp", True),
    ("adpe", "adpe", True),
    ("pe", "pe", True),
    ("water", "wcf", False),
)


def _impacts_to_json(impacts) -> dict:
    usage = impacts.usage
    embodied = impacts.embodied
    return {
        name: _criterion(
            getattr(impacts, attr),
            getattr(usage, attr) if usage else None,
            getattr(embodied, attr) if embodied and has_embodied else None,
        )
        for name, attr, has_embodied in _CRITERIA
    }


def handle_estimate(req: dict) -> dict:
    latency = req.get("request_latency")
    latency_missing = latency is None

    impacts = llm_impacts(
        provider=req.get("provider"),
        model_name=req.get("model_name"),
        output_token_count=req.get("output_token_count"),
        request_latency=latency,
        electricity_mix_zone=req.get("electricity_mix_zone"),
    )

    if impacts.has_errors:
        codes = {e.code for e in impacts.errors}
        if "model-not-registered" in codes:
            return {"status": "unknown_model"}
        if "zone-not-registered" in codes:
            return {"status": "missing_zone"}
        return {"status": "error", "message": "; ".join(str(e) for e in impacts.errors)}

    resp = {
        "status": "ok",
        "impacts": _impacts_to_json(impacts),
        "warnings": [str(w) for w in (impacts.warnings or [])],
        "methodology": {"ecologits_version": ECOLOGITS_VERSION, "source": "bundled"},
    }
    if latency_missing:
        resp["latency_missing"] = True
    return resp


def handle_zone_factors(req: dict) -> dict:
    zone = req.get("zone")
    mix = electricity_mixes.find_electricity_mix(zone)
    if mix is None:
        return {"status": "missing_zone"}
    return {
        "status": "ok",
        "zone": mix.zone,
        "gwp_kg_per_kwh": _range(mix.gwp, mix.gwp),
        "adpe": _range(mix.adpe, mix.adpe),
        "pe": _range(mix.pe, mix.pe),
        "wue": _range(mix.wue, mix.wue),
    }


def handle_request(req: dict) -> dict:
    op = req.get("op")
    if op == "estimate":
        return handle_estimate(req)
    if op == "zone_factors":
        return handle_zone_factors(req)
    return {"status": "error", "message": f"unknown op: {op!r}"}


def main() -> None:
    for raw_line in sys.stdin:
        raw_line = raw_line.strip()
        if not raw_line:
            continue

        try:
            req = json.loads(raw_line)
        except json.JSONDecodeError as exc:
            print(json.dumps({"status": "error", "message": f"invalid json: {exc}"}), flush=True)
            continue

        req_id = req.get("id") if isinstance(req, dict) else None
        try:
            if not isinstance(req, dict):
                resp = {"status": "error", "message": "request must be a JSON object"}
            else:
                resp = handle_request(req)
        except Exception as exc:  # noqa: BLE001 - a bad request must never crash the loop
            resp = {"status": "error", "message": str(exc)}

        resp["id"] = req_id
        print(json.dumps(resp), flush=True)


if __name__ == "__main__":
    main()
