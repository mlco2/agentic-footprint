<script lang="ts">
  // Health tab "Conformance" panel (SCREENS.md §5; gap #9, DEFERRED BY
  // DECISION). Renders whichever branch `selectConformance` produced: the
  // M1 empty-state pending text when `health.conformance` is absent (the
  // mock's case, and the real server's — docs/design-log.md), or a 2-column
  // grid of bars when it's present. Both branches are real, tested paths —
  // neither is a fallback for the other.
  import type { ConformanceModel } from "../selectors/health";
  import EmptyState from "./EmptyState.svelte";

  let { model }: { model: ConformanceModel } = $props();
</script>

<div class="conformance">
  <div class="rail-section__heading">Conformance</div>
  {#if model.kind === "pending"}
    <EmptyState label="Conformance" message="conformance counters: pending team decision" />
  {:else}
    <div class="conformance__grid">
      {#each model.rows as row (row.field)}
        <div class="conformance-row">
          <div class="conformance-row__head">
            <span class="conformance-row__field">{row.field}</span>
            <span class="conformance-row__pct">{row.pctLabel}</span>
          </div>
          <div class="conformance-row__fraction">{row.fractionLabel}</div>
          <div class="conformance-row__track">
            <div class="conformance-row__fill {row.colorClass}" style:width="{row.barPct}%"></div>
          </div>
          {#if row.note}
            <p class="conformance-row__note">{row.note}</p>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .conformance__grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-3) var(--space-6);
  }
  .conformance-row__head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-2);
  }
  .conformance-row__field {
    font-size: 12.5px;
    color: var(--color-text);
  }
  .conformance-row__pct {
    font-size: 14px;
    font-weight: 600;
  }
  .conformance-row__fraction {
    font-size: 11px;
    color: var(--color-neutral-600);
    margin-top: 1px;
  }
  .conformance-row__track {
    height: 4px;
    background: var(--color-neutral-200);
    margin-top: var(--space-1);
  }
  .conformance-row__fill {
    height: 100%;
  }
  .conformance-row__note {
    margin: 3px 0 0;
    font-size: 11.5px;
    color: var(--color-neutral-700);
  }
</style>
