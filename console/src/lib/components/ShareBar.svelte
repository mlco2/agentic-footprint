<script lang="ts">
  // Generic segmented proportional bar (Task 6 brief; Task 7's Attribution
  // allocation table reuses this same component, so its props stay exactly
  // `{ segments }` — no Inspector-specific or Attribution-specific prop
  // creeps in here). Each segment's own `fraction` sets its width directly
  // (not `flex-grow`), so segments never stretch to fill the track — when a
  // caller's fractions don't sum to 1 (e.g. the per-span focused bar only
  // names "this span"/agent/baseline and leaves other overlapping spans'
  // shares out of the picture on purpose), the remainder is simply blank
  // track, never a relabelled/misattributed segment.
  //
  // Fill vocabulary is DESIGN-SYSTEM §3's share-bar mapping: solid accent =
  // attributed/measured, accent-300 = the agent's own process, hatched
  // neutral = baseline/idle (also reused for an excluded/remote row — see
  // selectors/inspector.ts's `rowFill`). No raw hex — every colour is a
  // Broadsheet CSS variable.
  import type { ShareBarSegment } from "../selectors/inspector";
  import { fmtJoules, fmtPct } from "../format";

  // Task 7: `variant="compact"` renders the track only (no per-segment
  // legend line) — the Attribution allocation table's "l2 share" column
  // (SCREENS.md §3: 122px "bar + %") sits inside a table row that already
  // shows the span/joules columns beside it, so a full label · joules · %
  // legend underneath would repeat information the row already states.
  // Default stays `"full"` so every existing caller (Inspector's per-sample
  // and per-span bars) renders byte-for-byte as before.
  let { segments, variant = "full" }: { segments: readonly ShareBarSegment[]; variant?: "full" | "compact" } = $props();
</script>

<div class="sharebar" class:sharebar--compact={variant === "compact"}>
  <div class="sharebar__track">
    {#each segments as seg, i (`${seg.label}:${i}`)}
      <div
        class="sharebar__seg"
        class:sharebar__seg--accent={seg.fill === "accent"}
        class:sharebar__seg--accent300={seg.fill === "accent300"}
        class:sharebar__seg--hatch={seg.fill === "neutral-hatch"}
        style:width="{Math.max(0, seg.fraction * 100)}%"
        title={seg.title}
      ></div>
    {/each}
  </div>
  {#if variant === "full"}
    <div class="sharebar__legend">
      {#each segments as seg, i (`${seg.label}:${i}`)}
        <span class="sharebar__legend-item" title={seg.title}>
          <span
            class="sharebar__swatch"
            class:sharebar__seg--accent={seg.fill === "accent"}
            class:sharebar__seg--accent300={seg.fill === "accent300"}
            class:sharebar__seg--hatch={seg.fill === "neutral-hatch"}
          ></span>
          {seg.label} · {fmtJoules(seg.value_j)} · {fmtPct(seg.fraction)}
        </span>
      {/each}
    </div>
  {/if}
</div>

<style>
  .sharebar {
    margin: 3px 0 5px;
  }
  .sharebar--compact {
    margin: 0;
    width: 100%;
  }
  .sharebar__track {
    display: flex;
    height: 10px;
    background: var(--color-neutral-100);
  }
  .sharebar__seg {
    height: 100%;
    box-sizing: border-box;
  }
  .sharebar__seg--accent {
    background: var(--color-accent);
  }
  .sharebar__seg--accent300 {
    background: var(--color-accent-300);
  }
  .sharebar__seg--hatch {
    background-image: repeating-linear-gradient(45deg, var(--color-neutral-700) 0 1.5px, transparent 1.5px 4px);
  }
  .sharebar__legend {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin-top: 2px;
  }
  .sharebar__legend-item {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: var(--color-neutral-600);
  }
  .sharebar__swatch {
    display: inline-block;
    width: 8px;
    height: 8px;
    flex: 0 0 8px;
  }
</style>
