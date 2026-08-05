#!/usr/bin/env python3
"""`af_sampler` sidecar: local hardware energy + watched process trees.

    python af_sampler/__main__.py --state-dir DIR --session SID [--interval 5]

Appends Contract #1 events (one JSON object per line, one `write()` each)
to `DIR/spool/codecarbon.<session>.jsonl`:

  * `energy_sample`  — one per interval, from a `codecarbon`
    `OfflineEmissionsTracker` window (`start_task()`/`stop_task()`), with
    `cpu`/`dram`/`gpu`/`total` components in joules.
  * `process_sample` — one per interval, from `psutil`: for every watched
    root pid, the CPU-time delta of its whole process tree over the window.

Control stream on **stdin**, one JSON object per line (no responses — this
is a collector, not a request/response sidecar like `af_estimator`):

    {"op":"watch","span_id":"...","pids":[123, 456]}
    {"op":"unwatch","span_id":"..."}
    {"op":"shutdown"}

`watch` samples its pids immediately to take a CPU-time baseline (so the
first window reports a delta, never the tree's whole history). Ops are
drained *after* each window closes, so they take effect from the next
window on — a window is never retroactively re-attributed. `unwatch` does
not stop sampling: the tree enters a 60s **orphan tail** during which its
entries carry an extra `"orphan_of": "<span_id>"` key (the schema allows
additional properties on process items), so background work a tool left
running is still visible to the join pipeline instead of vanishing with
the span that started it. The entry is dropped as soon as the tail expires
or the tree dies.

Failure honesty: a pid that is gone is omitted, not reported as zero; a
CPU-time delta that comes out negative (pid reuse, a child leaving the
tree) is clamped to 0 rather than emitted. A window whose `stop_task()`
raised or returned nothing emits **no `energy_sample` at all** (a 0 J
sample carrying real `method` tags would be a fabricated measurement) and
the tracker is rebuilt from the injected factory, so the next window
measures again instead of the sidecar wedging at zero forever. No
malformed stdin line can crash the loop.

The `codecarbon`/`psutil` imports live inside `main()`: `codecarbon` pulls
in pandas and costs ~1s, and the `Sampler` state machine below is
deliberately dependency-free so `python/tests/test_sampler.py` can drive it
on a bare interpreter with fakes.
"""
from __future__ import annotations

import argparse
import json
import os
import queue
import re
import sys
import threading
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

SCHEMA_VERSION = "0.1.0"
COLLECTOR_NAME = "codecarbon"
COLLECTOR_VERSION = "0.1.0"

#: 1 kWh = 3.6e6 J, exactly.
KWH_TO_J = 3.6e6

#: How long a process tree keeps being sampled after its span is unwatched.
ORPHAN_TAIL_S = 60.0

DEFAULT_INTERVAL_S = 5.0
DEFAULT_STATE_DIR = "~/.local/state/agentic-footprint"

#: codecarbon's `CPU._mode` -> Contract #1 `method`. `cpu_load`/`constant`
#: are TDP-derived models, `intel_rapl` reads real energy counters.
#: `intel_power_gadget` *is* measured, but the schema enum has no name for
#: it, and inventing one would break the measured/modeled distinction the
#: enum exists to preserve -- so it degrades to `other`.
_CPU_MODE_METHOD = {
    "cpu_load": "tdp_model",
    "constant": "tdp_model",
    "intel_rapl": "rapl",
    "apple_powermetrics": "powermetrics",
    "intel_power_gadget": "other",
}


def kwh_to_joules(kwh) -> float:
    """codecarbon reports energy in kWh; Contract #1 wants joules."""
    return float(kwh) * KWH_TO_J


def detect_methods(hardware) -> dict:
    """Map a codecarbon tracker's resolved `_hardware` list to a
    `{component_kind: method}` dict.

    Dispatch is on the hardware object's *class name* plus, for `CPU`, its
    `_mode` attribute — codecarbon picks the class/mode combination at
    `start()` time based on what the machine actually offers (RAPL files,
    `powermetrics`, NVML, or nothing but a TDP number), which is exactly
    the measured-vs-modeled distinction the schema's `method` enum
    encodes. Anything unrecognized degrades to `other` rather than
    guessing; a kind that is absent from the returned dict simply wasn't
    detected.
    """
    methods: dict = {}
    for hw in hardware or []:
        try:
            name = type(hw).__name__
            if name == "CPU":
                methods["cpu"] = _CPU_MODE_METHOD.get(getattr(hw, "_mode", None), "other")
            elif name == "AppleSiliconChip":
                part = str(getattr(hw, "chip_part", "CPU")).upper()
                methods["gpu" if part == "GPU" else "cpu"] = "powermetrics"
            elif name == "GPU":
                methods["gpu"] = "nvml"
            elif name == "RAM":
                methods["dram"] = "tdp_model"
        except Exception:  # noqa: BLE001 - detection must never be fatal
            continue
    return methods


def _iso(epoch_seconds: float) -> str:
    """RFC 3339 UTC with a trailing `Z` (what the schema's `date-time`
    format and the rest of the collectors emit)."""
    return (
        datetime.fromtimestamp(epoch_seconds, timezone.utc)
        .isoformat(timespec="milliseconds")
        .replace("+00:00", "Z")
    )


def _energy_field(data, name: str) -> float:
    """Read one kWh field off a codecarbon `EmissionsData`, tolerating a
    missing/None/non-numeric value and clamping negatives (the schema
    forbids them, and a negative delta is a codecarbon accounting glitch,
    not information)."""
    value = getattr(data, name, None)
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        return 0.0
    return max(0.0, float(value))


class _Watch:
    """One watched span: its root pids and their last observed absolute
    CPU time. `orphan_since` is None while the span is open, and the
    unwatch timestamp once it has entered the orphan tail."""

    __slots__ = ("session_id", "span_id", "pids", "last_cpu", "orphan_since")

    def __init__(self, session_id: str, span_id: str, pids) -> None:
        self.session_id = session_id
        self.span_id = span_id
        self.pids = list(pids)
        self.last_cpu: dict = {}
        self.orphan_since = None


class Sampler:
    """The sidecar's whole state machine, with every environmental
    dependency injected so tests can drive windows synchronously:

    * `clock`             — callable -> float epoch seconds
    * `tracker_factory`   — callable -> codecarbon-shaped tracker
      (`start`, `start_task`, `stop_task`, `stop`, `_hardware`)
    * `process_inspector` — callable pid -> (cpu_seconds_total, rss_bytes,
      alive), aggregated over the pid's whole process tree
    * `log`               — callable(str) for non-fatal complaints
    """

    def __init__(
        self,
        *,
        spool_path,
        session_id: str,
        tracker_factory,
        process_inspector,
        clock=time.time,
        log=None,
    ) -> None:
        self.spool_path = Path(spool_path)
        self.spool_path.parent.mkdir(parents=True, exist_ok=True)
        # O_APPEND + a single os.write() of the whole encoded line is what
        # makes the collector contract's "one atomic write per event" true
        # unconditionally: a buffered text handle can split a line across
        # two syscalls (encoder chunking, buffer boundaries), and two
        # collectors appending to one file would then interleave halves.
        self._session_id = session_id
        self._outputs = {}
        self._add_session(session_id, self.spool_path)
        self._tracker_factory = tracker_factory
        self._inspect = process_inspector
        self._clock = clock
        self._log = log if log is not None else _stderr_log

        self._tracker = None
        self._methods: dict = {}
        self._watches: dict = {}
        self._window_index = 0
        self._window_name = None
        self._window_start = None
        #: True only while a window is open on a tracker that accepted
        #: `start_task()` — i.e. only while an energy reading is possible.
        self._window_measured = False

    # -- lifecycle ---------------------------------------------------------

    def start(self) -> None:
        self._build_tracker()
        self._open_window()

    def tick(self) -> None:
        """Close the current window, emit its two events, open the next."""
        self._close_window()
        self._open_window()

    def finish(self) -> None:
        """Flush the partially-elapsed window and shut the tracker down.

        The final window is emitted even when it is very short: a truncated
        interval is real measured energy, and dropping it would silently
        under-report the tail of every session.
        """
        self._close_window(final=True)
        try:
            if self._tracker is not None:
                self._tracker.stop()
        except Exception as exc:  # noqa: BLE001 - never fail on the way out
            self._log(f"af_sampler: tracker.stop() failed: {exc}")
        self._tracker = None
        self.close()

    def close(self) -> None:
        for _path, fd in self._outputs.values():
            try:
                os.close(fd)
            except OSError:
                pass
        self._outputs.clear()

    def _add_session(self, session_id: str, spool_path=None) -> None:
        if session_id in self._outputs:
            return
        path = Path(spool_path) if spool_path is not None else self.spool_path.parent / (
            f"codecarbon.{sanitize_id(session_id)}.jsonl"
        )
        path.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(str(path), os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
        self._outputs[session_id] = (path, fd)

    def _remove_session(self, session_id: str) -> None:
        output = self._outputs.pop(session_id, None)
        if output is not None:
            try:
                os.close(output[1])
            except OSError:
                pass
        for key in [key for key, watch in self._watches.items() if watch.session_id == session_id]:
            del self._watches[key]

    def _build_tracker(self) -> bool:
        """(Re)build the tracker from the injected factory and re-derive the
        method tags from *its* resolved hardware. Returns False (and leaves
        `self._tracker` None) if the factory or `start()` failed — the next
        window retries, rather than the sidecar silently never measuring
        again."""
        try:
            tracker = self._tracker_factory()
            tracker.start()
        except Exception as exc:  # noqa: BLE001 - retried next window
            self._log(f"af_sampler: could not start a tracker: {exc}")
            self._tracker = None
            self._methods = {}
            return False
        self._tracker = tracker
        # Methods are a property of the tracker's resolved hardware, so they
        # are re-derived here rather than carried over from the dead one.
        self._methods = detect_methods(getattr(tracker, "_hardware", []))
        return True

    def _replace_tracker(self) -> None:
        """Retire a tracker that failed mid-window and build a fresh one.

        codecarbon's `stop_task()` can leave `_active_task` set when it
        raises; the next `start_task()` then early-returns, so *every*
        later window would come back empty. Replacing the tracker outright
        is the only reliable way back to measuring."""
        old, self._tracker = self._tracker, None
        if old is not None:
            try:
                old.stop()
            except Exception as exc:  # noqa: BLE001 - it is already broken
                self._log(f"af_sampler: retiring a failed tracker: {exc}")
        self._build_tracker()

    @property
    def watch_count(self) -> int:
        return len(self._watches)

    # -- control ops -------------------------------------------------------

    def apply_op(self, op) -> None:
        """Apply one control op.

        `shutdown` never arrives here: `run_loop` recognises it the moment
        it comes off the control queue and returns, precisely so the
        in-flight window is not re-opened first. Everything that *is*
        queued for the next window boundary is one of the ops below.

        Never raises: the control stream is external input, and one bad op
        must not take the collector down mid-session.
        """
        try:
            if not isinstance(op, dict):
                self._log(f"af_sampler: ignoring non-object op: {op!r}")
                return
            name = op.get("op")
            if name == "watch":
                self._op_watch(op)
            elif name == "unwatch":
                self._op_unwatch(op)
            elif name == "remove_session":
                session_id = op.get("session_id")
                if isinstance(session_id, str) and session_id:
                    self._remove_session(session_id)
            else:
                self._log(f"af_sampler: ignoring unknown op: {name!r}")
        except Exception as exc:  # noqa: BLE001 - see docstring
            self._log(f"af_sampler: op failed ({op!r}): {exc}")

    def _op_watch(self, op: dict) -> None:
        session_id = op.get("session_id", self._session_id)
        span_id = op.get("span_id")
        pids = op.get("pids")
        if not isinstance(session_id, str) or not session_id:
            self._log(f"af_sampler: watch without a usable session_id: {op!r}")
            return
        if not isinstance(span_id, str) or not span_id:
            self._log(f"af_sampler: watch without a usable span_id: {op!r}")
            return
        if not isinstance(pids, list) or not all(isinstance(p, int) for p in pids):
            self._log(f"af_sampler: watch without a usable pid list: {op!r}")
            return

        self._add_session(session_id)
        watch = _Watch(session_id, span_id, pids)
        # Baseline immediately, not at the next window boundary: the tree
        # may already have a long CPU history (a re-used shell, the agent
        # process itself), and none of it belongs to this span.
        for pid in watch.pids:
            cpu_seconds, _rss, _alive = self._inspect(pid)
            watch.last_cpu[pid] = float(cpu_seconds)
        self._watches[(session_id, span_id)] = watch

    def _op_unwatch(self, op: dict) -> None:
        session_id = op.get("session_id", self._session_id)
        span_id = op.get("span_id")
        watch = self._watches.get((session_id, span_id))
        if watch is None:
            self._log(f"af_sampler: unwatch for an unknown span: {span_id!r}")
            return
        watch.orphan_since = self._clock()

    # -- windows -----------------------------------------------------------

    def _open_window(self) -> None:
        self._window_index += 1
        self._window_name = f"af-window-{self._window_index}"
        self._window_start = self._clock()
        self._window_measured = False
        if self._tracker is None:
            return
        try:
            self._tracker.start_task(self._window_name)
        except Exception as exc:  # noqa: BLE001 - close() replaces it
            self._log(f"af_sampler: start_task failed: {exc}")
            return
        self._window_measured = True

    def _close_window(self, final: bool = False) -> None:
        """Close the open window and emit its events.

        `process_sample` is emitted for every window unconditionally.
        `energy_sample` is emitted **only** when the tracker actually
        returned a reading: a window whose `stop_task()` raised or returned
        None has no measurement, and emitting 0 J components tagged with
        the real methods would be a fabricated measurement — precisely
        indistinguishable, downstream, from a genuinely idle machine.
        """
        if self._window_name is None:
            return
        name, t_start = self._window_name, self._window_start
        self._window_name = None

        data = None
        problem = None
        if not self._window_measured:
            problem = "window never opened on a live tracker"
        else:
            try:
                data = self._tracker.stop_task(name)
            except Exception as exc:  # noqa: BLE001
                problem = f"stop_task failed: {exc}"
            else:
                if data is None:
                    problem = "stop_task returned no data"
        self._window_measured = False

        t_end = self._clock()
        processes_by_session, cpu_by_session = self._collect_process_samples()
        if problem is None:
            self._emit_energy_samples(data, t_start, t_end, cpu_by_session)
        else:
            self._log(
                f"af_sampler: no energy reading for {name} ({problem}); "
                "emitting no energy_sample for this window"
            )
        self._emit_process_samples(t_start, t_end, processes_by_session)

        # Rebuild for the *next* window. Not on the final close: there is no
        # next window, and starting a tracker only to stop it costs a full
        # hardware probe on the shutdown path.
        if problem is not None and not final:
            self._replace_tracker()

    def _energy_components(self, data) -> list:
        cpu_method = self._methods.get("cpu", "other")
        gpu_j = kwh_to_joules(_energy_field(data, "gpu_energy"))
        components = [
            {
                "kind": "cpu",
                "energy_j": kwh_to_joules(_energy_field(data, "cpu_energy")),
                "method": cpu_method,
            },
            {
                "kind": "dram",
                "energy_j": kwh_to_joules(_energy_field(data, "ram_energy")),
                "method": self._methods.get("dram", "other"),
            },
        ]
        # A zero `gpu` component on a machine with no GPU would read as "we
        # measured the GPU and it used nothing"; omit it instead. A nonzero
        # value is always reported, even if hardware detection missed it.
        if "gpu" in self._methods or gpu_j > 0:
            components.append(
                {
                    "kind": "gpu",
                    "energy_j": gpu_j,
                    "method": self._methods.get("gpu", "other"),
                }
            )
        # `total` is codecarbon's `energy_consumed` (cpu+gpu+ram) and so has
        # no single method of its own. It inherits the CPU component's,
        # since CPU dominates the total on every machine this PoC targets.
        components.append(
            {
                "kind": "total",
                "energy_j": kwh_to_joules(_energy_field(data, "energy_consumed")),
                "method": cpu_method,
            }
        )
        return components

    def _emit_energy_samples(self, data, t_start: float, t_end: float, cpu_by_session: dict) -> None:
        sessions = list(self._outputs)
        if not sessions:
            return
        total_cpu = sum(cpu_by_session.get(session_id, 0) for session_id in sessions)
        shares = {
            session_id: (
                cpu_by_session.get(session_id, 0) / total_cpu
                if total_cpu > 0
                else 1.0 / len(sessions)
            )
            for session_id in sessions
        }
        components = self._energy_components(data)
        for session_id, share in shares.items():
            sliced = [{**component, "energy_j": component["energy_j"] * share} for component in components]
            self._emit(
                session_id,
                "energy_sample",
                {"t_start": _iso(t_start), "t_end": _iso(t_end), "components": sliced},
            )

    def _collect_process_samples(self):
        now = self._clock()
        processes_by_session = {session_id: [] for session_id in self._outputs}
        cpu_by_session = {session_id: 0 for session_id in self._outputs}
        for key in list(self._watches):
            watch = self._watches[key]
            if watch.orphan_since is not None and now - watch.orphan_since >= ORPHAN_TAIL_S:
                del self._watches[key]
                continue

            entries, any_alive = self._sample_watch(watch)
            processes_by_session.setdefault(watch.session_id, []).extend(entries)
            cpu_by_session[watch.session_id] = cpu_by_session.get(watch.session_id, 0) + sum(
                entry["cpu_time_delta_ms"] for entry in entries
            )
            # An orphan whose tree is gone has nothing left to attribute.
            if watch.orphan_since is not None and not any_alive:
                del self._watches[key]
        return processes_by_session, cpu_by_session

    def _emit_process_samples(self, t_start: float, t_end: float, processes_by_session: dict) -> None:
        for session_id in list(self._outputs):
            self._emit(
                session_id,
                "process_sample",
                {
                    "t_start": _iso(t_start),
                    "t_end": _iso(t_end),
                    "processes": processes_by_session.get(session_id, []),
                },
            )

    def _sample_watch(self, watch: _Watch):
        entries = []
        any_alive = False
        for pid in watch.pids:
            try:
                cpu_seconds, rss_bytes, alive = self._inspect(pid)
            except Exception as exc:  # noqa: BLE001
                self._log(f"af_sampler: inspecting pid {pid} failed: {exc}")
                continue
            if not alive:
                # The tree is gone. Whatever it burned between the last
                # window and its death is unobservable, so report nothing
                # rather than a fabricated zero.
                continue
            any_alive = True
            previous = watch.last_cpu.get(pid, float(cpu_seconds))
            watch.last_cpu[pid] = float(cpu_seconds)
            delta_ms = int(round(max(0.0, float(cpu_seconds) - previous) * 1000))
            entry = {
                "pid": pid,
                "cpu_time_delta_ms": delta_ms,
                "memory_rss_bytes": int(rss_bytes),
            }
            if watch.orphan_since is not None:
                entry["orphan_of"] = watch.span_id
            entries.append(entry)
        return entries, any_alive

    # -- spool -------------------------------------------------------------

    def _emit(self, session_id: str, event_type: str, payload: dict) -> None:
        event = {
            "schema_version": SCHEMA_VERSION,
            "event_id": str(uuid.uuid4()),
            "type": event_type,
            "ts": _iso(self._clock()),
            "collector": {"name": COLLECTOR_NAME, "version": COLLECTOR_VERSION},
            "session_id": session_id,
            "payload": payload,
        }
        # Exactly one `write(2)` per event, per the collector contract in
        # The whole line is encoded
        # first and handed to a single `os.write()` on an O_APPEND fd, so
        # the kernel places it at the current end of file atomically and a
        # concurrent collector can never interleave halves of two lines.
        # No buffering layer sits in between, so there is nothing to flush.
        line = (json.dumps(event, separators=(",", ":")) + "\n").encode("utf-8")
        output = self._outputs.get(session_id)
        if output is None:
            return
        os.write(output[1], line)


def _stderr_log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def sanitize_id(value: str) -> str:
    """Session ids reach the filesystem (`codecarbon.<session>.jsonl`), so
    strip everything outside the allow-list — same rule as the Claude Code
    hook shim's `sanitize_id()`, which makes path traversal structurally
    impossible (no `/` can survive)."""
    cleaned = re.sub(r"[^A-Za-z0-9._-]", "", value or "")
    if not cleaned or cleaned.startswith("."):
        cleaned = "x" + cleaned
    return cleaned


def _make_psutil_inspector():
    """pid -> (cpu_seconds_total, rss_bytes, alive) summed over the pid's
    whole process tree (the root plus every descendant)."""
    import psutil

    def inspect(pid: int):
        try:
            root = psutil.Process(pid)
            members = [root] + root.children(recursive=True)
        except (psutil.Error, OSError):
            return (0.0, 0, False)

        cpu_seconds = 0.0
        rss_bytes = 0
        alive = False
        for member in members:
            try:
                times = member.cpu_times()
                cpu_seconds += times.user + times.system
                rss_bytes += member.memory_info().rss
                alive = True
            except (psutil.Error, OSError):
                # A child that exited between listing and reading it: skip.
                continue
        return (cpu_seconds, rss_bytes, alive)

    return inspect


def _make_tracker_factory(interval: float, country_iso_code: str):
    from codecarbon import OfflineEmissionsTracker

    def factory():
        # We want ENERGY only: estimation (CO2, water, electricity mix)
        # is the control plane's job, so no output handler, no file, and
        # the required `country_iso_code` is inert here.
        return OfflineEmissionsTracker(
            country_iso_code=country_iso_code,
            measure_power_secs=interval,
            tracking_mode="machine",
            output_handlers=[],
            save_to_file=False,
            log_level="error",
            allow_multiple_runs=True,
        )

    return factory


def _stdin_reader(ops: "queue.Queue", log) -> None:
    for raw_line in sys.stdin:
        raw_line = raw_line.strip()
        if not raw_line:
            continue
        try:
            ops.put(json.loads(raw_line))
        except json.JSONDecodeError as exc:
            log(f"af_sampler: ignoring malformed control line: {exc}")
    # EOF on stdin means the supervisor is gone; wind down cleanly so the
    # last window still reaches the spool.
    ops.put({"op": "shutdown"})


def is_shutdown(op) -> bool:
    """True for the one op the main loop must act on *immediately* rather
    than queue until the window closes."""
    return isinstance(op, dict) and op.get("op") == "shutdown"


def run_loop(sampler, ops, interval: float, clock=time.monotonic, respond=None) -> None:
    """Drive `sampler` until shutdown, then return (the caller flushes).

    The loop waits on the control queue with a timeout that expires at the
    next window boundary instead of sleeping the whole interval and then
    draining:

    * `queue.Empty` *is* the tick — close the window, emit, open the next,
      and only then apply the ops that arrived during it, so no window is
      ever retroactively re-attributed (the property the old drain-after-
      sleep shape existed to guarantee).
    * a `shutdown` op returns straight away, **before** a new window is
      opened. `Sampler.finish()` then closes the window that is genuinely
      in flight, exactly once. The old shape opened a fresh window and
      immediately closed it, emitting a degenerate `t_start == t_end`
      sample (a division hazard for any downstream J/s or W figure), and
      made shutdown wait up to a full interval.
    """
    deadline = clock() + interval
    pending = []
    while True:
        remaining = deadline - clock()
        try:
            op = ops.get(timeout=remaining if remaining > 0 else 0)
        except queue.Empty:
            sampler.tick()
            queued, pending = pending, []
            for queued_op in queued:
                sampler.apply_op(queued_op)
            # Recomputed from now, not `deadline + interval`: a window that
            # overran (codecarbon's stop/start do blocking measurement)
            # must not be chased by a burst of catch-up ticks.
            deadline = clock() + interval
            continue
        if is_shutdown(op):
            return
        if isinstance(op, dict) and op.get("op") == "ping" and "id" in op:
            if respond is not None:
                respond({"id": op["id"], "ok": True, "status": "ready"})
            continue
        pending.append(op)


def positive_interval(value: str) -> float:
    """`--interval` type: reject anything that isn't a finite positive
    number. Silently coercing a bad value to the default would hide a
    misconfigured supervisor behind windows of a length nobody asked for."""
    try:
        seconds = float(value)
    except (TypeError, ValueError):
        raise argparse.ArgumentTypeError(f"must be a number, got {value!r}")
    if not seconds > 0 or seconds == float("inf"):
        raise argparse.ArgumentTypeError(f"must be a positive number of seconds, got {value!r}")
    return seconds


def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        prog="af_sampler",
        description="codecarbon/psutil collector sidecar for agentic-footprint",
    )
    parser.add_argument(
        "--state-dir",
        default=os.environ.get("AF_STATE_DIR", DEFAULT_STATE_DIR),
        help="agentic-footprint state dir (spool/ lives under it)",
    )
    parser.add_argument("--session", required=True, help="session id to tag events with")
    parser.add_argument(
        "--interval",
        type=positive_interval,
        default=DEFAULT_INTERVAL_S,
        help="seconds per sampling window (default: %(default)s)",
    )
    parser.add_argument(
        "--country-iso-code",
        default=os.environ.get("AF_COUNTRY_ISO_CODE", "USA"),
        help=(
            "required by OfflineEmissionsTracker; unused here since this "
            "sidecar emits energy only (default: %(default)s)"
        ),
    )
    return parser.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    interval = args.interval  # argparse already rejected <= 0
    state_dir = Path(args.state_dir).expanduser()
    session_id = args.session
    spool_path = state_dir / "spool" / f"codecarbon.{sanitize_id(session_id)}.jsonl"

    sampler = Sampler(
        spool_path=spool_path,
        session_id=session_id,
        tracker_factory=_make_tracker_factory(interval, args.country_iso_code),
        process_inspector=_make_psutil_inspector(),
    )

    ops: "queue.Queue" = queue.Queue()
    reader = threading.Thread(
        target=_stdin_reader, args=(ops, _stderr_log), name="af-sampler-stdin", daemon=True
    )
    reader.start()

    sampler.start()
    try:
        def respond(message):
            print(json.dumps(message, separators=(",", ":")), flush=True)

        run_loop(sampler, ops, interval, respond=respond)
    except KeyboardInterrupt:
        pass
    finally:
        sampler.finish()
    return 0


if __name__ == "__main__":
    sys.exit(main())
