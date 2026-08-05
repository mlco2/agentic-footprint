<script lang="ts">
  // Health tab "Quarantined lines" panel (SCREENS.md §5): "2px magenta left
  // border, --space-2 inset; reason in magenta, origin right-aligned, raw
  // line in --color-neutral-700 pre-wrap." Reuses `.rule-quarantine`
  // (console.css's 5th sanctioned rule kind) for the left border rather than
  // a component-local border — the exact rule this file's own doc comment
  // says exists for this.
  import type { RejectedRow } from "../selectors/health";
  import EmptyState from "./EmptyState.svelte";

  let { rows }: { rows: readonly RejectedRow[] } = $props();
</script>

<div class="rejected">
  <div class="rail-section__heading">Quarantined lines</div>
  {#if rows.length === 0}
    <EmptyState label="Quarantined lines" message="no quarantined lines" />
  {:else}
    {#each rows as row, i (`${row.origin}:${row.lineLabel}:${i}`)}
      <div class="rejected-row rule-quarantine">
        <div class="rejected-row__head">
          <span class="rejected-row__reason">{row.reason}</span>
          <span class="rejected-row__origin">{row.origin} · line {row.lineLabel} · byte {row.byteOffsetLabel}</span>
        </div>
        <div class="rejected-row__meta">{row.tsLabel}</div>
        <pre class="rejected-row__raw">{row.raw}</pre>
      </div>
    {/each}
  {/if}
</div>

<style>
  .rejected-row {
    padding: var(--space-2);
    margin-bottom: var(--space-2);
  }
  .rejected-row__head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--space-2);
  }
  .rejected-row__reason {
    font-size: 12.5px;
    color: var(--color-accent-2-700);
  }
  .rejected-row__origin {
    font-size: 11px;
    color: var(--color-neutral-600);
    text-align: right;
    white-space: nowrap;
  }
  .rejected-row__meta {
    font-size: 11px;
    color: var(--color-neutral-600);
    margin-top: 1px;
  }
  .rejected-row__raw {
    margin: var(--space-1) 0 0;
    font-family: var(--font-body);
    font-size: 11.5px;
    color: var(--color-neutral-700);
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
