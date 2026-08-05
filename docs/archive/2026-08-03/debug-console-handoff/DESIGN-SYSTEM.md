# Design system guide — `af` debug console

The console consumes the bound **Broadsheet** design system. This document covers (1) how
to consume it, (2) the tokens in use, (3) the console-specific semantic layer built on top,
and (4) the conventions that keep a dense telemetry view legible in a newsprint system.

---

## 1. Consuming Broadsheet

```html
<link rel="stylesheet" href="_ds/broadsheet-cbf7a41a-3fb9-4e0e-bcbd-4ed714de9abf/styles.css">
```

That is the only stylesheet. Take every colour, font, space and radius from its variables.
Do not hard-code a hex, a font name, or a px value the tokens already carry.

Broadsheet's own guidance, and how this console honours it:

| Broadsheet rule | In this console |
|---|---|
| Light ground, no dark surfaces | `--color-bg` #f3f2f2 throughout. No dark mode. |
| Source Serif 4 for headings *and* body | Yes — including tables, numbers, the decision log and JSON. |
| "Do not introduce a sans-serif for UI chrome; the serif is the chrome" | Honoured. **There is no monospace either** — see §4. |
| "Do not structure the page with rules, borders or boxes" | Partially diverged — see §5. |
| Cyan for interactive, magenta as the rarer second spot colour | Honoured, and given analytical meaning — see §3. |
| Phosphor duotone icons | None used; the design needs no icons. |
| Density 1.25×, radius 2px baked into the scales | Use `--space-*` / `--radius-*`, never raw numbers. |

---

## 2. Tokens in use

**Ground and ink**
`--color-bg` · `--color-text` · `--color-neutral-200/300/400/600/700/800/900`

**Accents**
`--color-accent` (cyan #0088b0) · `--color-accent-200/300/700/800`
`--color-accent-2` (magenta #d6006c) · `--color-accent-2-200/700`

**Type** — `--font-heading` / `--font-body` (both Source Serif 4);
`--font-heading-weight` 600. Masthead uses 700.

**Space** — `--space-1` 5px · `--space-2` 10px · `--space-3` 15px · `--space-4` 20px ·
`--space-6` 30px · `--space-8` 40px

**Radius** — `--radius-md` 2px (the only one used; the console is nearly square-cornered)

**Not used**: `--color-process-yellow` (a print treatment, never interface chrome),
`--shadow-*` (the console has no elevation), `--color-surface`, `.cmyk` / `.halftone`.

---

## 3. Semantic layer — the one idea to preserve

The console's central visual device, and the reason it works as a *measurement* tool:

> **Solid ink = measured. Hatching = modelled, estimated, or not locally measurable.**

```css
/* modelled / unmeasurable */
background-image: repeating-linear-gradient(45deg,
  var(--color-neutral-700) 0 1.5px, transparent 1.5px 4px);

/* alarm: orphaned compute, coverage gap */
background-image: repeating-linear-gradient(45deg,
  var(--color-accent-2) 0 1.5px, transparent 1.5px 4px);
```

This encodes RFC §3 principle 3 (*failure honesty*) as a **texture rather than a colour**,
which is why the palette stays inside Broadsheet's two accents. Full mapping:

| Meaning | Treatment |
|---|---|
| Locally measured energy (rapl / powermetrics / nvml) | Solid `--color-accent` |
| Modelled (`tdp_model`), remote-estimated, `execution_locus: remote` | Neutral hatch, transparent fill, `--color-neutral-700` border |
| Baseline / idle remainder | Solid `--color-neutral-300`, hatched in share bars |
| **Alarm** — orphaned pid tree, coverage gap, quarantined line, `unknown_model`, transcript-sourced usage | `--color-accent-2` (magenta) — hatched for regions, solid for text |
| Interactive / selected | `--color-accent`; selected row fill `--color-accent-200`; selected bar ring `0 0 0 2px var(--color-text)` |
| Agent's own process (pid 4412) | `--color-accent-300` — distinct from tool work |

**Magenta is rationed.** It appears only where the control plane is admitting a gap. If
magenta shows up as decoration, the device is broken.

**Tool-kind fills** (all within the neutral/cyan range, so kind never competes with the
measured/modelled axis):

| `tool_kind` | Fill | Border |
|---|---|---|
| `bash` | `--color-accent` | `--color-accent-700` |
| `file_op` | `--color-accent-300` | `--color-accent-700` |
| `mcp` | `--color-neutral-300` | `--color-neutral-700` |
| `web` | transparent + neutral hatch | `--color-neutral-700` |
| `subagent` | `--color-neutral-400` | `--color-neutral-800` |
| `status: error` | any fill, but border → `--color-accent-2` | |

---

## 4. Type

Everything is Source Serif 4. **There is no monospace anywhere** — including the raw JSONL
inspector, the decision log and the `af statusline` preview. This is deliberate and it is
the system's instruction, not an oversight: newspapers set listings, legal notices and
market tables in serif.

Two things make it work; do not drop either:

- `font-variant-numeric: tabular-nums` on the root. Without it, columns of figures do not
  line up and the whole thing falls apart.
- `white-space: pre-wrap` + `word-break: break-word` on JSON and log output.

**Scale as used** (px, since Broadsheet carries no type-scale variables):

| Role | Size / weight |
|---|---|
| Masthead | 19 / 700 |
| Tab labels | 14 / 600 active, 400 idle |
| Big figures (impact cards) | 33 / 600, `letter-spacing: -.025em` |
| Section headings (KPIs, panel titles) | 15–17 / 600 |
| Eyebrow labels | 11 / 600, `letter-spacing: .09em`, uppercase, `--color-neutral-700` |
| Table body, log lines, inspector values | 12–12.5 / 400 |
| Table column headers | 10.5 / 400, `letter-spacing: .07em`, uppercase |
| Chart lane labels, bar labels | 10.5–11.5 |
| Footer / metadata | 11 |

11px is the floor. This is a desktop-only tool for two or three developers; it is denser
than Broadsheet's editorial pages by design, but the scale relationships are the system's.

---

## 5. Structure — the one honest divergence

Broadsheet says: *"Do not structure the page with rules, borders or boxes."* An app shell
with five simultaneous panes cannot fully honour that. What the console does:

- **Kept**: no cards, no boxed panels, no elevation, no rounded containers. Sections are
  separated by whitespace and by the type scale.
- **Diverged**: a small, fixed vocabulary of rules, all in full-strength ink
  (`--color-text`) or `--color-neutral-200`, and nothing else:
  1. `3px` masthead rule under the header — Broadsheet's sanctioned "front-page furniture"
     thick-thin head pair.
  2. `1px` ink rule under the tab bar, above the footer, and between top-level panes.
  3. `1px` ink rule under every table header row; `1px --color-neutral-200` between body
     rows. This is the one place Broadsheet itself prints rules (`.table`).
  4. `1px` ink rule on the left edge of every chart plot area (the axis).
  5. `2px --color-accent-2` left border on quarantined-line entries.

Adding a sixth kind of rule is how this drifts back into generic devtools chrome. Don't.

---

## 6. Interaction states

From Broadsheet, unmodified:

```css
:focus-visible { outline: 2px solid var(--color-accent); outline-offset: 2px; }
::selection    { background: var(--color-accent-200); }
```

Buttons are ghost-style: transparent ground, a `2px` bottom border in `--color-accent` when
active, `--color-neutral-600` label when idle. Hover takes an `--color-accent-200` tint.
Pressed goes one step past base (`--color-accent-600`). Disabled drops to 45% opacity.

Row hover in tables: `--color-neutral-100`. Row selected: `--color-accent-200`.

---

## 7. Why Broadsheet's component classes are not used

`.table`, `.tag` and `.btn` are deliberately **not** applied. This is a recorded decision,
not an omission — an automated adherence check will flag it.

- **`.table`** styles a semantic `<table>` with a themed header and row rules. These tables
  need per-column flex widths that stay aligned across a sticky header, inline share bars
  inside cells, and hatch fills carrying the measured/modelled distinction (§3). Adopting
  `.table` means overriding most of it back and losing the hatch device.
- **`.tag`** is a decorative label. The `usage_source` and method badges are
  **colour-coded by provenance rank** (Annex B: `api_response` → `agent_telemetry` →
  `transcript` → `estimated`), which is semantic, not decorative.
- **`.btn`** carries Broadsheet's airy padding; the tab bar is 32px tall.

Everything else — tokens, type, scale, accent pair, interaction states — is the system's.
If the team prefers literal component-class adherence and will accept a plainer chart,
switching `.tag` and `.btn` is cheap; `.table` is the expensive one.
