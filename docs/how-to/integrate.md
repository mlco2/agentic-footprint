# Use af from another program

This guide is for programs that show Agentic Footprint data or drive its
setup, such as a desktop application. The interface is the `af` command
line: run a command, read one JSON document from stdout.

## The two documents to read

| Command | Answers | Writes anything |
|---|---|---|
| `af status` | Is af installed and running here, and is each agent configured? | No |
| `af summary --period 7d` | How many tokens, and what estimated impacts, over today, 7 days or 30 days? | No |

Both are described field by field in
[machine-readable output](../reference/machine-readable-output.md).

## Run a command and parse it

1. Run `af status` or `af summary` as a child process. No shell is needed.
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
    done = subprocess.run(["af", *args], capture_output=True, text=True)
    if done.returncode != 0:
        raise RuntimeError(done.stderr.strip())
    document = json.loads(done.stdout)
    if document.get("protocol_version") != SUPPORTED:
        raise RuntimeError("unsupported af output version")
    return document

status = read(["status"])
summary = read(["summary", "--period", "7d"])
```

## Handle versions

- Treat a `protocol_version` you were not written for as unsupported. Tell
  the user that af and your program do not match; do not read the document
  on a best-effort basis.
- Ignore fields you do not know. Fields are added without a version change.
- `af summary` and `af status` version their documents separately.
- `af_version` identifies the binary. Do not use it to decide whether you can
  read the document.

## Exit statuses

| Command | Exit | Meaning |
|---|---|---|
| `af status` | 0 | A document was printed. A receiver that is down or an agent that is not configured is data in the document, not an error. |
| `af summary` | 0 | A document was printed. A state directory with no store yet gives a document of zeros. |
| `af summary` | 1 | The store exists and could not be read. |
| either | 2 | The command line was not understood, for example an unknown `--period`. |

## How often to run them

`af watch` writes to the store once a minute, so `af summary` changes at most
that often.

`af status` lists processes, inspects running agent processes, makes one
request to the local receiver and runs the managed Python runtime's import
check. It can take a few seconds; do not run it on a thread that must stay responsive.

## Read the result honestly

- Impacts in `af summary` cover `estimated_calls` only. When
  `unestimated_calls` is not zero the `remote` ranges are a lower bound; show
  `models_not_estimated` so a reader knows what is left out.
- Each impact is a range (`min`, `max`) from a model of remote inference. It
  is not a measurement. Read `unit` from the document.
- In `af status` an agent is `receiving` when the store has an event from it
  in the last 24 hours; `last_event_at` says when the latest one was.
- `collector.running` is `null` on Windows.
- When `python.ready` is `false` in `af status`, tokens are recorded but no
  impact is estimated: every call stays `pending`. Show `python.fix`.

See [known limitations](../reference/known-limitations.md).

## Use the same state directory

Every command reads the state directory given by `AF_STATE_DIR`, or the
platform default (`~/.local/state/agentic-footprint` on macOS and Linux,
`%LOCALAPPDATA%\agentic-footprint` on Windows). A program that runs `af`
must run it with the same `AF_STATE_DIR` as the collector, or it reads an
empty directory.

## Commands that change the machine

Running these is a decision for the user. Ask first.

| Command | What it changes |
|---|---|
| `af setup` | Edits agent configuration files (Claude Code settings, Codex configuration), after making a timestamped backup of each file it changes; installs or registers the Claude Code hook; on macOS and Linux can install and start the background service. Interactive unless `--yes` is passed. |
| `af service install` | Writes a per-user launchd or systemd service definition and starts `af watch` under it. macOS and Linux only. |
| `af service start` | Starts or restarts that service. |
| `af self update` | Replaces the `af` binary, refreshes the managed Python runtime, and restarts the background receiver. Works only for an af installed from a release binary. |
| `af report` | Ingests pending facts and writes derived results to the store before printing. |
| `af replay` | Deletes the derived records and recomputes them. |

`af setup --check` and `af setup --dry-run` write nothing; `--check` exits
non-zero when something needs to change. `af self update --dry-run` prints
what would change.

There is no command yet to stop or remove the service, or to revert what
`af setup` wrote.

`af watch` and `af console` are long-running servers, not queries.

## Commands that change nothing

`af status`, `af summary`, `af doctor`, `af statusline`, `af setup --check`,
`af setup --dry-run`, `af service status` and every `--help`.

`af doctor` prints text for a person and exits 1 when something needs
attention. For a program, `af status` carries the same kind of findings as
data.

## Do not read the store directly

The SQLite store (`state.db`), the spool and the other files in the state
directory are internal. Their layout is not a contract and can change in any
version, and the running `af watch` writes to the store while you would be
reading it. `af summary` and `af status` are the supported way to read them. If you need a figure they
do not carry, ask for it to be added to the document instead of querying the
database.
