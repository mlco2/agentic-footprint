# Screens — layout and component specs

Shell is fixed: `100vh`, `display:flex`, `flex-direction:column`, `overflow:hidden`.
Masthead (auto) → tab bar (32px) → `main` (`flex:1; min-height:0`) → footer (auto).

Two flex rules that caused real bugs in the prototype; carry them over:
- Chart scroll containers are **`flex: 0 1 auto`**, never `flex: 1`. With `flex:1` the
  container absorbs all leftover height and strands the axis hundreds of px below the lanes.
- Every scrollable flex child needs `min-height: 0`, or it refuses to shrink.

---

## 1. Timeline

Three alternative layouts. **Choose one** — recommendation **A**.

### Layout A — rail · chart · inspector (recommended)
```
196px rail │ flex:1 chart column │ 320px inspector
```
- **Rail** (`overflow:auto`, `--space-2` padding): Collectors (status dot + name +
  "N ev · N/s · N rejected"), Event types (toggle chips with 8px swatch + count),
  Watchdog (pid, cmd, cpu/rss, state) with an orphan summary line in magenta italic.
- **Chart column**: title row ("Session timeline", window label, span count, legend) →
  chart (`flex:0 1 auto; overflow:auto`) → axis label row (16px) → **decision log**
  (`flex:1 1 auto; min-height:130px; max-height:38%`).
  - Chart is a 116px right-aligned label gutter + plot area with a `1px` ink left border.
  - Lanes top→bottom: `llm_call` (22px) · `action_span` (≤7 packed tracks × 21px) ·
    `energy_sample` (54px, bottom-anchored bars) · `process_sample` (34px).
  - Decision log rows: 76px prefix (colour per `kind`, 600 weight for `[orphan]`) ·
    76px timestamp · text. Cap `[attr]` lines to ~8 of ~30 visible — the sampler emits one
    per 2s and will otherwise bury `[span open]` and `[ingest]` entirely.
- **Inspector**: eyebrow (record type) → 17px/600 title → italic sub → key/value rows
  (106px key) → share bars when a span or sample is selected → raw JSONL.

### Layout B — wide lanes · live tail
KPI strip (7 figures at 22px/600) → one lane per `tool_kind` (≤3 packed rows × 9px) →
92px stacked machine-power lane (cpu+dram solid cyan, gpu neutral above) → 372px right
aside: live tail + a 178px JSON pane.
**Note**: the tail must exclude `process_sample` and `energy_sample`, or it is 100% sampler
noise. This layout has no filter chips, so the exclusion has to be built in.

### Layout C — overview strip · table · drawer
78px overview strip (energy bars with span ticks over them, axis inside) → filter chip row →
full-width event table → 226px drawer (286px inspector · correlated context · 318px JSON).

**Shared table geometry** (also used by the Stream tab), `gap: var(--space-2)`:
`ts` 80 · `type` 104 · `collector` 118 · `attribution` 86 · `facts` flex:1 ·
`source/method` 112 · `status` 48 right. Sticky header, uppercase 10.5px, ink rule under.

**Stream rows must be sorted by timestamp**, not arrival order — spans and `llm_call`s are
stamped at their *end* time while energy samples arrive on their own cadence, so insertion
order is not chronological.

---

## 2. Stream
`flex:1` table column + 352px inspector aside. Filter chips + "clear filters" ghost button +
"N shown of M · newest first". Same table geometry as above. Inspector adds a
**Correlated** section: events within ±6s of the selection, with a signed `±Ns` offset,
each clickable.

---

## 3. Attribution
```
232px sample list │ flex:1 detail │ 274px policy aside
```
- **Sample list**: `hh:mm:ss → hh:mm:ss`, total joules in cyan, "N spans · idle N%", and a
  magenta "L1 N%" flag when the shadow sum exceeds 100%. Selected row gets an
  `--color-accent-200` fill and a `2px` cyan left border.
- **Detail**: 19px title + interval + sample id → six stats at 17px/600 (total measured,
  avg power, overlapping spans, attributed, baseline/idle, "l1 would sum to") →
  **sample-interval strip** (spans drawn to scale inside the sample's bounds, 19px rows,
  ink borders left and right marking the interval) → components table (solid = measured,
  hatched = modelled) → allocation table:
  `span` flex:1 · `locus` 58 · `overlap` 68 · `cpu Δ` 78 · `l2 share` 122 (bar + %) ·
  `l2 joules` 70 · `l1 joules` 70, with an agent-process row and a
  `baseline / idle · not attributed to any action` row (hatched bar) → notes:
  over-attribution, remote exclusion, idle share.
  - Sub-joule shares must show 2 decimals. Rounding to "0 J" hides exactly the small-share
    rows the L1-vs-L2 comparison exists to expose.
- **Aside**: policy prose → formula with this sample's real numbers substituted →
  `process_sample` cpu deltas → grid intensity (incl. "auto-geolocated: never" in cyan).

---

## 4. Impact
`flex:1` main + 296px aside.
- Three figures at `--space-8` gaps, 33px/600: **local · measured** (cyan, solid swatch),
  **remote · modelled** (ink, hatched swatch), **combined · cross-paradigm** (cyan swatch
  with hatch). Each: value + unit, range line, secondary line, then small tinted badges.
- `impact_join` table: `criterion` 70 · `unit` 62 · `local measured` 108 ·
  `remote estimated` 122 · `split` flex:1 (7px stacked bar, cyan solid + neutral hatch) ·
  `combined min–max` 148. Criteria with no local measurement read **"not measured"** in
  `--color-neutral-600` and "· remote only" in the combined column — never `0`.
- Cross-paradigm note in magenta eyebrow + ink prose.
- Per-model table; `unknown_model` rows in magenta.
- Aside: "What token-only misses" (6 rows), estimation-status histogram,
  `af statusline` stdout preview, methodology block.

---

## 5. Health
`flex:1` main + 300px aside.
- Collector table: `collector` 156 (dot + name) · `version` 50 · `transport` 112 ·
  `events` 58 · `rate` 58 · `rejected` 68 · `last seen` 74 · `emits` flex:1.
  Dot: cyan < 12s idle, neutral-400 < 45s, magenta beyond. Clamp idle at 0 — spans stamped
  at end time can be marginally ahead of `now`.
- Conformance: 2-column grid, `--space-3 --space-6` gap. Each: field name, % at 14px/600,
  `n/total`, a 4px bar (cyan > 90%, neutral 60–90%, magenta below), and a note explaining
  what a low value *means* — several are expected to be low and that must be said inline,
  or the panel reads as six alarms.
- Quarantined lines: `2px` magenta left border, `--space-2` inset; reason in magenta,
  origin right-aligned, raw line in `--color-neutral-700` `pre-wrap`.
- Aside: Ingestion key/values, Watchdog, `af python doctor` rows with status dots.
