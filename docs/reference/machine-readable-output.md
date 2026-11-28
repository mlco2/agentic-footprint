# Machine-readable output

`afoot summary` and `afoot status` each print one JSON document on stdout, on a
single line, for other programs to read. Both are read-only. Diagnostics, if
any, go to stderr.

Example payloads below use illustrative values.

## Versioning

Both documents carry `protocol_version`, currently `1` for each. The two
numbers are independent.

- `protocol_version` changes only when a program written against the previous
  shape would misread the new one.
- Fields can be added without a version change. Ignore fields you do not
  know.
- Treat a document whose `protocol_version` is not one you were written for
  as unsupported, and do not guess at its contents.

`afoot_version` is the version of the `afoot` binary that produced the document. It
is informative and is not the compatibility signal.

`afoot summary` has a JSON Schema,
[`schemas/summary/v1.schema.json`](https://github.com/mlco2/agentic-footprint/blob/main/schemas/summary/v1.schema.json).
`afoot status` has no schema file yet; this page is its description.

All timestamps are RFC 3339 in UTC with milliseconds, for example
`2026-03-12T16:41:07.512Z`.

## `afoot summary`

```sh
afoot summary --period 7d
```

Options and exit status are in the [CLI reference](cli.md#afoot-summary).

### Top-level fields

| Field | Type | Meaning |
|---|---|---|
| `protocol_version` | integer | `1`. See [Versioning](#versioning). |
| `afoot_version` | string | Version of the binary. |
| `generated_at` | string | When the document was built. |
| `period` | object | What the document covers; see below. |
| `collector` | object | Whether a collector is running, and its last stored event. |
| `totals` | figures | Every call of the period. |
| `by_agent` | array of figures | One element per agent, largest token total first. Each adds `agent` (`claude-code`, `codex`, `opencode`, `pi`, or `other` for a collector that belongs to no supported agent) and `name` (display name). |
| `by_model` | array of figures | One element per agent and model, largest token total first. Each adds `agent` and `model`. `model` is the model the provider reported serving, or the requested model when that is not known. |
| `models_not_estimated` | array | Models with calls that have no impact figure for a lasting reason; see below. |
| `by_day` | array of figures | One element per local day of the period, oldest first, including days with no call. Each adds `date` (`YYYY-MM-DD`). |

### `period`

| Field | Type | Meaning |
|---|---|---|
| `name` | string | `today`, `7d` or `30d`. |
| `from` | string | Start of the first local day. |
| `to` | string | When the document was built; equal to `generated_at`. |
| `utc_offset_minutes` | integer | Minutes east of UTC that days are cut in, for the whole period. |

A call belongs to the period when its timestamp is between `from` and `to`.
Days are cut with one offset; across a daylight-saving change, days on the
other side of the change are cut an hour off local midnight.

### `collector`

| Field | Type | Meaning |
|---|---|---|
| `running` | boolean or null | Whether an `afoot watch` answers on its console socket in the state directory. `null` on Windows, where there is no such socket to ask. |
| `last_event_at` | string or null | Timestamp of the most recent event in the store, of any kind and regardless of the period. `null` when the store is empty or missing. |

### Figures

`totals` and every element of `by_agent`, `by_model` and `by_day` carry the
same fields.

| Field | Type | Meaning |
|---|---|---|
| `calls` | integer | LLM calls in the group. |
| `sessions` | integer | Sessions with at least one call in the group. |
| `tokens` | object | Token counts as the agents reported them: `input`, `output`, `thought`, `cache_read`, `cache_write`, and `total`, their sum. |
| `estimated_calls` | integer | Calls whose impacts are in `remote`. |
| `unestimated_calls` | integer | `calls` minus `estimated_calls`. |
| `unestimated_by_reason` | object | Count of unestimated calls per reason: `pending` (the estimator has not stored a result yet) or the estimator's status, such as `unknown_model`. Empty when every call is estimated. |
| `remote` | object | Modelled impacts of the remote inference; see below. |

Tokens count every call. Impacts count estimated calls only: when
`unestimated_calls` is not zero, each `remote` range is a lower bound for the
group.

### `remote`

`remote` has one entry per impact criterion that could be summed: `energy`,
`gwp`, `water`, `adpe` and `pe`. Each entry is:

```json
{"unit": "kWh", "total": {"min": 0.41, "max": 1.37}}
```

- `total.min` and `total.max` are the sums of the estimator's ranges over the
  group's estimated calls. They are modelled, not measured.
- `unit` is the unit the estimator reported. Read it from the document
  instead of assuming it.
- A criterion is absent when no call in the group has an estimate, or when
  estimates reported it in different units. `remote` is `{}` when nothing is
  estimated.
- Nothing measured on this machine is included. Local energy is not part of
  this document.

### `models_not_estimated`

One element per agent and model that has at least one call left unestimated
for a reason other than `pending`, largest token total first.

| Field | Type | Meaning |
|---|---|---|
| `agent` | string | As in `by_model`. |
| `model` | string | As in `by_model`. |
| `reason` | string | The estimator's status for those calls, for example `unknown_model`. |
| `calls` | integer | Calls of this model unestimated for a lasting reason. |
| `tokens` | integer | The tokens of those calls only (a model can be partly estimated). |

These calls and tokens are counted everywhere else in the document. Their
impacts are in no `remote` sum.

### Example

A `today` summary with one unknown model:

```json
{
  "protocol_version": 1,
  "afoot_version": "0.1.0",
  "generated_at": "2026-03-12T16:41:07.512Z",
  "period": {
    "name": "today",
    "from": "2026-03-11T23:00:00.000Z",
    "to": "2026-03-12T16:41:07.512Z",
    "utc_offset_minutes": 60
  },
  "collector": {
    "running": true,
    "last_event_at": "2026-03-12T16:40:12.004Z"
  },
  "totals": {
    "calls": 50,
    "sessions": 3,
    "tokens": {
      "input": 12000,
      "output": 8000,
      "thought": 1000,
      "cache_read": 90000,
      "cache_write": 4000,
      "total": 115000
    },
    "estimated_calls": 40,
    "unestimated_calls": 10,
    "unestimated_by_reason": {"unknown_model": 10},
    "remote": {
      "energy": {"unit": "kWh", "total": {"min": 0.012, "max": 0.045}},
      "gwp": {"unit": "kgCO2eq", "total": {"min": 0.004, "max": 0.019}},
      "water": {"unit": "L", "total": {"min": 0.05, "max": 0.21}},
      "adpe": {"unit": "kgSbeq", "total": {"min": 1.1e-8, "max": 4.2e-8}},
      "pe": {"unit": "MJ", "total": {"min": 0.11, "max": 0.48}}
    }
  },
  "by_agent": [
    {
      "agent": "claude-code",
      "name": "Claude Code",
      "calls": 40,
      "sessions": 2,
      "tokens": {
        "input": 10000,
        "output": 7000,
        "thought": 1000,
        "cache_read": 80000,
        "cache_write": 4000,
        "total": 102000
      },
      "estimated_calls": 40,
      "unestimated_calls": 0,
      "unestimated_by_reason": {},
      "remote": {
        "energy": {"unit": "kWh", "total": {"min": 0.012, "max": 0.045}},
        "gwp": {"unit": "kgCO2eq", "total": {"min": 0.004, "max": 0.019}},
        "water": {"unit": "L", "total": {"min": 0.05, "max": 0.21}},
        "adpe": {"unit": "kgSbeq", "total": {"min": 1.1e-8, "max": 4.2e-8}},
        "pe": {"unit": "MJ", "total": {"min": 0.11, "max": 0.48}}
      }
    },
    {
      "agent": "codex",
      "name": "Codex",
      "calls": 10,
      "sessions": 1,
      "tokens": {
        "input": 2000,
        "output": 1000,
        "thought": 0,
        "cache_read": 10000,
        "cache_write": 0,
        "total": 13000
      },
      "estimated_calls": 0,
      "unestimated_calls": 10,
      "unestimated_by_reason": {"unknown_model": 10},
      "remote": {}
    }
  ],
  "by_model": [
    {
      "agent": "claude-code",
      "model": "example-model-a",
      "calls": 40,
      "sessions": 2,
      "tokens": {
        "input": 10000,
        "output": 7000,
        "thought": 1000,
        "cache_read": 80000,
        "cache_write": 4000,
        "total": 102000
      },
      "estimated_calls": 40,
      "unestimated_calls": 0,
      "unestimated_by_reason": {},
      "remote": {
        "energy": {"unit": "kWh", "total": {"min": 0.012, "max": 0.045}},
        "gwp": {"unit": "kgCO2eq", "total": {"min": 0.004, "max": 0.019}},
        "water": {"unit": "L", "total": {"min": 0.05, "max": 0.21}},
        "adpe": {"unit": "kgSbeq", "total": {"min": 1.1e-8, "max": 4.2e-8}},
        "pe": {"unit": "MJ", "total": {"min": 0.11, "max": 0.48}}
      }
    },
    {
      "agent": "codex",
      "model": "example-model-b",
      "calls": 10,
      "sessions": 1,
      "tokens": {
        "input": 2000,
        "output": 1000,
        "thought": 0,
        "cache_read": 10000,
        "cache_write": 0,
        "total": 13000
      },
      "estimated_calls": 0,
      "unestimated_calls": 10,
      "unestimated_by_reason": {"unknown_model": 10},
      "remote": {}
    }
  ],
  "models_not_estimated": [
    {
      "agent": "codex",
      "model": "example-model-b",
      "reason": "unknown_model",
      "calls": 10,
      "tokens": 13000
    }
  ],
  "by_day": [
    {
      "date": "2026-03-12",
      "calls": 50,
      "sessions": 3,
      "tokens": {
        "input": 12000,
        "output": 8000,
        "thought": 1000,
        "cache_read": 90000,
        "cache_write": 4000,
        "total": 115000
      },
      "estimated_calls": 40,
      "unestimated_calls": 10,
      "unestimated_by_reason": {"unknown_model": 10},
      "remote": {
        "energy": {"unit": "kWh", "total": {"min": 0.012, "max": 0.045}},
        "gwp": {"unit": "kgCO2eq", "total": {"min": 0.004, "max": 0.019}},
        "water": {"unit": "L", "total": {"min": 0.05, "max": 0.21}},
        "adpe": {"unit": "kgSbeq", "total": {"min": 1.1e-8, "max": 4.2e-8}},
        "pe": {"unit": "MJ", "total": {"min": 0.11, "max": 0.48}}
      }
    }
  ]
}
```

Here the day's impacts cover 40 of 50 calls. A reader should be told that the
`remote` totals leave out `example-model-b`.

## `afoot status`

```sh
afoot status
```

It takes no options and exits 0.

### Top-level fields

| Field | Type | Meaning |
|---|---|---|
| `protocol_version` | integer | `1`. See [Versioning](#versioning). |
| `afoot_version` | string | Version of the binary. |
| `binary` | string or null | Path of the running `afoot` executable; `null` when it cannot be resolved. |
| `state_dir` | string | The state directory in use (`AFOOT_STATE_DIR` or the platform default). |
| `self_update.managed` | boolean | Whether `afoot self update` manages this binary: `true` only when the installer placed it from a release binary. `false` for a build from a source checkout. |
| `service.supported` | boolean | Whether `afoot service` can manage a background receiver here: macOS (launchd), Linux with a systemd user session, or Windows (Task Scheduler). |
| `service.installed` | boolean | Whether the per-user service definition file exists. It does not say the service is running; `receiver.reachable` and `collector.running` do. |
| `receiver.endpoint` | string | The OTLP logs endpoint that was checked: the installed service's, otherwise `http://127.0.0.1:4318/v1/logs`. |
| `receiver.reachable` | boolean | Whether that endpoint answered as the receiver of this afoot version. |
| `receiver.note` | string or null | When not reachable, the reason, as a sentence for a person: nothing listening, another program on the port, or an afoot of another version. `null` when reachable. |
| `collector.running` | boolean or null | Whether an `afoot watch` answers on its console socket in the state directory. `null` on Windows. |
| `python.ready` | boolean | Whether the managed Python runtime (CodeCarbon and EcoLogits) is installed and works. Without it tokens are still recorded; there are no impact figures. |
| `python.problem` | string or null | What is wrong with the runtime, as a sentence for a person. `null` when ready. |
| `python.fix` | string or null | What to do about it, for example ``run `afoot python setup` ``, or to install uv first when nothing on this machine can build the runtime. `null` when ready. |
| `python.provisioner` | string or null | What would build the runtime here: `uv`, or `python` (a system Python with the `venv` module). `null` when neither was found, and when the runtime is ready (it is not looked for then). |
| `python.uv_install_url` | string | Where uv's install instructions are. |
| `storage.database_bytes` | integer | Size of the store (`state.db` and its write-ahead files), in bytes. `0` when there is none. |
| `storage.spool_bytes` | integer | Size of the files in the spool folder (what hooks wrote), in bytes. |
| `storage.spool_files` | integer | How many files the spool folder holds. |
| `storage.logs_bytes` | integer | Size of the service's log files, in bytes. |
| `agents` | array | One row per supported agent (`claude-code`, `codex`, `opencode`, `pi`), installed or not. Empty when the home directory cannot be determined. |

`receiver.reachable` and `collector.running` answer different questions. An
`afoot watch` started with `--no-otlp`, or one whose receiver port was taken, is
a running collector with no reachable receiver.

### Agent rows

| Field | Type | Meaning |
|---|---|---|
| `id` | string | `claude-code`, `codex`, `opencode`, or `pi`. |
| `name` | string | Display name. |
| `state` | string | See [agent states](#agent-states). |
| `detail` | string or null | What is wrong with the user-scope configuration file, when `state` was read from it. |
| `action` | string or null | A command or instruction that addresses the state, for example `afoot setup` or `restart claude`. `null` when the state was read from a process, whose row carries its own. |
| `config_path` | string or null | The user-scope configuration file that carries telemetry configuration; `null` when there is none. |
| `processes` | array or null | One row per running process of the agent; `null` where processes cannot be listed (Windows). |
| `sessions` | integer | Sessions with an event in the last 24 hours. |
| `last_event_at` | string or null | The agent's most recent event in the last 24 hours, else `null`. |

Each element of `processes`:

| Field | Type | Meaning |
|---|---|---|
| `pid` | integer | Process id. |
| `started_at` | string or null | When the process started. |
| `stale` | boolean | Whether the process started before the configuration it loads was last changed. |
| `cwd` | string or null | The process's working directory, when it could be inspected. |
| `config_path` | string or null | The configuration file this process loads, when known. |
| `state` | string | The state of this process; see [agent states](#agent-states). |
| `detail` | string or null | The reason for the state, when there is one. |
| `action` | string or null | What addresses it, for example `afoot setup --agents codex --project /path/to/project`. |
| `session_id` | string or null | The agent session tied to this process, when known. |
| `last_event_at` | string or null | The most recent event of this process's session in the last 24 hours, when the session is known. |

### Agent states

| State | Meaning | Where |
|---|---|---|
| `receiving` | An event from this agent has arrived (`afoot status`: in the last 24 hours). | Agent and process. |
| `needs_restart` | Configured, but the process started before the configuration last changed, so it has not loaded it. Restart the agent. | Agent and process. |
| `misconfigured` | Telemetry configuration is present but will not reach this receiver. `detail` says why. | Agent and process. |
| `not_configured` | Installed or running, with no telemetry configuration. Run `afoot setup`. | Agent and process. |
| `waiting` | Configured; nothing more is known. | Agent and process. |
| `not_installed` | The agent's command is not on `PATH` and it has no configuration. | Agent only. |
| `unknown` | The process could not be inspected. | Process only. |

An agent with an event is `receiving`, whatever its files say. Otherwise the
agent's `state` is the most actionable state among its processes, in the
order `needs_restart`, `misconfigured`, `not_configured`, `waiting`. With no
process that says one of these, it comes from the user-scope configuration
file and `PATH`.

!!! note "What `receiving` means in `afoot status`"
    `afoot status` reads the store: an agent with a stored event in the last 24
    hours is `receiving`, with `sessions` and `last_event_at` counted over
    those 24 hours. An agent used yesterday and misconfigured since is still
    `receiving` until the 24 hours have passed. The running collector reports
    what it has received since it started, on
    [`GET /debug/status`](debug-api.md#watcher-status).

### Example

A Linux machine with the background service installed, Claude Code configured
and running, and Codex not installed:

```json
{
  "protocol_version": 1,
  "afoot_version": "0.1.0",
  "binary": "/home/alex/.local/bin/afoot",
  "state_dir": "/home/alex/.local/state/agentic-footprint",
  "self_update": {"managed": false},
  "service": {"supported": true, "installed": true},
  "receiver": {
    "endpoint": "http://127.0.0.1:4318/v1/logs",
    "reachable": true,
    "note": null
  },
  "collector": {"running": true},
  "python": {
    "ready": true,
    "problem": null,
    "fix": null,
    "provisioner": null,
    "uv_install_url": "https://docs.astral.sh/uv/getting-started/installation/"
  },
  "storage": {
    "database_bytes": 48234496,
    "spool_bytes": 9318400,
    "spool_files": 212,
    "logs_bytes": 81920
  },
  "agents": [
    {
      "id": "claude-code",
      "name": "Claude Code",
      "state": "waiting",
      "detail": null,
      "action": null,
      "config_path": "/home/alex/.claude/settings.json",
      "processes": [
        {
          "pid": 4242,
          "started_at": "2026-03-12T08:02:11.000Z",
          "stale": false,
          "cwd": "/home/alex/work/example-project",
          "config_path": "/home/alex/.claude/settings.json",
          "state": "waiting",
          "detail": null,
          "action": null,
          "session_id": null,
          "last_event_at": null
        }
      ],
      "sessions": 0,
      "last_event_at": null
    },
    {
      "id": "codex",
      "name": "Codex",
      "state": "not_installed",
      "detail": null,
      "action": null,
      "config_path": null,
      "processes": [],
      "sessions": 0,
      "last_event_at": null
    }
  ]
}
```
