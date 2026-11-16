"""Contract between the agents and the pinned estimator: every model id the
supported agents emit must be known to the ecologits version in
`python/manifest.toml`.

An unknown model is not an error anywhere in the pipeline — the call is
stored as `unknown_model` and carries no inference impact — so without this
test a new default model silently zeroes most of a user's footprint (it did:
`claude-opus-5-5` was unknown to ecologits 0.11.1 for weeks).

Set `AF_REQUIRE_ECOLOGITS=1` (CI does) to fail instead of skipping when
ecologits is not importable.
"""
import json
import os
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

if os.environ.get("AF_REQUIRE_ECOLOGITS") == "1":
    import ecologits
else:
    ecologits = pytest.importorskip("ecologits")

ROOT = Path(__file__).resolve().parents[2]
MAIN = ROOT / "python" / "af_estimator" / "__main__.py"
MANIFEST = tomllib.loads((ROOT / "python" / "manifest.toml").read_text())
AGENT_MODELS = json.loads((ROOT / "tests" / "fixtures" / "models" / "agent-models.json").read_text())


def _recorded_llm_calls():
    """`(provider, model)` of every llm_call in the real recorded sessions."""
    seen = set()
    for path in sorted((ROOT / "tests" / "fixtures" / "acceptance").glob("*.jsonl")):
        for line in path.read_text().splitlines():
            event = json.loads(line)
            if event.get("type") == "llm_call":
                payload = event["payload"]
                seen.add((payload["provider"], payload["model_id_requested"]))
    return sorted(seen)


def _estimate(provider: str, model: str) -> dict:
    request = {
        "op": "estimate",
        "id": 1,
        "provider": provider,
        "model_name": model,
        "output_token_count": 100,
        "electricity_mix_zone": "WOR",
    }
    out = subprocess.run(
        [sys.executable, str(MAIN)],
        input=json.dumps(request) + "\n",
        capture_output=True,
        text=True,
        timeout=120,
        check=True,
    )
    return json.loads(out.stdout.splitlines()[0])


def test_installed_ecologits_is_the_manifest_pin():
    # Coverage is only meaningful against the version `af` actually ships.
    assert ecologits.__version__ == MANIFEST["packages"]["ecologits"]


def test_runtime_dependency_group_mirrors_the_manifest():
    pyproject = tomllib.loads((ROOT / "pyproject.toml").read_text())
    runtime = sorted(pyproject["dependency-groups"]["runtime"])
    expected = sorted(
        f"{name}{spec}" if spec[0] in "<>=!~" else f"{name}=={spec}"
        for name, spec in MANIFEST["packages"].items()
    ) + sorted(
        f"{name}>={floor}" for name, floor in MANIFEST.get("minimums", {}).items()
    )
    expected.sort()
    assert runtime == expected


@pytest.mark.parametrize(
    "entry",
    AGENT_MODELS["models"],
    ids=lambda entry: f"{entry['agent']}:{entry['model']}",
)
def test_agent_model_is_known_to_the_pinned_estimator(entry):
    response = _estimate(entry["provider"], entry["model"])
    assert response["status"] == "ok", response


@pytest.mark.parametrize("call", _recorded_llm_calls(), ids=lambda call: f"{call[0]}:{call[1]}")
def test_recorded_session_model_is_known_to_the_pinned_estimator(call):
    response = _estimate(*call)
    assert response["status"] == "ok", response
