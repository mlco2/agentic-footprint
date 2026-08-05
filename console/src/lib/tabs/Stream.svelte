<script lang="ts">
  // flex:1 table column (chips row above) + 352px inspector aside
  // (SCREENS.md §2). All data flows from stores through the pure selectors
  // in selectors/stream.ts, recomputed via $derived.by keyed on
  // eventStore.rev/uiStore.hiddenTypes/uiStore.selectedId — App.svelte's
  // {#if uiStore.tab === "stream"} unmounts this component entirely while
  // another tab is active, which is what keeps it from computing while
  // hidden (DATA-CONTRACT §3.5: "only the visible tab's selectors run").
  import { eventStore } from "../stores/eventStore.svelte";
  import { allocStore } from "../stores/allocStore.svelte";
  import { uiStore } from "../stores/uiStore.svelte";
  import { selectStreamRows } from "../selectors/stream";
  import { selectCorrelated, selectInspector, selectRelevantSampleIds, selectSampleShare, selectSpanEnergy } from "../selectors/inspector";
  import EmptyState from "../components/EmptyState.svelte";
  import EventTable from "../components/EventTable.svelte";
  import FilterChips from "../components/FilterChips.svelte";
  import Inspector from "../components/Inspector.svelte";

  const streamRows = $derived.by(() => selectStreamRows(eventStore.rev, uiStore.hiddenTypes));
  const inspector = $derived.by(() => selectInspector(eventStore.rev, uiStore.selectedId));
  const correlated = $derived.by(() => selectCorrelated(eventStore.rev, uiStore.selectedId));
  const spanEnergy = $derived.by(() => selectSpanEnergy(eventStore.rev, allocStore.rev, uiStore.selectedId));
  const sampleShare = $derived.by(() => selectSampleShare(eventStore.rev, allocStore.rev, uiStore.selectedId));
  const hasAnyEvents = $derived.by(() => {
    void eventStore.rev; // track the batched revision — `retained` itself is a plain field
    return eventStore.retained > 0;
  });

  // Task 6: mirrors Timeline.svelte's own effect — the selector only READS
  // allocStore, this container triggers the fetch. Both tabs can have the
  // same selection active (uiStore.selectedId is shared), so both run this
  // independently; `allocStore.fetch`'s own in-flight dedup means only one
  // network request ever goes out even if both tabs are mounted... though
  // {#if uiStore.tab === ...} in App.svelte means only one ever is.
  $effect(() => {
    for (const id of selectRelevantSampleIds(eventStore.rev, uiStore.selectedId)) {
      if (allocStore.get(id) === undefined) void allocStore.fetch(id).catch(() => {});
    }
  });

  function select(id: string): void {
    uiStore.select(id);
  }
</script>

<div class="stream">
  <section class="stream__table">
    <FilterChips shown={streamRows.shown} total={streamRows.total} />
    <div class="stream__table-scroll">
      {#if !hasAnyEvents}
        <EmptyState label="Stream" message="no events received yet" />
      {:else if streamRows.rows.length === 0}
        <EmptyState label="Stream" message="every event type is hidden — clear filters to see them" />
      {:else}
        <EventTable rows={streamRows.rows} selectedId={uiStore.selectedId} onSelect={select} />
      {/if}
    </div>
  </section>

  <aside class="stream__inspector rule-ink-l tab-pane">
    <Inspector model={inspector} spanEnergy={spanEnergy} sampleShare={sampleShare} correlated={correlated} onSelectCorrelated={select} />
  </aside>
</div>

<style>
  .stream {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .stream__table {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    padding: var(--space-2) var(--space-3) 0;
    min-height: 0;
  }
  .stream__table-scroll {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  .stream__inspector {
    flex: 0 0 352px;
  }
</style>
