<script lang="ts">
  // impact_join criteria table (SCREENS.md §4): criterion 70 · unit 62 ·
  // local measured 108 · remote estimated 122 · split flex:1 (7px stacked
  // bar) · combined min–max 148. Column widths and the split bar's cyan
  // solid / neutral hatch fills are the only "layout" this component owns —
  // every string and every fraction is already computed by
  // selectors/impact.ts (`selectCriteriaTable`).
  import type { CriteriaRow } from "../selectors/impact";

  let { rows }: { rows: readonly CriteriaRow[] } = $props();
</script>

<div class="criteria-table">
  <div class="criteria-table__row criteria-table__row--head rule-table-head">
    <span class="col-criterion">criterion</span>
    <span class="col-unit">unit</span>
    <span class="col-local">local measured</span>
    <span class="col-remote">remote estimated</span>
    <span class="col-split">split</span>
    <span class="col-combined">combined min–max</span>
  </div>
  {#each rows as row (row.criterion)}
    <div class="criteria-table__row rule-table-row">
      <span class="col-criterion">{row.criterion}</span>
      <span class="col-unit">{row.unit}</span>
      <span class="col-local" class:status-neutral={!row.localMeasured}>{row.localLabel}</span>
      <span class="col-remote" class:status-neutral={!row.remoteMeasured}>{row.remoteLabel}</span>
      <span class="col-split">
        <span class="split-bar">
          <span class="split-bar__seg split-bar__seg--local" style:width="{row.splitLocalFraction * 100}%"></span>
          <span class="split-bar__seg split-bar__seg--remote hatch" style:width="{row.splitRemoteFraction * 100}%"></span>
        </span>
      </span>
      <span class="col-combined" class:status-neutral={!row.combinedMeasured}>{row.combinedLabel}</span>
    </div>
  {/each}
</div>

<style>
  .criteria-table__row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 4px 0;
    font-size: 12px;
  }
  .criteria-table__row--head {
    font-size: 10.5px;
    letter-spacing: 0.07em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
    padding-bottom: 3px;
  }
  .col-criterion {
    flex: 0 0 70px;
  }
  .col-unit {
    flex: 0 0 62px;
    color: var(--color-neutral-600);
  }
  .col-local {
    flex: 0 0 108px;
  }
  .col-remote {
    flex: 0 0 122px;
  }
  .col-split {
    flex: 1;
    min-width: 40px;
  }
  .col-combined {
    flex: 0 0 148px;
  }
  .split-bar {
    display: flex;
    height: 7px;
    width: 100%;
    background: var(--color-neutral-100);
  }
  .split-bar__seg {
    height: 100%;
    box-sizing: border-box;
  }
  .split-bar__seg--local {
    background: var(--color-accent);
  }
  /* No rule for `.split-bar__seg--remote` on purpose: the shared global
     `.hatch` class (console.css) supplies its `background-image`, and a
     `background: transparent` shorthand here would win the cascade (this
     component's scoped styles are injected after console.css) and silently
     reset that image back to `none` — the exact bug this comment now guards
     against. The segment's un-hatched default (no background at all) is
     already transparent. */
</style>
