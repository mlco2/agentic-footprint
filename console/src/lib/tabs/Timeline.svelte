<script lang="ts">
  // Layout A (rail · chart · inspector) — SCREENS.md §1 recommendation, the
  // only Timeline layout with a decision log (layouts B/C are dropped, no
  // switcher). All data flows through the pure selectors in
  // selectors/timeline.ts, recomputed via $derived.by keyed on
  // eventStore.rev/uiStore.nowMs/uiStore.hiddenTypes/uiStore.selectedId.
  // App.svelte's {#if uiStore.tab === "timeline"} unmounts this component
  // entirely while another tab is active, which is what keeps these
  // selectors from computing while hidden (DATA-CONTRACT §3.5: "only the
  // visible tab's selectors run") — the same discipline Stream.svelte
  // documents for its own selectors.
  //
  // The chart-column flex skeleton (chart `flex:0 1 auto; overflow:auto` ->
  // 16px axis row -> decision log `flex:1 1 auto; min-height:130px;
  // max-height:38%`) is the M1 fix — kept verbatim, built inside rather
  // than around.
  import { eventStore } from "../stores/eventStore.svelte";
  import { allocStore } from "../stores/allocStore.svelte";
  import { uiStore } from "../stores/uiStore.svelte";
  import { selectCorrelated, selectInspector, selectRelevantSampleIds, selectSampleShare, selectSpanEnergy } from "../selectors/inspector";
  import { selectDecisionLog, selectRail, selectTimelineLanes } from "../selectors/timeline";
  import EmptyState from "../components/EmptyState.svelte";
  import LaneChart from "../components/LaneChart.svelte";
  import DecisionLog from "../components/DecisionLog.svelte";
  import CollectorList from "../components/CollectorList.svelte";
  import WatchdogList from "../components/WatchdogList.svelte";
  import Inspector from "../components/Inspector.svelte";

  const lanes = $derived.by(() => selectTimelineLanes(eventStore.rev, uiStore.nowMs, uiStore.hiddenTypes, uiStore.selectedId));
  const decisionRows = $derived.by(() => selectDecisionLog(eventStore.rev));
  const rail = $derived.by(() => selectRail(eventStore.rev, uiStore.nowMs));
  const inspector = $derived.by(() => selectInspector(eventStore.rev, uiStore.selectedId));
  const correlated = $derived.by(() => selectCorrelated(eventStore.rev, uiStore.selectedId));
  const spanEnergy = $derived.by(() => selectSpanEnergy(eventStore.rev, allocStore.rev, uiStore.selectedId));
  const sampleShare = $derived.by(() => selectSampleShare(eventStore.rev, allocStore.rev, uiStore.selectedId));
  const hasAnyEvents = $derived.by(() => {
    void eventStore.rev;
    return eventStore.retained > 0;
  });

  // Task 6: the selector only READS allocStore — this effect is the "tab
  // container triggers fetches" half of that split. Runs whenever the
  // selection (or eventStore.rev, e.g. a new overlapping sample arriving)
  // changes; `allocStore.fetch` itself no-ops for an id already cached or
  // already in flight, so re-running this on every recompute is cheap and
  // safe, never a duplicate network call.
  $effect(() => {
    for (const id of selectRelevantSampleIds(eventStore.rev, uiStore.selectedId)) {
      if (allocStore.get(id) === undefined) void allocStore.fetch(id).catch(() => {});
    }
  });

  function select(id: string): void {
    uiStore.select(id);
  }
</script>

<div class="timeline">
  <aside class="timeline__rail tab-pane">
    <CollectorList collectors={rail.collectors} />

    <div class="rail-section">
      <div class="rail-section__heading">Event types</div>
      <div class="rail-types">
        {#each rail.types as t (t.type)}
          <button type="button" class="chip" class:is-off={uiStore.hiddenTypes.has(t.type)} onclick={() => uiStore.toggleHiddenType(t.type)}>
            <span class="chip__swatch" style:background="var(--type-swatch-{t.type})"></span>
            {t.type}
            <span class="chip__count">{t.count}</span>
          </button>
        {/each}
      </div>
    </div>

    <WatchdogList watchdog={rail.watchdog} orphanSummary={rail.orphanSummary} />
  </aside>

  <section class="timeline__chart rule-ink-l">
    <div class="timeline__chart-title">
      <span class="timeline__chart-title-main">Session timeline</span>
      <span class="timeline__chart-title-meta">{lanes.windowLabel}</span>
      <span class="timeline__chart-title-meta">{lanes.spanCount} spans</span>
      {#if lanes.droppedSpans > 0}
        <span class="timeline__chart-title-meta">+{lanes.droppedSpans} spans not shown</span>
      {/if}
      <div class="timeline__legend">
        <span class="timeline__legend-item"><span class="timeline__legend-swatch" style:background="var(--color-accent)"></span>measured</span>
        <span class="timeline__legend-item"><span class="timeline__legend-swatch hatch"></span>modelled</span>
        <span class="timeline__legend-item"><span class="timeline__legend-swatch hatch-alarm"></span>alarm</span>
      </div>
    </div>
    <div class="timeline__chart-body rule-chart-axis">
      {#if !hasAnyEvents}
        <EmptyState label="Timeline" message="waiting for events — connect a collector" />
      {:else}
        <LaneChart model={lanes} onSelect={select} />
      {/if}
    </div>
    <div class="timeline__axis-row" aria-hidden="true">
      {#each lanes.axisTicks as tick (tick.leftPct)}
        <span class="timeline__axis-tick" style:left="{tick.leftPct}%">{tick.label}</span>
      {/each}
    </div>
    <div class="timeline__decision-log">
      {#if decisionRows.length === 0}
        <EmptyState label="Decision log" message="no decisions logged yet" />
      {:else}
        <DecisionLog rows={decisionRows} onSelect={select} />
      {/if}
    </div>
  </section>

  <aside class="timeline__inspector rule-ink-l tab-pane">
    <Inspector model={inspector} spanEnergy={spanEnergy} sampleShare={sampleShare} correlated={correlated} onSelectCorrelated={select} />
  </aside>
</div>

<style>
  .timeline {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .timeline__rail {
    flex: 0 0 196px;
  }
  .rail-types {
    display: flex;
    flex-direction: column;
    gap: 2px;
    align-items: flex-start;
  }
  .timeline__chart {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .timeline__chart-title {
    flex: 0 0 auto;
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
  }
  .timeline__chart-title-main {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 15px;
  }
  .timeline__chart-title-meta {
    font-size: 11.5px;
    color: var(--color-neutral-600);
    white-space: nowrap;
  }
  .timeline__legend {
    flex: 1;
    display: flex;
    justify-content: flex-end;
    gap: var(--space-3);
  }
  .timeline__legend-item {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    color: var(--color-neutral-600);
    white-space: nowrap;
  }
  .timeline__legend-swatch {
    display: inline-block;
    width: 10px;
    height: 10px;
    background: var(--color-neutral-300);
  }
  .timeline__chart-body {
    /* Chart scroll containers are flex:0 1 auto, never flex:1 (SCREENS.md) —
       otherwise the chart absorbs all leftover height and strands the axis
       below the lanes. min-height approximates the lane stack (llm_call
       22px + action_span 21px + energy_sample 54px + process_sample 34px). */
    flex: 0 1 auto;
    min-height: 130px;
    overflow: auto;
    padding-left: var(--space-2);
  }
  .timeline__axis-row {
    flex: 0 0 16px;
    position: relative;
  }
  .timeline__axis-tick {
    position: absolute;
    top: 0;
    font-size: 10.5px;
    color: var(--color-neutral-600);
    white-space: nowrap;
    transform: translateX(-50%);
  }
  .timeline__axis-tick:last-child {
    transform: translateX(-100%);
  }
  .timeline__decision-log {
    flex: 1 1 auto;
    min-height: 130px;
    max-height: 38%;
    overflow: auto;
    padding: 0 var(--space-3);
  }
  .timeline__inspector {
    flex: 0 0 320px;
  }
</style>
