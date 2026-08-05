<script lang="ts">
  // The Impact tab's three headline figures (SCREENS.md §4: "--space-8
  // gaps, 33px/600"). Purely presentational — every string here is already
  // formatted by selectors/impact.ts; this component only lays it out and
  // picks the swatch treatment DESIGN-SYSTEM §3 specifies (solid cyan =
  // local measured, ink hatch = remote modelled, cyan+hatch = combined
  // cross-paradigm).
  import type { ImpactCardModel } from "../selectors/impact";

  let { cards }: { cards: readonly ImpactCardModel[] } = $props();
</script>

<div class="impact-cards">
  {#each cards as card (card.key)}
    <div class="impact-card">
      <div class="impact-card__eyebrow">
        <span class="impact-card__swatch impact-card__swatch--{card.swatch}"></span>
        {card.eyebrow}
      </div>
      <div class="impact-card__value" class:status-neutral={!card.measured}>{card.valueLabel}</div>
      {#if card.rangeLine}
        <div class="impact-card__range">{card.rangeLine}</div>
      {/if}
      {#if card.secondaryLine}
        <div class="impact-card__secondary">{card.secondaryLine}</div>
      {/if}
      {#if card.badges.length > 0}
        <div class="impact-card__badges">
          {#each card.badges as badge, i (i)}
            <span class="impact-card__badge" class:status-alarm={badge.tone === "alarm"}>{badge.label}</span>
          {/each}
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .impact-cards {
    display: flex;
    gap: var(--space-8);
    flex-wrap: wrap;
  }
  .impact-card {
    min-width: 0;
  }
  .impact-card__eyebrow {
    display: flex;
    align-items: center;
    gap: 6px;
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 11px;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
    margin-bottom: 4px;
  }
  .impact-card__swatch {
    width: 10px;
    height: 10px;
    flex: 0 0 10px;
    box-sizing: border-box;
  }
  .impact-card__swatch--local {
    background: var(--color-accent);
  }
  .impact-card__swatch--remote {
    background: transparent;
    border: 1px solid var(--color-neutral-700);
    background-image: repeating-linear-gradient(45deg, var(--color-neutral-700) 0 1.5px, transparent 1.5px 4px);
  }
  .impact-card__swatch--combined {
    background: var(--color-accent-300);
    background-image: repeating-linear-gradient(45deg, var(--color-accent-700) 0 1.5px, transparent 1.5px 4px);
  }
  .impact-card__value {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 33px;
    letter-spacing: -0.025em;
    line-height: 1.1;
  }
  .impact-card__range,
  .impact-card__secondary {
    font-size: 12px;
    color: var(--color-neutral-700);
    margin-top: 2px;
  }
  .impact-card__badges {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: var(--space-1);
  }
  .impact-card__badge {
    font-size: 11px;
    padding: 1px 6px;
    background: var(--color-neutral-100);
  }
  /* Neutral tone scoped to NON-alarm badges only — same specificity as a
     bare `.impact-card__badge`, so it never outranks the global
     `.status-alarm` utility (console.css) that `class:status-alarm` toggles
     on an alarm badge. A plain `color` on `.impact-card__badge` itself would
     win that fight regardless of `.status-alarm`'s presence (Svelte injects
     component styles after the global stylesheet), which is the exact bug
     this rule replaces — the same class of cascade bug the CriteriaTable
     split-bar hatch fix already documents. */
  .impact-card__badge:not(.status-alarm) {
    color: var(--color-neutral-700);
  }
</style>
