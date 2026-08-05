<script lang="ts">
  // SCREENS.md §2's inspector: eyebrow, 17px/600 title, italic sub, key/value
  // rows (106px key col), share bars when a span or sample is selected
  // (SCREENS.md §1 Layout A), a Correlated section (each row clickable ->
  // selectedId), raw JSONL via JsonPane. Renders the M1 empty state when
  // nothing is selected.
  //
  // Task 6: `spanEnergy`/`sampleShare` are separate props, not folded into
  // `model` — both are built from `allocStore` too (not just `eventStore`
  // like `model`), so the tab container computes them via their own
  // selectors (`selectSpanEnergy`/`selectSampleShare`) keyed on
  // `allocStore.rev` as well. This component only ever READS them; fetching
  // missing traces is the container's job (see Timeline.svelte/Stream.svelte).
  import type { CorrelatedRow, InspectorModel, SampleShareModel, SpanEnergyModel } from "../selectors/inspector";
  import EmptyState from "./EmptyState.svelte";
  import JsonPane from "./JsonPane.svelte";
  import ShareBar from "./ShareBar.svelte";
  import { pressable } from "../actions/pressable";

  let {
    model,
    spanEnergy = null,
    sampleShare = null,
    correlated,
    onSelectCorrelated,
  }: {
    model: InspectorModel | null;
    spanEnergy?: SpanEnergyModel | null;
    sampleShare?: SampleShareModel | null;
    correlated: readonly CorrelatedRow[];
    onSelectCorrelated: (id: string) => void;
  } = $props();

  /** DATA-CONTRACT §4: "trace pending" / "trace unavailable (outside
   * window)" honest text — never a share bar, never 0 — for a sample the
   * container hasn't fetched yet (or the server 404'd as out-of-window). */
  function pendingLabel(status: "pending" | "unavailable"): string {
    return status === "pending" ? "trace pending" : "trace unavailable (outside window)";
  }
</script>

{#if model === null}
  <EmptyState label="Inspector" message="no record selected" />
{:else}
  <div class="inspector">
    <div class="inspector__eyebrow">{model.eyebrow}</div>
    <div class="inspector__title">{model.title}</div>
    <div class="inspector__sub">{model.sub}</div>

    {#each model.rows as row (row.key)}
      <div class="inspector__row">
        <span class="inspector__key">{row.key}</span>
        <span class="inspector__value" class:kv-tone-alarm={row.tone === "alarm"} class:kv-tone-modelled={row.tone === "modelled"}>
          {row.value}
        </span>
      </div>
    {/each}

    {#if spanEnergy !== null}
      <div class="inspector__row">
        <span class="inspector__key">energy · l2_cpu_time</span>
        <span class="inspector__value">{spanEnergy.totalLabel}</span>
      </div>
      {#each spanEnergy.samples as sample (sample.sampleEventId)}
        <div class="inspector__sharebar-block">
          <div class="inspector__sharebar-caption">{sample.label}</div>
          {#if sample.status === "ready" && sample.noRowNote}
            <p class="inspector__empty-note">{sample.noRowNote}</p>
          {:else if sample.status === "ready"}
            <ShareBar segments={sample.segments ?? []} />
          {:else}
            <p class="inspector__empty-note">{pendingLabel(sample.status)}</p>
          {/if}
        </div>
      {/each}
    {/if}

    {#if sampleShare !== null}
      <div class="inspector__sharebar-block">
        {#if sampleShare.status === "ready"}
          <ShareBar segments={sampleShare.segments ?? []} />
        {:else}
          <p class="inspector__empty-note">{pendingLabel(sampleShare.status)}</p>
        {/if}
      </div>
    {/if}

    <div class="inspector__section-heading">Correlated</div>
    {#if correlated.length === 0}
      <p class="inspector__empty-note">nothing else within ±6s</p>
    {:else}
      {#each correlated as row (row.id)}
        <div class="inspector__correlated-row" use:pressable={() => onSelectCorrelated(row.id)}>
          <span class="inspector__correlated-type">{row.type}</span>
          <span class="inspector__correlated-summary">{row.summary}</span>
          <span class="inspector__correlated-offset">{row.offsetLabel}</span>
        </div>
      {/each}
    {/if}

    <JsonPane json={model.rawJson} />
  </div>
{/if}

<style>
  .inspector__eyebrow {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 11px;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
  }
  .inspector__title {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 17px;
    margin: 2px 0 1px;
    line-height: 1.2;
  }
  .inspector__sub {
    font-size: 11.5px;
    color: var(--color-neutral-600);
    font-style: italic;
    margin-bottom: var(--space-2);
  }
  .inspector__row {
    display: flex;
    gap: var(--space-2);
    padding: 2px 0;
  }
  .inspector__key {
    flex: 0 0 106px;
    font-size: 11.5px;
    color: var(--color-neutral-600);
  }
  .inspector__value {
    flex: 1;
    font-size: 12.5px;
    word-break: break-word;
  }
  .inspector__section-heading {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 11px;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
    margin: var(--space-3) 0 3px;
  }
  .inspector__empty-note {
    margin: 0;
    font-size: 12px;
    color: var(--color-neutral-600);
    font-style: italic;
  }
  .inspector__correlated-row {
    display: flex;
    gap: var(--space-2);
    padding: 2px 0;
    font-size: 12px;
    cursor: pointer;
  }
  .inspector__correlated-row:hover {
    background: var(--color-neutral-100);
  }
  .inspector__correlated-type {
    flex: 0 0 96px;
  }
  .inspector__correlated-summary {
    flex: 1;
    min-width: 0;
    color: var(--color-neutral-700);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .inspector__correlated-offset {
    color: var(--color-neutral-600);
    white-space: nowrap;
  }
  .inspector__sharebar-block {
    margin: var(--space-1) 0;
  }
  .inspector__sharebar-caption {
    font-size: 11px;
    color: var(--color-neutral-600);
  }
</style>
