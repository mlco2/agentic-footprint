<script lang="ts">
  // Health tab (Task 9 / M9; SCREENS.md §5): flex:1 main (collector table ->
  // conformance -> quarantined lines) + 300px aside (ingestion KVs ->
  // watchdog -> python doctor), replacing the M-scaffold's EmptyState-only
  // skeleton. `uiStore.nowMs` feeds `selectCollectorTable`'s idle-dot calc —
  // this is the only place this tab reads the clock, and only as a selector
  // ARGUMENT (global-constraints.md #5/#6: selectors never call
  // `Date.now()` themselves).
  import { healthStore } from "../stores/healthStore.svelte";
  import { sessionStore } from "../stores/sessionStore.svelte";
  import { eventStore } from "../stores/eventStore.svelte";
  import { uiStore } from "../stores/uiStore.svelte";
  import { selectCollectorTable, selectConformance, selectHealthAside, selectRejected } from "../selectors/health";
  import EmptyState from "../components/EmptyState.svelte";
  import ConformanceBars from "../components/ConformanceBars.svelte";
  import RejectedList from "../components/RejectedList.svelte";
  import WatchdogList from "../components/WatchdogList.svelte";

  const health = $derived(healthStore.data ?? undefined);
  const session = $derived(sessionStore.data);

  const collectors = $derived.by(() => selectCollectorTable(health, uiStore.nowMs));
  const conformance = $derived.by(() => selectConformance(health));
  const rejected = $derived.by(() => selectRejected(health));
  const aside = $derived.by(() => selectHealthAside(health, eventStore.rev, session));
</script>

<div class="health">
  <section class="health__main tab-pane">
    <div class="health__region">
      <div class="rail-section__heading">Collectors</div>
      {#if collectors.length === 0}
        <EmptyState label="Collectors" message="no collectors reporting yet" />
      {:else}
        <div class="collector-table">
          <div class="collector-table__head rule-table-head">
            <span class="col col--collector">collector</span>
            <span class="col col--version">version</span>
            <span class="col col--transport">transport</span>
            <span class="col col--events col--right">events</span>
            <span class="col col--rate col--right">rate</span>
            <span class="col col--rejected col--right">rejected</span>
            <span class="col col--lastseen col--right">last seen</span>
            <span class="col col--emits">emits</span>
          </div>
          {#each collectors as c (c.name)}
            <div class="collector-table__row rule-table-row">
              <span class="col col--collector">
                <span class="status-dot {c.dotClass}" aria-hidden="true"></span>
                {c.name}
              </span>
              <span class="col col--version">{c.version}</span>
              <span class="col col--transport">{c.transport}</span>
              <span class="col col--events col--right">{c.events}</span>
              <span class="col col--rate col--right">{c.rateLabel}</span>
              <span class="col col--rejected col--right">{c.rejected}</span>
              <span class="col col--lastseen col--right">{c.lastSeenLabel}</span>
              <span class="col col--emits">{c.emitsLabel}</span>
            </div>
          {/each}
        </div>
      {/if}
    </div>

    <div class="health__region">
      <ConformanceBars model={conformance} />
    </div>

    <div class="health__region">
      <RejectedList rows={rejected} />
    </div>
  </section>

  <aside class="health__aside rule-ink-l tab-pane">
    <div class="rail-section">
      <div class="rail-section__heading">Ingestion</div>
      {#if aside.ingestion.length === 0}
        <EmptyState label="Ingestion" message="no ingestion data yet" />
      {:else}
        <div class="ingestion-list">
          {#each aside.ingestion as row (row.label)}
            <div class="ingestion-row rule-table-row">
              <span class="ingestion-row__label">{row.label}</span>
              <span class="ingestion-row__value">{row.value}</span>
            </div>
          {/each}
        </div>
      {/if}
    </div>

    <WatchdogList watchdog={aside.watchdog} orphanSummary={aside.orphanSummary} />

    <div class="rail-section">
      <div class="rail-section__heading">af python doctor</div>
      {#if aside.doctor.length === 0}
        <EmptyState label="Python doctor" message="no doctor rows yet" />
      {:else}
        {#each aside.doctor as row (row.key)}
          <div class="doctor-row">
            <span class="status-dot {row.dotClass}" aria-hidden="true"></span>
            <span class="doctor-row__key">{row.key}</span>
            <span class="doctor-row__value">{row.value}</span>
          </div>
        {/each}
      {/if}
    </div>
  </aside>
</div>

<style>
  .health {
    flex: 1;
    min-width: 0;
    display: flex;
  }
  .health__main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .health__aside {
    flex: 0 0 300px;
  }
  .health__region {
    min-width: 0;
  }

  /* — collector table (SCREENS.md §5 geometry) — */
  .collector-table {
    font-size: 12px;
  }
  .collector-table__head {
    display: flex;
    gap: var(--space-2);
    padding: 3px 0;
    font-size: 10.5px;
    letter-spacing: 0.07em;
    text-transform: uppercase;
    color: var(--color-neutral-600);
  }
  .collector-table__row {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    padding: 3px 0;
  }
  .col--collector {
    flex: 0 0 156px;
    display: flex;
    align-items: baseline;
    gap: var(--space-1);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col--version {
    flex: 0 0 50px;
    color: var(--color-neutral-600);
  }
  .col--transport {
    flex: 0 0 112px;
    color: var(--color-neutral-700);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col--events {
    flex: 0 0 58px;
  }
  .col--rate {
    flex: 0 0 58px;
    color: var(--color-neutral-600);
  }
  .col--rejected {
    flex: 0 0 68px;
  }
  .col--lastseen {
    flex: 0 0 74px;
    color: var(--color-neutral-600);
  }
  .col--emits {
    flex: 1;
    min-width: 0;
    color: var(--color-neutral-600);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col--right {
    text-align: right;
  }

  /* — aside: ingestion KVs — (border comes from the shared .rule-table-row
     class in console.css, not redeclared here — global-constraints.md #2's
     "only the five sanctioned rule kinds") */
  .ingestion-row {
    display: flex;
    flex-direction: column;
    padding: 3px 0;
    font-size: 11.5px;
  }
  .ingestion-row__label {
    color: var(--color-neutral-600);
    font-size: 10.5px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .ingestion-row__value {
    color: var(--color-text);
    overflow-wrap: break-word;
  }

  /* — aside: python doctor — */
  .doctor-row {
    display: flex;
    align-items: baseline;
    gap: var(--space-1);
    padding: 2px 0;
    font-size: 12px;
  }
  .doctor-row__key {
    color: var(--color-neutral-700);
    flex: 0 0 auto;
  }
  .doctor-row__value {
    color: var(--color-neutral-600);
    font-size: 11px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
