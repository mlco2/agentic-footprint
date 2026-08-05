"""Tests for `python/af_sampler/__main__.py`, the codecarbon sampler sidecar.

Unlike `test_estimator.py` (which drives a real subprocess because the
estimator's whole contract *is* its stdio framing), the sampler's contract
is a state machine over time: energy windows, a watch-list, and a 60s
orphan tail. Driving that through a subprocess would mean sleeping through
real windows and depending on real hardware counters — slow, flaky, and it
would make `codecarbon` a hard requirement for CI.

So this file imports the module directly (same `sys.path` shim the repo
uses to reach sidecar sources) and drives the `Sampler` class
synchronously with three injected seams:

  * `clock`             — callable -> float epoch seconds (fake, advanced by hand)
  * `tracker_factory`   — callable -> codecarbon-shaped tracker (fake windows)
  * `process_inspector` — callable pid -> (cpu_seconds_total, rss_bytes, alive)

`af_sampler.__main__` imports nothing outside the stdlib at module level
(codecarbon and psutil are imported inside `main()`), so **these tests run
on a bare interpreter** — no managed venv needed. Only the `AF_E2E=1`
test at the bottom touches the real tracker.
"""
import json
import os
import queue
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from af_sampler.__main__ import (  # noqa: E402  (after sys.path shim)
    ORPHAN_TAIL_S,
    Sampler,
    detect_methods,
    is_shutdown,
    kwh_to_joules,
    parse_args,
    positive_interval,
    run_loop,
    sanitize_id,
)

MAIN = Path(__file__).resolve().parents[1] / "af_sampler" / "__main__.py"
SANITIZE_VECTORS = (
    Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "sanitize-vectors.json"
)


# --------------------------------------------------------------------------
# fakes
# --------------------------------------------------------------------------


class FakeEmissionsData:
    """Shape-compatible stand-in for codecarbon's `EmissionsData` delta:
    only the four energy fields (kWh) the sampler reads."""

    def __init__(self, cpu=0.0, gpu=0.0, ram=0.0, total=None):
        self.cpu_energy = cpu
        self.gpu_energy = gpu
        self.ram_energy = ram
        self.energy_consumed = total if total is not None else cpu + gpu + ram
        self.duration = 1.0


class FakeCPU:
    """Class name matters: `detect_methods` maps by codecarbon class name."""

    def __init__(self, mode="cpu_load"):
        self._mode = mode


class FakeRAM:
    pass


class FakeGPU:
    pass


class FakeAppleSiliconChip:
    def __init__(self, chip_part="CPU"):
        self.chip_part = chip_part


# `detect_methods` dispatches on `type(hw).__name__`, so the fakes have to
# carry codecarbon's names, not the test-local ones.
FakeCPU.__name__ = "CPU"
FakeRAM.__name__ = "RAM"
FakeGPU.__name__ = "GPU"
FakeAppleSiliconChip.__name__ = "AppleSiliconChip"


class FakeTracker:
    def __init__(self, hardware=None, windows=None):
        self._hardware = list(hardware or [])
        self._windows = list(windows or [])
        self.stopped = False

    def start(self):
        pass

    def start_task(self, task_name=None):
        pass

    def stop_task(self, task_name=None):
        if self._windows:
            return self._windows.pop(0)
        return FakeEmissionsData()

    def stop(self):
        self.stopped = True


class WedgingTracker(FakeTracker):
    """A tracker that loses a window and stays lost.

    Models codecarbon's real failure mode: when `stop_task()` raises (or
    returns nothing) it can leave `_active_task` set, after which every
    `start_task()` early-returns and every subsequent `stop_task()` comes
    back empty. A sampler that keeps using it would report 0 J forever, so
    the only way back to measuring is a *new* tracker.
    """

    def __init__(self, raise_on_fail=False, **kwargs):
        super().__init__(**kwargs)
        self.raise_on_fail = raise_on_fail
        self.wedged = False

    def stop_task(self, task_name=None):
        if self.wedged:
            return None
        self.wedged = True
        if self.raise_on_fail:
            raise RuntimeError("codecarbon: task measurement failed")
        return None


class FakeClock:
    def __init__(self, t=1_767_000_000.0):
        self.t = t

    def __call__(self):
        return self.t

    def advance(self, dt):
        self.t += dt


class FakeProcs:
    """pid -> (cpu_seconds_total, rss_bytes, alive); unknown pids are dead."""

    def __init__(self):
        self.table = {}

    def set(self, pid, cpu_seconds, rss=0, alive=True):
        self.table[pid] = (float(cpu_seconds), int(rss), bool(alive))

    def kill(self, pid):
        self.table[pid] = (0.0, 0, False)

    def __call__(self, pid):
        return self.table.get(pid, (0.0, 0, False))


class BurningProcs:
    """A process tree that burns exactly 1s of CPU per second of the fake
    clock, so a window's reported delta is its own duration."""

    def __init__(self, clock, pids):
        self.clock = clock
        self.t0 = clock()
        self.pids = set(pids)

    def __call__(self, pid):
        if pid not in self.pids:
            return (0.0, 0, False)
        return (self.clock() - self.t0, 4096, True)


TICK = object()  #: scripted step meaning "the window deadline expired"


class ScriptedOps:
    """`queue.Queue`-shaped stand-in for `run_loop`'s control queue.

    Each `get()` consumes one scripted step and advances the fake clock by
    `dt`, so windows have a real duration: a step of `TICK` raises
    `queue.Empty` (which is how `run_loop` learns the deadline expired),
    anything else is delivered as an op.
    """

    def __init__(self, clock, script, dt=5.0):
        self.clock = clock
        self.script = list(script)
        self.dt = dt
        self.timeouts = []

    def get(self, timeout=None):
        self.timeouts.append(timeout)
        assert self.script, "run_loop asked for more ops than the script has"
        step = self.script.pop(0)
        self.clock.advance(self.dt)
        if step is TICK:
            raise queue.Empty
        return step


def make_sampler(tmp_path, tracker=None, clock=None, procs=None, factory=None):
    """A `Sampler` wired to the three injected seams, plus the clock.

    Every case drives the clock, so it is returned; the other two seams ride
    on the sampler as `sampler.procs` (the `FakeProcs`/`BurningProcs` table)
    and `sampler.errors` (the log lines it produced), which the minority of
    cases that inspect them reach directly.
    """
    clock = clock or FakeClock()
    procs = procs or FakeProcs()
    errors = []
    sampler = Sampler(
        spool_path=tmp_path / "spool" / "codecarbon.sess-1.jsonl",
        session_id="sess-1",
        tracker_factory=factory if factory is not None else (lambda: tracker),
        process_inspector=procs,
        clock=clock,
        log=errors.append,
    )
    sampler.procs = procs
    sampler.errors = errors
    return sampler, clock


def counting_factory(trackers):
    """A `tracker_factory` handing out `trackers` in order (repeating the
    last one), recording every call so tests can assert the sampler really
    rebuilt its tracker."""
    made = []

    def factory():
        tracker = trackers[min(len(made), len(trackers) - 1)]
        made.append(tracker)
        return tracker

    return factory, made


def read_events(path):
    lines = Path(path).read_text(encoding="utf-8").splitlines()
    return [json.loads(line) for line in lines if line.strip()]


def events_of(path, type_):
    return [e for e in read_events(path) if e["type"] == type_]


def only_component(event, kind):
    matches = [c for c in event["payload"]["components"] if c["kind"] == kind]
    assert len(matches) == 1, f"expected exactly one {kind!r} component in {matches!r}"
    return matches[0]


# --------------------------------------------------------------------------
# unit conversion
# --------------------------------------------------------------------------


def test_kwh_to_joules_is_exact_for_the_reference_value():
    # 1 Wh = 3600 J, so 1e-3 kWh must be exactly 3600 J -- no float drift
    # allowance: this is the only place local energy units are converted.
    assert kwh_to_joules(1e-3) == 3600.0
    assert kwh_to_joules(0.0) == 0.0
    assert kwh_to_joules(1.0) == 3.6e6


# --------------------------------------------------------------------------
# method detection
# --------------------------------------------------------------------------


def test_method_mapping_rapl_tdp_model_and_nvml():
    methods = detect_methods([FakeCPU(mode="intel_rapl"), FakeRAM(), FakeGPU()])
    assert methods == {"cpu": "rapl", "dram": "tdp_model", "gpu": "nvml"}


def test_method_mapping_cpu_load_is_modeled_not_measured():
    methods = detect_methods([FakeCPU(mode="cpu_load"), FakeRAM()])
    assert methods == {"cpu": "tdp_model", "dram": "tdp_model"}


def test_method_mapping_apple_silicon_is_powermetrics_for_both_parts():
    methods = detect_methods(
        [
            FakeRAM(),
            FakeAppleSiliconChip(chip_part="CPU"),
            FakeAppleSiliconChip(chip_part="GPU"),
        ]
    )
    assert methods == {"cpu": "powermetrics", "dram": "tdp_model", "gpu": "powermetrics"}


def test_method_mapping_unknown_hardware_falls_back_without_raising():
    class Weird:
        pass

    assert detect_methods([Weird(), FakeCPU(mode="who-knows")]) == {"cpu": "other"}
    assert detect_methods(None) == {}


# --------------------------------------------------------------------------
# energy_sample
# --------------------------------------------------------------------------


def test_energy_sample_converts_each_component_and_tags_methods(tmp_path):
    tracker = FakeTracker(
        hardware=[FakeCPU(mode="intel_rapl"), FakeRAM()],
        windows=[FakeEmissionsData(cpu=1e-3, ram=5e-4, total=2e-3)],
    )
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    clock.advance(5.0)
    sampler.tick()

    (event,) = events_of(sampler.spool_path, "energy_sample")
    assert only_component(event, "cpu") == {
        "kind": "cpu",
        "energy_j": 3600.0,
        "method": "rapl",
    }
    assert only_component(event, "dram")["energy_j"] == 1800.0
    assert only_component(event, "total")["energy_j"] == 7200.0
    # No GPU hardware and no GPU energy -> no fabricated zero component.
    assert [c for c in event["payload"]["components"] if c["kind"] == "gpu"] == []


def test_total_component_carries_the_cpu_method(tmp_path):
    tracker = FakeTracker(
        hardware=[FakeCPU(mode="intel_rapl"), FakeRAM()],
        windows=[FakeEmissionsData(cpu=1e-3, ram=1e-3)],
    )
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    clock.advance(5.0)
    sampler.tick()

    (event,) = events_of(sampler.spool_path, "energy_sample")
    assert only_component(event, "total")["method"] == "rapl"
    assert only_component(event, "cpu")["method"] == "rapl"


def test_gpu_component_emitted_when_gpu_hardware_is_present_even_at_zero(tmp_path):
    tracker = FakeTracker(
        hardware=[FakeCPU(), FakeRAM(), FakeGPU()],
        windows=[FakeEmissionsData(cpu=1e-3, gpu=0.0)],
    )
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    clock.advance(5.0)
    sampler.tick()

    (event,) = events_of(sampler.spool_path, "energy_sample")
    assert only_component(event, "gpu") == {
        "kind": "gpu",
        "energy_j": 0.0,
        "method": "nvml",
    }


def test_window_bounds_track_the_injected_clock(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    clock.advance(5.0)
    sampler.tick()
    clock.advance(5.0)
    sampler.tick()

    first, second = events_of(sampler.spool_path, "energy_sample")
    assert first["payload"]["t_end"] == second["payload"]["t_start"]
    assert first["payload"]["t_start"] < first["payload"]["t_end"]
    assert first["payload"]["t_end"].endswith("Z")


# --------------------------------------------------------------------------
# a window with no reading is a gap, never a fabricated zero
# --------------------------------------------------------------------------


@pytest.mark.parametrize("raise_on_fail", [False, True], ids=["returns-none", "raises"])
def test_unreadable_window_emits_no_energy_sample_and_recovers(tmp_path, raise_on_fail):
    # 0 J components tagged with the real `method` values would be a
    # fabricated measurement: downstream, indistinguishable from a machine
    # that genuinely used nothing. Emit nothing, and get back to measuring.
    broken = WedgingTracker(
        hardware=[FakeCPU(mode="cpu_load"), FakeRAM()], raise_on_fail=raise_on_fail
    )
    healthy = FakeTracker(
        hardware=[FakeCPU(mode="intel_rapl"), FakeRAM()],
        windows=[FakeEmissionsData(cpu=1e-3)],
    )
    factory, made = counting_factory([broken, healthy])
    sampler, clock = make_sampler(tmp_path, factory=factory)
    sampler.start()

    clock.advance(5.0)
    sampler.tick()

    assert events_of(sampler.spool_path, "energy_sample") == []
    # ... but the window still happened, and what we *did* observe is kept.
    assert len(events_of(sampler.spool_path, "process_sample")) == 1
    assert any("energy" in message for message in sampler.errors), sampler.errors
    # The wedged tracker was replaced, so the next window can measure again.
    assert len(made) == 2

    clock.advance(5.0)
    sampler.tick()

    (event,) = events_of(sampler.spool_path, "energy_sample")
    assert only_component(event, "cpu")["energy_j"] == 3600.0
    # Methods are re-derived from the *new* tracker's hardware, not carried
    # over from the dead one.
    assert only_component(event, "cpu")["method"] == "rapl"
    assert len(made) == 2  # a healthy tracker is not churned


def test_energy_gap_does_not_stop_process_samples_or_crash(tmp_path):
    # Worst case: the factory itself is broken too, so there is no tracker
    # at all. The sidecar must keep reporting what it can still observe.
    broken = WedgingTracker(hardware=[FakeCPU(), FakeRAM()])
    calls = []

    def factory():
        calls.append(1)
        if len(calls) > 1:
            raise RuntimeError("no tracker for you")
        return broken

    sampler, clock = make_sampler(tmp_path, factory=factory)
    sampler.start()
    sampler.procs.set(5, cpu_seconds=0.0)
    sampler.apply_op({"op": "watch", "span_id": "s", "pids": [5]})

    for step in range(3):
        sampler.procs.set(5, cpu_seconds=float(step + 1))
        clock.advance(5.0)
        sampler.tick()
    sampler.finish()

    assert events_of(sampler.spool_path, "energy_sample") == []
    samples = events_of(sampler.spool_path, "process_sample")
    assert len(samples) == 4
    assert samples[0]["payload"]["processes"][0]["cpu_time_delta_ms"] == 1000


# --------------------------------------------------------------------------
# watch-list / process_sample
# --------------------------------------------------------------------------


def test_watch_takes_a_baseline_so_the_first_window_is_a_delta(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()

    # The tree has already burned 100s of CPU before we start watching it;
    # that history must NOT be attributed to the first window.
    sampler.procs.set(4242, cpu_seconds=100.0, rss=1024)
    sampler.apply_op({"op": "watch", "span_id": "span-a", "pids": [4242]})

    sampler.procs.set(4242, cpu_seconds=100.25, rss=2048)
    clock.advance(5.0)
    sampler.tick()

    (event,) = events_of(sampler.spool_path, "process_sample")
    assert event["payload"]["processes"] == [
        {"pid": 4242, "cpu_time_delta_ms": 250, "memory_rss_bytes": 2048}
    ]


def test_two_windows_report_independent_deltas(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    sampler.procs.set(7, cpu_seconds=10.0)
    sampler.apply_op({"op": "watch", "span_id": "s", "pids": [7]})

    sampler.procs.set(7, cpu_seconds=11.5)
    clock.advance(5.0)
    sampler.tick()

    sampler.procs.set(7, cpu_seconds=13.0)
    clock.advance(5.0)
    sampler.tick()

    first, second = events_of(sampler.spool_path, "process_sample")
    assert first["payload"]["processes"][0]["cpu_time_delta_ms"] == 1500
    assert second["payload"]["processes"][0]["cpu_time_delta_ms"] == 1500


def test_negative_delta_is_clamped_to_zero(tmp_path):
    # pid reuse / a child dropping out of the tree can make the summed
    # absolute CPU time go DOWN. The schema forbids negatives, and a
    # negative "consumption" is meaningless -- clamp, never emit it.
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    sampler.procs.set(9, cpu_seconds=50.0)
    sampler.apply_op({"op": "watch", "span_id": "s", "pids": [9]})

    sampler.procs.set(9, cpu_seconds=12.0)
    clock.advance(5.0)
    sampler.tick()

    (event,) = events_of(sampler.spool_path, "process_sample")
    assert event["payload"]["processes"][0]["cpu_time_delta_ms"] == 0


def test_dead_pids_are_omitted_rather_than_reported_as_zero(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    sampler.procs.set(11, cpu_seconds=1.0)
    sampler.apply_op({"op": "watch", "span_id": "s", "pids": [11]})

    sampler.procs.kill(11)
    clock.advance(5.0)
    sampler.tick()

    (event,) = events_of(sampler.spool_path, "process_sample")
    assert event["payload"]["processes"] == []


def test_process_sample_is_emitted_every_window_even_with_nothing_watched(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    clock.advance(5.0)
    sampler.tick()

    (event,) = events_of(sampler.spool_path, "process_sample")
    assert event["payload"]["processes"] == []


# --------------------------------------------------------------------------
# orphan tail
# --------------------------------------------------------------------------


def test_unwatch_keeps_sampling_the_tree_tagged_with_orphan_of(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    sampler.procs.set(321, cpu_seconds=0.0)
    sampler.apply_op({"op": "watch", "span_id": "span-x", "pids": [321]})

    sampler.procs.set(321, cpu_seconds=1.0)
    clock.advance(5.0)
    sampler.tick()
    sampler.apply_op({"op": "unwatch", "span_id": "span-x"})

    # The span closed but the process tree it spawned is still burning CPU:
    # that's exactly the orphan the join pipeline must be told about.
    sampler.procs.set(321, cpu_seconds=3.0, rss=99)
    clock.advance(5.0)
    sampler.tick()

    during, after = events_of(sampler.spool_path, "process_sample")
    assert "orphan_of" not in during["payload"]["processes"][0]
    assert after["payload"]["processes"] == [
        {
            "pid": 321,
            "cpu_time_delta_ms": 2000,
            "memory_rss_bytes": 99,
            "orphan_of": "span-x",
        }
    ]


def test_orphan_tail_expires_after_sixty_seconds(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    sampler.procs.set(55, cpu_seconds=0.0)
    sampler.apply_op({"op": "watch", "span_id": "span-y", "pids": [55]})
    sampler.apply_op({"op": "unwatch", "span_id": "span-y"})

    # Still inside the tail: sampled.
    sampler.procs.set(55, cpu_seconds=1.0)
    clock.advance(ORPHAN_TAIL_S - 1.0)
    sampler.tick()

    # Past the tail: dropped, even though the tree is still alive.
    sampler.procs.set(55, cpu_seconds=2.0)
    clock.advance(2.0)
    sampler.tick()

    inside, outside = events_of(sampler.spool_path, "process_sample")
    assert inside["payload"]["processes"][0]["orphan_of"] == "span-y"
    assert outside["payload"]["processes"] == []


def test_orphan_is_dropped_as_soon_as_its_tree_dies(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    sampler.procs.set(66, cpu_seconds=0.0)
    sampler.apply_op({"op": "watch", "span_id": "span-z", "pids": [66]})
    sampler.apply_op({"op": "unwatch", "span_id": "span-z"})

    sampler.procs.kill(66)
    clock.advance(1.0)
    sampler.tick()
    clock.advance(1.0)
    sampler.tick()

    for event in events_of(sampler.spool_path, "process_sample"):
        assert event["payload"]["processes"] == []
    assert sampler.watch_count == 0


# --------------------------------------------------------------------------
# control protocol robustness
# --------------------------------------------------------------------------


@pytest.mark.parametrize(
    "op",
    [
        "not-an-object",
        {},
        {"op": "watch"},
        {"op": "watch", "span_id": "s"},
        {"op": "watch", "span_id": "s", "pids": "nope"},
        {"op": "unwatch"},
        {"op": "unwatch", "span_id": "never-watched"},
        {"op": "teleport"},
        None,
    ],
)
def test_malformed_ops_are_logged_and_ignored_without_crashing(tmp_path, op):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()

    sampler.apply_op(op)

    # ... and the sampler still works afterwards.
    sampler.procs.set(1, cpu_seconds=0.0)
    sampler.apply_op({"op": "watch", "span_id": "ok", "pids": [1]})
    sampler.procs.set(1, cpu_seconds=0.5)
    clock.advance(5.0)
    sampler.tick()

    (event,) = events_of(sampler.spool_path, "process_sample")
    assert event["payload"]["processes"][0]["cpu_time_delta_ms"] == 500


# `shutdown` is `run_loop`'s to act on, not `apply_op`'s — see
# `test_is_shutdown_only_matches_the_shutdown_op` and the two
# `test_shutdown_*_window` cases below, which cover it where it lives.


def test_finish_flushes_the_open_window_and_stops_the_tracker(tmp_path):
    tracker = FakeTracker(
        hardware=[FakeCPU(), FakeRAM()],
        windows=[FakeEmissionsData(cpu=1e-3)],
    )
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    clock.advance(2.0)
    sampler.finish()

    assert tracker.stopped is True
    (event,) = events_of(sampler.spool_path, "energy_sample")
    assert only_component(event, "cpu")["energy_j"] == 3600.0


# --------------------------------------------------------------------------
# main loop: window boundaries vs. shutdown
# --------------------------------------------------------------------------


def test_is_shutdown_only_matches_the_shutdown_op():
    assert is_shutdown({"op": "shutdown"}) is True
    assert is_shutdown({"op": "watch", "span_id": "s", "pids": []}) is False
    assert is_shutdown("shutdown") is False
    assert is_shutdown(None) is False


def test_ping_is_acknowledged_without_waiting_for_a_window(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    responses = []

    ops = ScriptedOps(clock, [{"op": "ping", "id": 7}, {"op": "shutdown"}], dt=0.0)
    run_loop(sampler, ops, interval=5.0, clock=clock, respond=responses.append)
    sampler.finish()

    assert responses == [{"id": 7, "ok": True, "status": "ready"}]


def test_shared_sampler_splits_one_machine_window_across_sessions_without_duplication(tmp_path):
    tracker = FakeTracker(
        hardware=[FakeCPU(), FakeRAM()],
        windows=[FakeEmissionsData(cpu=1e-3)],
    )
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    sampler.procs.set(10, cpu_seconds=0.0)
    sampler.procs.set(20, cpu_seconds=0.0)
    sampler.apply_op({"op": "watch", "session_id": "sess-1", "span_id": "one", "pids": [10]})
    sampler.apply_op({"op": "watch", "session_id": "sess-2", "span_id": "two", "pids": [20]})

    sampler.procs.set(10, cpu_seconds=1.0)
    sampler.procs.set(20, cpu_seconds=3.0)
    clock.advance(5.0)
    sampler.tick()
    sampler.finish()

    first = events_of(tmp_path / "spool" / "codecarbon.sess-1.jsonl", "energy_sample")[0]
    second = events_of(tmp_path / "spool" / "codecarbon.sess-2.jsonl", "energy_sample")[0]
    first_process = events_of(
        tmp_path / "spool" / "codecarbon.sess-1.jsonl", "process_sample"
    )[0]
    second_process = events_of(
        tmp_path / "spool" / "codecarbon.sess-2.jsonl", "process_sample"
    )[0]
    first_cpu = only_component(first, "cpu")["energy_j"]
    second_cpu = only_component(second, "cpu")["energy_j"]
    assert first_cpu == pytest.approx(900.0)
    assert second_cpu == pytest.approx(2700.0)
    assert first_cpu + second_cpu == pytest.approx(3600.0)
    assert first_process["payload"]["processes"] == [
        {"pid": 10, "cpu_time_delta_ms": 1000, "memory_rss_bytes": 0}
    ]
    assert second_process["payload"]["processes"] == [
        {"pid": 20, "cpu_time_delta_ms": 3000, "memory_rss_bytes": 0}
    ]


def test_shutdown_between_ticks_closes_exactly_one_final_window(tmp_path):
    # The old shape (sleep a whole interval, tick, *then* drain the queue)
    # opened a fresh window and closed it microseconds later, emitting a
    # degenerate t_start == t_end sample -- a division hazard for any
    # downstream J/s or W figure.
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()

    ops = ScriptedOps(clock, [TICK, {"op": "shutdown"}], dt=5.0)
    run_loop(sampler, ops, interval=5.0, clock=clock)
    sampler.finish()

    energy = events_of(sampler.spool_path, "energy_sample")
    assert len(energy) == 2  # the ticked window + the one truncated by shutdown
    assert len(events_of(sampler.spool_path, "process_sample")) == 2
    for event in read_events(sampler.spool_path):
        assert event["payload"]["t_start"] != event["payload"]["t_end"]
    # Contiguous: the final window starts where the ticked one ended.
    assert energy[0]["payload"]["t_end"] == energy[1]["payload"]["t_start"]


def test_shutdown_before_any_tick_still_emits_one_window(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()

    ops = ScriptedOps(clock, [{"op": "shutdown"}], dt=3.0)
    run_loop(sampler, ops, interval=5.0, clock=clock)
    sampler.finish()

    # Shutdown arrived mid-window: no waiting out the rest of the interval,
    # and the truncated window is still real measured energy.
    (event,) = events_of(sampler.spool_path, "energy_sample")
    assert event["payload"]["t_start"] != event["payload"]["t_end"]
    assert tracker.stopped is True


def test_run_loop_waits_only_until_the_next_window_boundary(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()

    ops = ScriptedOps(clock, [TICK, TICK, {"op": "shutdown"}], dt=5.0)
    run_loop(sampler, ops, interval=5.0, clock=clock)

    # Every wait is bounded by the time left in the current window, so a
    # shutdown is acted on the moment it arrives -- never up to an interval
    # later -- while windows still close on schedule.
    assert ops.timeouts[0] == pytest.approx(5.0)
    assert all(t is not None and t >= 0 for t in ops.timeouts)


def test_ops_arriving_mid_window_take_effect_from_the_next_window(tmp_path):
    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    clock = FakeClock()
    procs = BurningProcs(clock, pids=[4242])
    sampler, _ = make_sampler(tmp_path, tracker, clock=clock, procs=procs)
    sampler.start()

    watch = {"op": "watch", "span_id": "span-a", "pids": [4242]}
    ops = ScriptedOps(clock, [watch, TICK, TICK, {"op": "shutdown"}], dt=5.0)
    run_loop(sampler, ops, interval=5.0, clock=clock)
    sampler.finish()

    first, second, third = events_of(sampler.spool_path, "process_sample")
    # The watch arrived while the first window was in flight: that window is
    # NOT retroactively re-attributed.
    assert first["payload"]["processes"] == []
    # From the next window on the tree is sampled, and the baseline was taken
    # when the op was applied -- so the delta is the window, not the history.
    assert second["payload"]["processes"][0]["cpu_time_delta_ms"] == 5000
    assert third["payload"]["processes"][0]["pid"] == 4242


# --------------------------------------------------------------------------
# session id -> filename, against the shared conformance vectors
# --------------------------------------------------------------------------


def test_sanitize_id_matches_the_shared_conformance_vectors():
    # `sanitize_id` exists three times over — here, in
    # `collectors/claude-code/af-hook.sh` and in
    # `crates/af-otlp/src/sanitize.rs` — because the three collectors that
    # build spool filenames are written in three languages. Two that
    # disagree about what a session id may contain produce two filenames
    # for one session, and the join then silently sees two sessions. The
    # vectors are the one thing all three CAN share; the other two suites
    # (test_hooks.sh, crates/af-otlp/tests/sanitize_vectors.rs) read this
    # same file.
    vectors = json.loads(SANITIZE_VECTORS.read_text(encoding="utf-8"))
    assert len(vectors) >= 10, "the vector file lost its contents"
    for vector in vectors:
        assert sanitize_id(vector["raw"]) == vector["sanitized"], vector["note"]
        # Idempotent, so a filename can be re-derived anywhere in the
        # pipeline without drifting.
        assert sanitize_id(vector["sanitized"]) == vector["sanitized"]


# --------------------------------------------------------------------------
# CLI surface
# --------------------------------------------------------------------------


def test_interval_must_be_a_positive_number():
    for bad in ("0", "-1", "-0.5", "nan", "abc", "inf"):
        with pytest.raises(Exception) as excinfo:
            positive_interval(bad)
        assert "positive" in str(excinfo.value) or "number" in str(excinfo.value)
    assert positive_interval("2.5") == 2.5


@pytest.mark.parametrize("bad", ["0", "-3"])
def test_nonpositive_interval_is_a_cli_error_not_a_silent_default(bad):
    # Silently coercing to the 5s default would hide a misconfigured
    # supervisor behind windows nobody asked for.
    with pytest.raises(SystemExit) as excinfo:
        parse_args(["--session", "s", "--interval", bad])
    assert excinfo.value.code == 2


def test_interval_defaults_and_accepts_positive_values():
    assert parse_args(["--session", "s"]).interval == 5.0
    assert parse_args(["--session", "s", "--interval", "0.5"]).interval == 0.5


# --------------------------------------------------------------------------
# Contract #1 envelope
# --------------------------------------------------------------------------


def test_every_emitted_line_is_a_valid_contract_1_envelope(tmp_path):
    tracker = FakeTracker(
        hardware=[FakeCPU(), FakeRAM(), FakeGPU()],
        windows=[FakeEmissionsData(cpu=1e-3, gpu=2e-4, ram=3e-4)],
    )
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    sampler.procs.set(3, cpu_seconds=0.0)
    sampler.apply_op({"op": "watch", "span_id": "s", "pids": [3]})
    sampler.procs.set(3, cpu_seconds=1.0)
    clock.advance(5.0)
    sampler.tick()
    sampler.finish()

    events = read_events(sampler.spool_path)
    assert len(events) == 4  # two windows x (energy_sample + process_sample)

    seen_ids = set()
    for event in events:
        for key in (
            "schema_version",
            "event_id",
            "type",
            "ts",
            "collector",
            "session_id",
            "payload",
        ):
            assert key in event, f"missing {key!r} in {event!r}"
        assert event["schema_version"] == "0.1.0"
        assert len(event["event_id"]) >= 16
        assert event["event_id"] not in seen_ids
        seen_ids.add(event["event_id"])
        assert event["type"] in ("energy_sample", "process_sample")
        assert event["ts"].endswith("Z")
        assert event["collector"] == {"name": "codecarbon", "version": "0.1.0"}
        assert event["session_id"] == "sess-1"
        assert "t_start" in event["payload"] and "t_end" in event["payload"]

    for event in events_of(sampler.spool_path, "energy_sample"):
        assert len(event["payload"]["components"]) >= 1
        for comp in event["payload"]["components"]:
            assert comp["kind"] in ("cpu", "dram", "gpu", "total", "other")
            assert comp["method"] in (
                "rapl",
                "powermetrics",
                "nvml",
                "tdp_model",
                "other",
            )
            assert comp["energy_j"] >= 0


def test_spool_is_appended_to_never_truncated(tmp_path):
    # Collectors never delete and never rewrite: a restarted sidecar must
    # not clobber the events an earlier run already spooled.
    spool = tmp_path / "spool" / "codecarbon.sess-1.jsonl"
    spool.parent.mkdir(parents=True)
    spool.write_text('{"pre":"existing"}\n', encoding="utf-8")

    tracker = FakeTracker(hardware=[FakeCPU(), FakeRAM()])
    sampler, clock = make_sampler(tmp_path, tracker)
    sampler.start()
    clock.advance(5.0)
    sampler.finish()

    lines = spool.read_text(encoding="utf-8").splitlines()
    assert lines[0] == '{"pre":"existing"}'
    assert len(lines) == 3
    assert all(json.loads(line) for line in lines)


# --------------------------------------------------------------------------
# gated real-hardware run (AF_E2E=1, needs the managed venv's codecarbon)
# --------------------------------------------------------------------------


@pytest.mark.skipif(
    os.environ.get("AF_E2E") != "1",
    reason="real codecarbon sampler run; set AF_E2E=1 (needs the managed venv)",
)
def test_real_codecarbon_run_emits_valid_energy_samples(tmp_path):
    proc = subprocess.Popen(
        [
            sys.executable,
            str(MAIN),
            "--state-dir",
            str(tmp_path),
            "--session",
            "e2e-session",
            "--interval",
            "1",
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    spool = tmp_path / "spool" / "codecarbon.e2e-session.jsonl"
    try:
        # Wait for 3 real 1s windows -- codecarbon's import + hardware probe
        # dominates startup, so poll the spool rather than guessing a sleep.
        deadline = time.time() + 120
        while time.time() < deadline:
            if spool.exists() and len(events_of(spool, "energy_sample")) >= 3:
                break
            time.sleep(0.5)
        stdout, stderr = proc.communicate(input='{"op":"shutdown"}\n', timeout=60)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise

    assert spool.exists(), f"no spool file (stderr: {stderr})"
    energy = events_of(spool, "energy_sample")
    assert len(energy) >= 3, f"only {len(energy)} energy samples (stderr: {stderr})"

    for event in energy:
        assert event["schema_version"] == "0.1.0"
        assert event["collector"]["name"] == "codecarbon"
        assert event["payload"]["components"], "components must never be empty"
        for comp in event["payload"]["components"]:
            assert comp["kind"] in ("cpu", "dram", "gpu", "total", "other")
            assert comp["method"] in (
                "rapl",
                "powermetrics",
                "nvml",
                "tdp_model",
                "other",
            )
            assert comp["energy_j"] >= 0.0
    assert proc.returncode == 0, f"exit {proc.returncode}: {stderr}"
