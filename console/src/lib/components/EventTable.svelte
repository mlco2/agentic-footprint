<script lang="ts">
  // Shared table geometry (SCREENS.md "Shared table geometry" + §2 Stream):
  // sticky uppercase header with an ink rule under it, 1px neutral-200 rules
  // between rows, hover/selected fills. Also reused by Timeline C (not built
  // in this task) — kept generic: no store imports, selection and click
  // routing are the caller's job via props.
  import type { StreamRow } from "../selectors/stream";
  import { pressable } from "../actions/pressable";

  let {
    rows,
    selectedId,
    onSelect,
  }: {
    rows: readonly StreamRow[];
    selectedId: string | null;
    onSelect: (id: string) => void;
  } = $props();
</script>

<div class="event-table">
  <div class="event-table__head rule-table-head">
    <span class="col col--ts">ts</span>
    <span class="col col--type">type</span>
    <span class="col col--collector">collector</span>
    <span class="col col--attribution">attribution</span>
    <span class="col col--facts">facts</span>
    <span class="col col--source">source / method</span>
    <span class="col col--status col--right">status</span>
  </div>
  {#each rows as row (row.id)}
    <div
      class="event-table__row rule-table-row"
      class:is-selected={row.id === selectedId}
      use:pressable={() => onSelect(row.id)}
    >
      <span class="col col--ts">{row.ts}</span>
      <span class="col col--type">{row.type}</span>
      <span class="col col--collector">{row.collector}</span>
      <span class="col col--attribution">{row.attribution}</span>
      <span class="col col--facts">{row.facts}</span>
      <span class="col col--source {row.sourceMethodClass}">{row.sourceMethod}</span>
      <span class="col col--status col--right {row.statusClass}">{row.status}</span>
    </div>
  {/each}
</div>

<style>
  .event-table {
    font-size: 12px;
  }
  .event-table__head {
    display: flex;
    gap: var(--space-2);
    padding: 3px 0;
    font-size: 10.5px;
    letter-spacing: 0.07em;
    text-transform: uppercase;
    color: var(--color-neutral-600);
    position: sticky;
    top: 0;
    background: var(--color-bg);
  }
  .event-table__row {
    display: flex;
    gap: var(--space-2);
    padding: 2.5px 0;
    cursor: pointer;
  }
  .event-table__row:hover {
    background: var(--color-neutral-100);
  }
  .event-table__row.is-selected {
    background: var(--color-accent-200);
  }
  .col--ts {
    flex: 0 0 80px;
    color: var(--color-neutral-600);
  }
  .col--type {
    flex: 0 0 104px;
  }
  .col--collector {
    flex: 0 0 118px;
    color: var(--color-neutral-700);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col--attribution {
    flex: 0 0 86px;
    color: var(--color-neutral-600);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col--facts {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col--source {
    flex: 0 0 112px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col--status {
    flex: 0 0 48px;
  }
  .col--right {
    text-align: right;
  }
</style>
