# Use afoot from another program

This guide is for programs that show Agentic Footprint data or drive its
setup, such as a desktop application. The interface is the `afoot` command
line: run a command, read one JSON document from stdout.

## The two documents to read

| Command | Answers | Writes anything |
|---|---|---|
| `afoot status` | Is afoot installed and running here, and is each agent configured? | No |
| `afoot summary --period 7d` | How many tokens, and what estimated impacts, over today, 7 days or 30 days? | No |

Both are described field by field in
[machine-readable output](../reference/machine-readable-output.md).

## Run a command and parse it

1. Run `afoot status` or `afoot summary` as a child process. No shell is needed.
2. Check the exit status. `0` means stdout holds one JSON document on a
   single line. Anything else means there is no document; stderr holds a
   message for a person.
3. Parse stdout as JSON. Do not parse stderr.
4. Check `protocol_version` before reading anything else.

```python
import json
import subprocess

SUPPORTED = 1

def read(args):
    done = subprocess.run(["afoot", *args], capture_output=True, text=True)
    if done.returncode != 0:
        raise RuntimeError(done.stderr.strip())
    document = json.loads(done.stdout)
    if document.get("protocol_version") != SUPPORTED:
        raise RuntimeError("unsupported afoot output version")
    return document

status = read(["status"])
summary = read(["summary", "--period", "7d"])
```

## Handle versions

- Treat a `protocol_version` you were not written for as unsupported. Tell
  the user that afoot and your program do not match; do not read the document
  on a best-effort basis.
- Ignore fields you do not know. Fields are added without a version change.
- `afoot summary` and `afoot status` version their documents separately.
- `afoot_version` identifies the binary. Do not use it to decide whether you can
  read the document.

## Exit statuses

| Command | Exit | Meaning |
|---|---|---|
| `afoot status` | 0 | A document was printed. A receiver that is down or an agent that is not configured is data in the document, not an error. |
| `afoot summary` | 0 | A document was printed. A state directory with no store yet gives a document of zeros. |
| `afoot summary` | 1 | The store exists and could not be read. |
| either | 2 | The command line was not understood, for example an unknown `--period`. |

## How often to run them

`afoot watch` writes to the store once a minute, so `afoot summary` changes at most
that often.

`afoot status` lists processes, inspects running agent processes, makes one
request to the local receiver and runs the managed Python runtime's import
check. It can take a few seconds; do not run it on a thread that must stay responsive.

## Interpreting results

- Impacts in `afoot summary` cover `estimated_calls` only. When
  `unestimated_calls` is not zero, `remote` ranges are a lower bound; display
  `models_not_estimated` alongside them so users see which models are excluded.
- Each impact is a modeled range (`min`, `max`) for remote inference, not a
  local measurement. Read `unit` from the payload rather than hardcoding it.
- In `afoot status`, an agent is `receiving` when the store has an event from it
  in the last 24 hours; `last_event_at` gives the latest timestamp.
- `collector.running` is `null` on Windows (no Unix console socket).
- When `python.ready` is `false` in `afoot status`, tokens are still recorded
  but impacts stay `pending`. Show `python.fix` to the user.

See [known limitations](../reference/known-limitations.md).

## Use the same state directory

Every command reads the state directory given by `AFOOT_STATE_DIR`, or the
platform default (`~/.local/state/agentic-footprint` on macOS and Linux,
`%LOCALAPPDATA%\agentic-footprint` on Windows). A program that runs `afoot`
must run it with the same `AFOOT_STATE_DIR` as the collector, or it reads an
empty directory.

## Commands that change the machine

!!! warning "Require explicit user consent"
    The commands below modify files, install services, or download packages. Run them only in response to an explicit user action.

| Command | What it changes |
|---|---|
| `afoot setup` | Edits agent configuration files (Claude Code settings, Codex configuration) and installs the OpenCode plugin and Pi extension, backing up each existing file it changes; installs or registers the Claude Code hook; can install and start the per-user background service. Interactive unless `--yes` is passed. |
| `afoot service install` | Writes a per-user service definition (launchd on macOS, systemd user session on Linux, Task Scheduler logon task on Windows) and starts `afoot watch` under it. |
| `afoot service start` | Starts or restarts that service. |
| `afoot self update` | Replaces the `afoot` binary, refreshes the managed Python runtime, and restarts the background receiver. Works only for an afoot installed from a release binary. |
| `afoot report` | Ingests pending facts and writes derived results to the store before printing. |
| `afoot replay` | Deletes the derived records and recomputes them. |

`afoot setup --check` and `afoot setup --dry-run` write nothing; `--check` exits
non-zero when something needs to change. `afoot self update --dry-run` prints
what would change.

There is no command yet to stop or remove the service, or to revert what
`afoot setup` wrote.

`afoot watch` and `afoot console` are long-running servers, not queries.

## Commands that change nothing

`afoot status`, `afoot summary`, `afoot doctor`, `afoot statusline`, `afoot setup --check`,
`afoot setup --dry-run`, `afoot service status` and every `--help`.

`afoot doctor` prints text for a person and exits 1 when something needs
attention. For a program, `afoot status` carries the same kind of findings as
data.

## Do not read the store directly

!!! warning "Under active development"
    The local store layout and programmatic export interfaces are still evolving and may change in upcoming releases. Rely on the versioned CLI outputs (`afoot summary`, `afoot status`) rather than coupling to internal files.

The SQLite store (`state.db`), the spool and the other files in the state
directory are internal. Their layout is not a contract and can change in any
version, and the running `afoot watch` writes to the store while you would be
reading it. `afoot summary` and `afoot status` are the supported way to read them. If you need a figure they
do not carry, ask for it to be added to the document instead of querying the
database.
