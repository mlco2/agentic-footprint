<script lang="ts">
  // Timeline rail "Watchdog" section (SCREENS.md §1 Layout A): pid, cmd,
  // cpu/rss, state — plus a magenta italic orphan summary line when any
  // entry is orphaned. DESIGN-SYSTEM §3: magenta is rationed to actual gaps
  // the control plane admits, so the summary line only ever appears when
  // `orphanSummary` is non-null (never a decorative default).
  import type { WatchdogRailRow } from "../selectors/timeline";
  import EmptyState from "./EmptyState.svelte";

  let {
    watchdog,
    orphanSummary,
  }: {
    watchdog: readonly WatchdogRailRow[];
    orphanSummary: string | null;
  } = $props();
</script>

<div class="rail-section">
  <div class="rail-section__heading">Watchdog</div>
  {#if watchdog.length === 0}
    <EmptyState label="Watchdog" message="no watched processes" />
  {:else}
    {#each watchdog as w (w.pid)}
      <div class="watchdog-row">
        <div class="watchdog-row__cmd">{w.cmd}</div>
        <div class="watchdog-row__meta">pid {w.pid} · {w.cpuPctLabel} cpu · {w.rssLabel} · {w.state}</div>
      </div>
    {/each}
  {/if}
  {#if orphanSummary !== null}
    <p class="watchdog__orphan-summary">{orphanSummary}</p>
  {/if}
</div>

<style>
  .watchdog-row {
    padding: 2px 0;
  }
  .watchdog-row__cmd {
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .watchdog-row__meta {
    font-size: 11px;
    color: var(--color-neutral-600);
  }
  .watchdog__orphan-summary {
    margin: var(--space-1) 0 0;
    font-size: 11.5px;
    font-style: italic;
    color: var(--color-accent-2-700);
  }
</style>
