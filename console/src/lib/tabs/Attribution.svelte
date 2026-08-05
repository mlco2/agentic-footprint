<script lang="ts">
  // 232px sample list · flex:1 detail · 274px policy aside (SCREENS.md §3).
  // All data flows from selectors/attribution.ts, recomputed via
  // `$derived.by` keyed on eventStore.rev/allocStore.rev/uiStore.selectedId —
  // App.svelte's `{#if uiStore.tab === "attribution"}` unmounts this
  // component entirely while another tab is active (DATA-CONTRACT §3.5:
  // "only the visible tab's selectors run").
  //
  // Selecting a sample sets `uiStore.selectedId` to the sample's own
  // `event_id` — the same shared-selection field Timeline/Stream use, so an
  // energy_sample bar clicked elsewhere lands here coherently (brief).
  import { eventStore } from "../stores/eventStore.svelte";
  import { allocStore } from "../stores/allocStore.svelte";
  import { sessionStore } from "../stores/sessionStore.svelte";
  import { uiStore } from "../stores/uiStore.svelte";
  import { STRIP_ROW_HEIGHT_PX, selectAllocationDetail, selectPolicyAside, selectSampleList } from "../selectors/attribution";
  import EmptyState from "../components/EmptyState.svelte";
  import AllocationTable from "../components/AllocationTable.svelte";
  import { pressable } from "../actions/pressable";

  // Sample list scoped to the masthead's session picker — attribution is a
  // per-session computation. Detail/aside follow the *selected sample*
  // (which came from this scoped list), so they need no extra scoping.
  const sampleList = $derived.by(() => selectSampleList(eventStore.rev, allocStore.rev, uiStore.selectedId, sessionStore.selectedId));
  const detail = $derived.by(() => selectAllocationDetail(eventStore.rev, allocStore.rev, uiStore.selectedId));
  const aside = $derived.by(() => selectPolicyAside(eventStore.rev, allocStore.rev, uiStore.selectedId));

  // Every listed sample's trace must be fetched (unlike Timeline/Stream,
  // which only fetch traces relevant to the CURRENT selection) — the sample
  // list itself shows "N spans · idle N%" for every row, so every row's
  // trace is "visible" in the brief's sense. The selector only reads
  // allocStore; this container triggers the fetch (same split Timeline.svelte
  // and Stream.svelte already use for their own allocStore reads).
  $effect(() => {
    for (const row of sampleList) {
      if (allocStore.get(row.sampleEventId) === undefined) void allocStore.fetch(row.sampleEventId).catch(() => {});
    }
  });

  function select(id: string): void {
    uiStore.select(id);
  }

  function pendingMessage(status: "pending" | "unavailable"): string {
    return status === "pending" ? "trace pending" : "trace unavailable (outside window)";
  }
</script>

<div class="attribution">
  <aside class="attribution__samples tab-pane">
    {#if sampleList.length === 0}
      <EmptyState label="Samples" message="no samples reported yet" />
    {:else}
      <ul class="sample-list">
        {#each sampleList as row (row.sampleEventId)}
          <li>
            <div
              class="sample-row"
              class:is-selected={row.selected}
              use:pressable={() => select(row.sampleEventId)}
            >
              <div class="sample-row__interval">{row.intervalLabel}</div>
              {#if row.status === "ready"}
                <div class="sample-row__total">{row.totalLabel}</div>
                <div class="sample-row__meta">
                  <span>{row.metaLabel}</span>
                  {#if row.l1FlagLabel}
                    <span class={row.l1FlagClass}>{row.l1FlagLabel}</span>
                  {/if}
                </div>
              {:else}
                <div class="sample-row__pending">{row.pendingLabel}</div>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </aside>

  <section class="attribution__detail rule-ink-l tab-pane">
    {#if uiStore.selectedId === null}
      <EmptyState label="Attribution" message="no energy_sample selected — pick one from the sample list" />
    {:else if detail === null}
      <EmptyState label="Attribution" message="select an energy_sample (not a span) to see its allocation trace" />
    {:else if detail.status !== "ready"}
      <EmptyState label="Attribution" message={pendingMessage(detail.status)} />
    {:else}
      <div class="detail">
        <div class="detail__eyebrow">energy_sample</div>
        <div class="detail__title">{detail.intervalLabel}</div>
        <div class="detail__sub">{detail.sampleEventId}</div>

        <div class="stat-grid">
          {#each detail.stats ?? [] as stat (stat.label)}
            <div class="stat-tile">
              <div class="stat-tile__label">{stat.label}</div>
              <div class="stat-tile__value" class:status-alarm={stat.tone === "alarm"}>{stat.value}</div>
            </div>
          {/each}
        </div>

        <div class="detail__section-heading">Sample interval</div>
        <div class="interval-strip" style:height="{Math.max(STRIP_ROW_HEIGHT_PX, (detail.intervalStrip ?? []).length * STRIP_ROW_HEIGHT_PX)}px">
          {#each detail.intervalStrip ?? [] as strip (strip.spanId)}
            <div
              class="interval-strip__row"
              class:hatch={strip.hatch === "neutral"}
              style:top="{strip.topPx}px"
              style:height="{STRIP_ROW_HEIGHT_PX}px"
              style:left="{strip.leftPct}%"
              style:width="{strip.widthPct}%"
              style:background={strip.fillVar}
              title={strip.title}
            >
              <span class="interval-strip__label" class:interval-strip__label--ink={strip.hatch === "neutral"}>{strip.label}</span>
            </div>
          {/each}
        </div>

        <div class="detail__section-heading">Components</div>
        <div class="component-table">
          {#each detail.components ?? [] as comp (comp.kind + comp.label)}
            <div class="component-row">
              <span class="component-row__swatch" class:hatch={comp.hatched}></span>
              <span class="component-row__label">{comp.label}</span>
              <span class="component-row__method">{comp.method}</span>
              <span class="component-row__joules">{comp.jouleLabel}</span>
            </div>
          {/each}
        </div>

        <div class="detail__section-heading">Allocation</div>
        <AllocationTable rows={detail.allocationRows ?? []} />

        {#if (detail.notes ?? []).length > 0}
          <div class="detail__section-heading">Notes</div>
          <ul class="notes">
            {#each detail.notes ?? [] as note, i (i)}
              <li class:status-alarm={note.tone === "alarm"}>{note.text}</li>
            {/each}
          </ul>
        {/if}
      </div>
    {/if}
  </section>

  <aside class="attribution__policy rule-ink-l tab-pane">
    {#if uiStore.selectedId === null}
      <EmptyState label="Policy" message="no attribution policy loaded yet" />
    {:else if aside === null}
      <EmptyState label="Policy" message="select an energy_sample to see its attribution policy" />
    {:else if aside.status !== "ready"}
      <EmptyState label="Policy" message={pendingMessage(aside.status)} />
    {:else}
      <div class="policy">
        <div class="detail__eyebrow">Policy · {aside.policyId}</div>
        {#each aside.policyProse ?? [] as line, i (i)}
          <p class="policy__prose">{line}</p>
        {/each}

        <div class="detail__section-heading">Formula</div>
        {#each aside.formulaLines ?? [] as line, i (i)}
          <p class="policy__formula">{line}</p>
        {/each}
        <p class="policy__formula-sub">
          denominator_cpu_ms = {aside.denominatorLabel} · total_j = {aside.totalJLabel}
        </p>
        {#if aside.denominatorNote}
          <p class="policy__note">{aside.denominatorNote}</p>
        {/if}
        {#if aside.formulaSubstitution}
          <p class="policy__formula-substitution-label">{aside.formulaSubstitution.label}</p>
          <p class="policy__formula-substitution">{aside.formulaSubstitution.shareLine}</p>
          <p class="policy__formula-substitution">{aside.formulaSubstitution.allocLine}</p>
        {/if}

        <div class="detail__section-heading">process_sample cpu Δ</div>
        <div class="formula-rows">
          {#each aside.formulaRows ?? [] as row (row.key)}
            <div class="formula-row">
              <span class="formula-row__label">{row.label}</span>
              <span class="formula-row__cpu">{row.cpuDeltaLabel}</span>
              <span class="formula-row__share">{row.shareLabel}</span>
              <span class="formula-row__alloc">{row.allocatedLabel}</span>
            </div>
          {/each}
        </div>

        <div class="detail__section-heading">Grid intensity</div>
        <p class="policy__grid">{aside.gridZone ?? "—"} · {aside.gridIntensityLabel ?? "—"}</p>
        <p class="policy__geo">{aside.geoNoteLabel}</p>
      </div>
    {/if}
  </aside>
</div>

<style>
  .attribution {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .attribution__samples {
    flex: 0 0 232px;
  }
  .attribution__detail {
    flex: 1;
    min-width: 0;
  }
  .attribution__policy {
    flex: 0 0 274px;
  }

  /* — sample list — */
  .sample-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .sample-row {
    cursor: pointer;
    padding: 4px var(--space-2) 4px 6px;
    border-left: 2px solid transparent;
  }
  .sample-row:hover {
    background: var(--color-neutral-100);
  }
  .sample-row.is-selected {
    background: var(--color-accent-200);
    border-left-color: var(--color-accent);
  }
  .sample-row__interval {
    font-size: 11px;
    color: var(--color-neutral-600);
  }
  .sample-row__total {
    font-size: 13px;
    font-weight: 600;
    color: var(--color-accent-700);
  }
  .sample-row__meta {
    display: flex;
    gap: var(--space-1);
    font-size: 11px;
    color: var(--color-neutral-700);
  }
  .sample-row__pending {
    font-size: 12px;
    font-style: italic;
    color: var(--color-neutral-600);
  }

  /* — detail column — */
  .detail__eyebrow {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 11px;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
  }
  .detail__title {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 19px;
    margin: 2px 0 1px;
  }
  .detail__sub {
    font-size: 11.5px;
    color: var(--color-neutral-600);
    font-style: italic;
    margin-bottom: var(--space-2);
  }
  .detail__section-heading {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 11px;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
    margin: var(--space-3) 0 3px;
  }

  .stat-grid {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-4) var(--space-4);
    margin-top: var(--space-2);
  }
  .stat-tile__label {
    font-size: 11px;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
  }
  .stat-tile__value {
    font-size: 17px;
    font-weight: 600;
  }

  /* One 19px row per span (SCREENS.md §3), stacked by `strip.topPx` — height
     grows with the row count instead of a single shared 19px band, so
     overlapping spans never occlude each other. */
  .interval-strip {
    position: relative;
    border-left: 1px solid var(--color-text);
    border-right: 1px solid var(--color-text);
    background: var(--color-neutral-100);
  }
  .interval-strip__row {
    position: absolute;
    box-sizing: border-box;
    border-bottom: 1px solid var(--color-neutral-200);
    border-right: 1px solid var(--color-neutral-200);
    overflow: hidden;
  }
  .interval-strip__row.hatch {
    background-image: repeating-linear-gradient(45deg, var(--color-neutral-700) 0 1.5px, transparent 1.5px 4px);
  }
  .interval-strip__label {
    font-size: 10.5px;
    /* Solid fill (`--color-accent`, dark cyan) gets bg-tinted (near-white)
       text for contrast; a transparent/hatched row (excluded/remote spans —
       the DESIGN-SYSTEM §3 measured/modelled axis) sits on the strip's own
       light neutral-100 backdrop, so its label needs ink text instead — the
       same near-white-on-near-white pairing that would otherwise make
       exactly the excluded rows this tab exists to surface illegible. */
    color: var(--color-bg);
    padding: 0 3px;
    white-space: nowrap;
  }
  .interval-strip__label--ink {
    color: var(--color-text);
  }

  .component-table {
    font-size: 12px;
  }
  .component-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 2px 0;
    border-bottom: 1px solid var(--color-neutral-200);
  }
  .component-row__swatch {
    width: 8px;
    height: 8px;
    flex: 0 0 8px;
    background: var(--color-accent);
  }
  .component-row__swatch.hatch {
    background: transparent;
    background-image: repeating-linear-gradient(45deg, var(--color-neutral-700) 0 1px, transparent 1px 3px);
  }
  .component-row__label {
    flex: 1;
    min-width: 0;
  }
  .component-row__method {
    flex: 0 0 90px;
    color: var(--color-neutral-600);
  }
  .component-row__joules {
    flex: 0 0 70px;
    text-align: right;
  }

  .notes {
    margin: 0;
    padding-left: 1.1em;
    font-size: 12px;
  }
  .notes li {
    margin-bottom: 3px;
  }

  /* — policy aside — */
  .policy__prose,
  .policy__formula-sub,
  .policy__grid {
    font-size: 12px;
    margin: 0 0 var(--space-2);
  }
  .policy__formula {
    font-size: 12.5px;
    font-weight: 600;
    margin: 0 0 2px;
  }
  .policy__note {
    font-size: 11px;
    font-style: italic;
    color: var(--color-neutral-600);
    margin: 2px 0 var(--space-2);
  }
  .policy__formula-substitution-label {
    font-size: 11px;
    color: var(--color-neutral-700);
    margin: var(--space-2) 0 1px;
  }
  .policy__formula-substitution {
    font-size: 12px;
    color: var(--color-accent-700);
    margin: 0 0 2px;
  }
  .policy__geo {
    font-size: 11px;
    color: var(--color-accent-700);
    margin: 0;
  }
  .formula-rows {
    font-size: 11.5px;
  }
  .formula-row {
    display: flex;
    gap: var(--space-1);
    padding: 2px 0;
    border-bottom: 1px solid var(--color-neutral-200);
  }
  .formula-row__label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .formula-row__cpu,
  .formula-row__share,
  .formula-row__alloc {
    flex: 0 0 52px;
    text-align: right;
    color: var(--color-neutral-600);
  }
</style>
