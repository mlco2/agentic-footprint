"""Tests for `python/af_estimator/__main__.py`, the ecologits estimator
sidecar.

Requires `ecologits` to be importable. It's pinned in
`python/manifest.toml` and installed only into the managed venv that
`af python setup` provisions (via `uv`) — not necessarily into whatever
interpreter runs plain `pytest`. `pytest.importorskip` makes that the
CI-safe default (skips cleanly without it); the project's `AF_E2E=1` +
managed-venv gate is what runs this file for real, matching the split used
elsewhere in this PoC.

Driven as a real subprocess (not by importing `__main__` directly) so this
exercises the exact newline-delimited-JSON framing `af-sidecar::Sidecar`
speaks to it.
"""
import json
import subprocess
import sys
from pathlib import Path

import pytest

ecologits = pytest.importorskip("ecologits")

from ecologits.model_repository import models  # noqa: E402  (after importorskip)

MAIN = Path(__file__).resolve().parents[1] / "af_estimator" / "__main__.py"


def _any_anthropic_model() -> str:
    for m in models.list_models():
        if m.provider.value == "anthropic":
            return m.name
    pytest.skip("no anthropic model registered in this ecologits version")


class SidecarProcess:
    """Minimal line-oriented subprocess driver for these tests — a small
    Python-side stand-in for `af-sidecar::Sidecar`'s request/response
    framing, not a reimplementation of its id-matching/timeout logic."""

    def __init__(self) -> None:
        self.proc = subprocess.Popen(
            [sys.executable, str(MAIN)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )

    def request(self, obj: dict) -> dict:
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        assert line, f"sidecar produced no output (stderr: {self.proc.stderr.read()})"
        return json.loads(line)

    def send_raw_line(self, line: str) -> dict:
        self.proc.stdin.write(line + "\n")
        self.proc.stdin.flush()
        out = self.proc.stdout.readline()
        assert out, f"sidecar produced no output (stderr: {self.proc.stderr.read()})"
        return json.loads(out)

    def close(self) -> None:
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=5)


@pytest.fixture
def sidecar():
    proc = SidecarProcess()
    yield proc
    proc.close()


def test_known_model_returns_ranged_impacts_with_all_five_criteria(sidecar):
    model_name = _any_anthropic_model()
    resp = sidecar.request(
        {
            "id": 1,
            "op": "estimate",
            "provider": "anthropic",
            "model_name": model_name,
            "output_token_count": 500,
            "electricity_mix_zone": "WOR",
            "request_latency": 1.2,
        }
    )
    assert resp["id"] == 1
    assert resp["status"] == "ok"

    impacts = resp["impacts"]
    for key in ("energy", "gwp", "adpe", "pe", "water"):
        assert key in impacts, f"missing criterion {key!r} in {impacts!r}"
        assert "unit" in impacts[key]
        total = impacts[key]["total"]
        assert total["min"] <= total["max"]

    assert resp["methodology"] == {
        "ecologits_version": ecologits.__version__,
        "source": "bundled",
    }
    assert "latency_missing" not in resp


def test_missing_request_latency_is_passed_through_and_flagged(sidecar):
    model_name = _any_anthropic_model()
    resp = sidecar.request(
        {
            "id": 2,
            "op": "estimate",
            "provider": "anthropic",
            "model_name": model_name,
            "output_token_count": 500,
            "electricity_mix_zone": "WOR",
        }
    )
    assert resp["status"] == "ok"
    assert resp["latency_missing"] is True


def test_bogus_model_returns_unknown_model(sidecar):
    resp = sidecar.request(
        {
            "id": 3,
            "op": "estimate",
            "provider": "anthropic",
            "model_name": "not-a-real-model-xyz",
            "output_token_count": 100,
            "electricity_mix_zone": "WOR",
        }
    )
    assert resp == {"id": 3, "status": "unknown_model"}


def test_bogus_zone_returns_missing_zone(sidecar):
    model_name = _any_anthropic_model()
    resp = sidecar.request(
        {
            "id": 4,
            "op": "estimate",
            "provider": "anthropic",
            "model_name": model_name,
            "output_token_count": 100,
            "electricity_mix_zone": "NOT-A-REAL-ZONE",
        }
    )
    assert resp == {"id": 4, "status": "missing_zone"}


def test_malformed_request_line_returns_error_without_crashing_the_loop(sidecar):
    resp = sidecar.send_raw_line("not json{{{")
    assert resp["status"] == "error"

    # The sidecar must still be alive and answer the next (well-formed)
    # request — one bad line must not take down the read loop.
    model_name = _any_anthropic_model()
    resp2 = sidecar.request(
        {
            "id": 5,
            "op": "estimate",
            "provider": "anthropic",
            "model_name": model_name,
            "output_token_count": 100,
            "electricity_mix_zone": "WOR",
        }
    )
    assert resp2["status"] == "ok"


def test_unknown_op_returns_error_with_id(sidecar):
    resp = sidecar.request({"id": 6, "op": "not-a-real-op"})
    assert resp["id"] == 6
    assert resp["status"] == "error"


def test_zone_factors_wor_returns_ok(sidecar):
    resp = sidecar.request({"id": 7, "op": "zone_factors", "zone": "WOR"})
    assert resp["status"] == "ok"
    assert resp["zone"] == "WOR"
    for key in ("gwp_kg_per_kwh", "adpe", "pe", "wue"):
        assert key in resp
        assert resp[key]["min"] == resp[key]["max"]


def test_zone_factors_unknown_zone_returns_missing_zone(sidecar):
    resp = sidecar.request({"id": 8, "op": "zone_factors", "zone": "ZZZ-NOPE"})
    assert resp == {"id": 8, "status": "missing_zone"}
