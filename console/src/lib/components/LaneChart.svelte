<script lang="ts">
  // DATA-CONTRACT §3.6: "flat absolutely-positioned bar list + lane labels +
  // axis — not nested per-lane loops". All geometry (left%/width%/topPx/
  // heightPx/fill/hatch/border) is data computed by selectors/timeline.ts;
  // this component only maps `LaneModel` onto markup with ONE loop over
  // `model.bars` (plus one small loop over the separate `laneLabels` array,
  // which is a distinct concern per the brief, not a second per-lane pass
  // over bars). 116px right-aligned label gutter + a plot area with the
  // 1px ink left axis rule (`rule-chart-axis`, DESIGN-SYSTEM §5).
  import type { LaneModel } from "../selectors/timeline";
  import { pressable } from "../actions/pressable";

  let {
    model,
    onSelect,
  }: {
    model: LaneModel;
    onSelect: (id: string) => void;
  } = $props();

  /** One inline style string per bar — kept as a single `style` attribute
   * (not a pile of `style:` directives) so the clickable/non-clickable
   * branches below don't have to repeat every geometry binding. */
  function barStyle(bar: LaneModel["bars"][number]): string {
    const shadow = bar.selected ? "0 0 0 2px var(--color-text)" : "none";
    return `left:${bar.leftPct}%; width:${bar.widthPct}%; top:${bar.topPx}px; height:${bar.heightPx}px; background:${bar.fillVar}; border:1px solid ${bar.borderVar}; box-shadow:${shadow};`;
  }

</script>

<div class="lanechart">
  <div class="lanechart__labels" style:height="{model.plotHeightPx}px">
    {#each model.laneLabels as l (l.label)}
      <div class="lanechart__label" style:top="{l.topPx}px">{l.label}</div>
    {/each}
  </div>
  <div class="lanechart__plot rule-chart-axis" style:height="{model.plotHeightPx}px">
    {#each model.bars as bar, i (`${bar.kind}:${bar.id || i}`)}
      {#if bar.id}
        <div
          class="lanechart__bar lanechart__bar--clickable"
          class:hatch={bar.hatch === "neutral"}
          class:hatch-alarm={bar.hatch === "alarm"}
          title={bar.title}
          style={barStyle(bar)}
          use:pressable={() => onSelect(bar.id)}
        ></div>
      {:else}
        <div class="lanechart__bar" class:hatch={bar.hatch === "neutral"} class:hatch-alarm={bar.hatch === "alarm"} title={bar.title} style={barStyle(bar)}></div>
      {/if}
    {/each}
  </div>
</div>

<style>
  .lanechart {
    display: flex;
    align-items: flex-start;
  }
  .lanechart__labels {
    flex: 0 0 116px;
    position: relative;
    text-align: right;
    padding-right: var(--space-2);
  }
  .lanechart__label {
    position: absolute;
    right: var(--space-2);
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 11.5px;
    color: var(--color-neutral-700);
    white-space: nowrap;
  }
  .lanechart__plot {
    flex: 1;
    min-width: 0;
    position: relative;
  }
  .lanechart__bar {
    position: absolute;
    box-sizing: border-box;
    /* Bars with no backing record (`id === ""`, e.g. a coverage-gap band)
       must never intercept a click meant for the bar underneath/behind them
       (gap bands render full plot height, first in the array = painted
       behind — but pointer-events still follows DOM order for overlapping
       siblings without this). */
    pointer-events: none;
  }
  .lanechart__bar--clickable {
    pointer-events: auto;
    cursor: pointer;
  }
</style>
