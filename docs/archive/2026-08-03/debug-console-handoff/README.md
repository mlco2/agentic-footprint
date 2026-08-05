# Handoff: `af watch --debug` web console

## Overview

A local web debugging interface for **agentic-footprint** (`af`) — the control plane
defined in `docs/rfc/0001-architecture.md`. It is the development-phase tool called for in
RFC §6 ("Watchdog & debugging interface") and specified further in
`docs/design-log.md` ("watch --debug output format"): a live view of incoming events,
open spans, watched pids, attribution decisions and orphan flags.

It answers three questions the PoC team hits daily:

1. **Is the spool receiving the events I expect, well-formed?** (collector health, schema
   conformance, quarantined lines)
2. **How did machine energy get split across overlapping spans?** (per-sample allocation
   trace, L2 vs L1 shadow)
3. **Where are we being dishonest by omission?** (coverage gaps, orphaned compute,
   `unknown_model`, `pending` estimates, unmeasured remote spans)

## ⚠️ Sequencing

**Do not start this until the tracker PoC is done.** The console is a consumer of the
control plane's read model; nearly every panel depends on a capability that does not exist
yet. `DATA-CONTRACT.md` lists exactly what must exist first, and flags which parts are
already in RFC iteration-1 scope versus genuinely new work.

## About the design files

`Debug Console.dc.html` is a **design reference written in HTML** — a prototype showing
intended look, layout and behaviour. It is **not production code to copy**.

It runs on a self-contained synthetic data generator (`seed()`, `step()`, `llm()`,
`span()`, `energy()` in its logic class). That generator exists only so the prototype
moves and can be judged; **delete it** when implementing. Every number it invents has a
real source named in `DATA-CONTRACT.md`.

Recreate the design in whatever front-end environment the repo settles on. There is no web
UI in `agentic-footprint` today, so the framework is an open choice — see
"Implementation notes" below for a recommendation that matches the repo's constraints.

## Fidelity

**High-fidelity.** Final colours, typography, spacing, density and interaction states.
All values come from the bound **Broadsheet** design system
(`_ds/broadsheet-cbf7a41a-3fb9-4e0e-bcbd-4ed714de9abf/styles.css`) — see
`DESIGN-SYSTEM.md`. Recreate pixel-close, but take the tokens from that stylesheet rather
than transcribing hex values by hand.

One deliberate divergence is recorded in `DESIGN-SYSTEM.md` §7: Broadsheet's `.table`,
`.tag` and `.btn` component classes are **not** used. Read that section before "fixing" it.

## Screens / views

Five tabs. `Timeline` additionally ships **three alternative layouts** (A/B/C) behind a
switcher in the tab bar — these are design options for the team to choose between, **not
three features to build**. Pick one (recommendation: **A**, the only one with the decision
log) and drop the others.

| # | Tab | Purpose |
|---|---|---|
| 1 | **Timeline** | Live gantt of `action_span` over a machine-power chart; the decision log mirroring `af watch --debug` stderr |
| 2 | **Stream** | Filterable raw event table + JSON inspector + correlated-neighbour panel |
| 3 | **Attribution** | One `energy_sample` → how its joules were apportioned; L2 active vs L1 shadow |
| 4 | **Impact** | `impact_join` for the session: measured vs modelled, kept separate |
| 5 | **Health** | Per-collector rates, schema conformance, quarantined lines, `af python doctor` |

Detailed layout, sizes and per-component specs: **`SCREENS.md`**.

## Interactions & behaviour

- **Tab switch** — pure client state. Only the visible tab's data is computed (the
  prototype gates every derived array on `tab`/`layout`; keep that discipline, it is what
  makes a 1 Hz refresh affordable).
- **Selection** — clicking any bar, table row, sample or correlated row sets one shared
  `selectedId`. The inspector is a single component that switches on the selected record's
  type (`action_span` | `llm_call` | `energy_sample` | other).
- **Live / paused** — pause freezes the time window (`now`) but must **not** drop the SSE
  connection; events keep buffering so nothing is lost. Label it "SSE paused · buffering".
- **Type filters** — chips toggle event types out of the stream. Client-side only.
- **Refresh cadence** — 1 Hz. Do not go faster; the timeline is a 3-minute window and
  nothing meaningful changes in 250 ms.
- **Hover** — every bar carries a `title` with the full numbers (watts, joules per
  component + method, duration, locus, pid, span_id). This is load-bearing: it is how the
  dense chart stays readable. Reproduce it, or a tooltip equivalent.
- **Empty and error states** — the console must render before any data arrives
  (all panels have a defined empty state) and must show a disconnected banner rather than
  a stale-but-plausible chart if SSE drops.

## State management

Four stores. See `DATA-CONTRACT.md` §3 for the full shape.

| Store | Holds | Notes |
|---|---|---|
| `SessionStore` | `session_meta`, host profile, active `attribution_policy`, methodology versions | Fetched once on connect |
| `EventStore` | ring buffer of Contract #1 events + a time-bucketed span index + type counts | Bounded; see §3.2 for the bucket structure |
| `AllocStore` | allocation trace per `energy_sample.event_id` | **Server-computed.** Memoise by id; never recompute client-side |
| `UiStore` | `tab`, `layout`, `selectedId`, `hiddenTypes`, `live`, `now` | Pure client state |

## Design tokens

Do not transcribe. Link Broadsheet's `styles.css` and use its variables. The token names
this design relies on are enumerated in `DESIGN-SYSTEM.md` §2, with the console-specific
semantic mapping (which token means "measured", which means "alarm") in §3.

## Assets

None. No images, no icon font, no inline SVG. Broadsheet specifies **Phosphor duotone**
icons if you later add any; the console currently uses none by design — it is set entirely
in type and ruled fills.

Fonts: **Source Serif 4** (400 / 600 / 700 + italic), loaded from Google Fonts in the
prototype. Vendor it locally for an offline-first local tool.

## Files

| File | What it is |
|---|---|
| `Debug Console.dc.html` | The prototype. Single file: template + logic class. |
| `DESIGN-SYSTEM.md` | The design language, tokens, and the console's own visual conventions. |
| `DATA-CONTRACT.md` | **The main implementation document.** Endpoints, payloads, stores, and the gap list. |
| `SCREENS.md` | Per-screen layout and component specs. |
| `_ds/broadsheet-…/` | The bound design system. `styles.css` is the only stylesheet to link. |

## Implementation notes

- **Keep it local-first.** Nothing in this console should reach the network except
  `127.0.0.1`. Vendor the font. No CDN, no analytics, no telemetry — the whole point of the
  project is that nothing leaves the machine (RFC §6).
- **Framework**: the repo is Rust-first and ships one binary. The lowest-friction option is
  to serve a small static bundle from the `af` binary itself (`include_dir!`) — plain
  TypeScript + a light reactive layer, no build server. If the team would rather have a
  real component framework, any of React/Svelte/Solid is fine; the design has no framework
  dependency. What matters is that `af watch --debug --serve` needs no external toolchain
  at runtime.
- **The client must not compute impacts or attribution.** RFC §3 principle 1: collectors
  and presentation never own methodology. The prototype violates this (it computes L2
  in-browser) purely because it has no backend. In production the allocation trace is
  fetched. This is the single most important correction to make when porting.
