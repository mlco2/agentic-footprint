<script lang="ts">
  // flex:1 main + 296px aside (SCREENS.md §4). Main: cards -> criteria
  // table -> cross-paradigm note -> per-model table. Aside: "what
  // token-only misses" -> estimation-status histogram -> af statusline
  // preview -> methodology block.
  //
  // Unlike Attribution/Timeline (which read eventStore/allocStore's bulk
  // mutable state and pass a numeric `.rev` into their selectors),
  // ReportStore/SessionStore each hold one small replace-on-arrival object
  // directly in `$state`, so that object itself is both the data AND the
  // correct memo key for selectors/impact.ts's `memo1` wrapping — reading
  // `reportStore.levels.session` / `sessionStore.data` here is already
  // reactive, with no extra `rev` counter needed.
  import { reportStore } from "../stores/reportStore.svelte";
  import { sessionStore } from "../stores/sessionStore.svelte";
  import { CRITERIA, CROSS_PARADIGM_NOTE, selectCriteriaTable, selectImpactAside, selectImpactCards, selectPerModel } from "../selectors/impact";
  import EmptyState from "../components/EmptyState.svelte";
  import ImpactCards from "../components/ImpactCards.svelte";
  import CriteriaTable from "../components/CriteriaTable.svelte";
  import StatuslinePreview from "../components/StatuslinePreview.svelte";

  // Scoped to the picked session: an impact_join is a per-session
  // computation, and blending sessions here would cross measurement
  // paradigms. `forSession` falls back to the unlabeled report on servers
  // that predate multi-session (and on the mock).
  const report = $derived(reportStore.forSession(sessionStore.selectedId));
  const session = $derived(sessionStore.data);

  const cards = $derived.by(() => selectImpactCards(report));
  const criteriaRows = $derived.by(() => selectCriteriaTable(report));
  const perModel = $derived.by(() => selectPerModel(report));
  const aside = $derived.by(() => selectImpactAside(report, session));
</script>

<div class="impact">
  <section class="impact__main tab-pane">
    {#if report === undefined}
      <EmptyState label="Impact" message="no impact_join data yet" />
    {:else}
      <ImpactCards {cards} />

      <div class="impact__section-heading">impact_join · {report.impact_join.unit.level}</div>
      <CriteriaTable rows={criteriaRows} />

      <div class="cross-paradigm">
        <div class="cross-paradigm__eyebrow status-alarm">{CROSS_PARADIGM_NOTE.eyebrow}</div>
        {#each CROSS_PARADIGM_NOTE.prose as line, i (i)}
          <p class="cross-paradigm__prose">{line}</p>
        {/each}
      </div>

      <div class="impact__section-heading">Per-model{#if perModel.llmCallsLabel} · {perModel.llmCallsLabel} llm_calls{/if}</div>
      {#if perModel.rows.length === 0}
        <p class="empty-note">no per-model estimates reported yet</p>
      {:else}
        <div class="per-model-table">
          <div class="per-model-table__row per-model-table__row--head rule-table-head">
            <span class="col-model">model</span>
            {#each CRITERIA as key (key)}
              <span class="col-criterion">{key}</span>
            {/each}
          </div>
          {#each perModel.rows as row (row.modelId)}
            <div class="per-model-table__row rule-table-row">
              <span class="col-model" class:status-alarm={row.isUnknown}>
                {row.modelId}
                {#if row.statusLabel}<span class="col-model__status"> · {row.statusLabel}</span>{/if}
              </span>
              {#each CRITERIA as key (key)}
                <span class="col-criterion" class:status-neutral={!row.cells[key].measured}>
                  {row.cells[key].label}
                </span>
              {/each}
            </div>
          {/each}
          <div class="per-model-table__row per-model-table__row--totals rule-table-head">
            <span class="col-model">total (server, ok estimates only)</span>
            {#each CRITERIA as key (key)}
              <span class="col-criterion" class:status-neutral={!perModel.totals[key].measured}>{perModel.totals[key].label}</span>
            {/each}
          </div>
        </div>
      {/if}
    {/if}
  </section>

  <aside class="impact__aside rule-ink-l tab-pane">
    {#if report === undefined}
      <EmptyState label="Methodology" message="no methodology data yet" />
    {:else}
      <div class="impact__section-heading">What token-only misses</div>
      <div class="misses-list">
        {#each aside.tokenOnlyMisses as row (row.label)}
          <div class="misses-row rule-table-row">
            <span class="misses-row__label">{row.label}</span>
            <span class="misses-row__value" class:status-alarm={row.tone === "alarm"}>{row.value}</span>
          </div>
        {/each}
      </div>

      <div class="impact__section-heading">Estimation status</div>
      <div class="histogram">
        {#each aside.histogram as row (row.status)}
          <div class="histogram-row">
            <span class="histogram-row__label" class:status-alarm={row.status === "unknown_model" && row.count > 0}>{row.status}</span>
            <span class="histogram-row__count">{row.count}</span>
          </div>
        {/each}
      </div>

      <div class="impact__section-heading">af statusline preview</div>
      <StatuslinePreview lines={aside.statuslineLines} />

      <div class="impact__section-heading">Methodology</div>
      {#if aside.methodology}
        <div class="methodology">
          <div class="methodology-row"><span>version</span><span>{aside.methodology.versionLabel}</span></div>
          <div class="methodology-row"><span>source</span><span>{aside.methodology.sourceLabel}</span></div>
          <div class="methodology-row"><span>ecologits</span><span>{aside.methodology.ecologitsLabel}</span></div>
          <div class="methodology-row"><span>codecarbon</span><span>{aside.methodology.codecarbonLabel}</span></div>
          <div class="methodology-row"><span>grid zone</span><span>{aside.methodology.gridZoneLabel}</span></div>
          <div class="methodology-row"><span>grid intensity</span><span>{aside.methodology.gridIntensityLabel}</span></div>
        </div>
      {:else}
        <p class="empty-note">methodology — not yet available</p>
      {/if}
    {/if}
  </aside>
</div>

<style>
  .impact {
    flex: 1;
    min-width: 0;
    display: flex;
  }
  .impact__main {
    flex: 1;
    min-width: 0;
  }
  .impact__aside {
    flex: 0 0 296px;
  }
  .impact__section-heading {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 11px;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
    margin: var(--space-4) 0 var(--space-2);
  }
  .empty-note {
    font-size: 12px;
    color: var(--color-neutral-600);
    font-style: italic;
  }

  /* — cross-paradigm note (DESIGN-SYSTEM §3: rationed magenta, an eyebrow +
     ink prose — never a box or a rule) — */
  .cross-paradigm {
    margin-top: var(--space-3);
  }
  .cross-paradigm__eyebrow {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 11px;
    letter-spacing: 0.09em;
    text-transform: uppercase;
  }
  .cross-paradigm__prose {
    font-size: 12.5px;
    color: var(--color-text);
    margin: 3px 0 0;
    max-width: 62em;
  }

  /* — per-model table — */
  .per-model-table__row {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    padding: 4px 0;
    font-size: 12px;
  }
  .per-model-table__row--head {
    font-size: 10.5px;
    letter-spacing: 0.07em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
    padding-bottom: 3px;
  }
  .per-model-table__row--totals {
    font-weight: 600;
    border-bottom: none;
  }
  .col-model {
    flex: 0 0 220px;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col-model__status {
    font-style: italic;
  }
  .col-criterion {
    flex: 1 0 0;
    min-width: 0;
    text-align: right;
  }

  /* — aside: misses list — (border comes from the shared .rule-table-row
     class in console.css, not redeclared here — global-constraints.md #2's
     "only the five sanctioned rule kinds") */
  .misses-row {
    display: flex;
    justify-content: space-between;
    gap: var(--space-2);
    padding: 2px 0;
    font-size: 12px;
  }
  .misses-row__label {
    color: var(--color-neutral-700);
  }
  .misses-row__value {
    text-align: right;
  }

  /* — aside: histogram — */
  .histogram-row {
    display: flex;
    justify-content: space-between;
    padding: 2px 0;
    font-size: 12px;
  }

  /* — aside: methodology — */
  .methodology-row {
    display: flex;
    justify-content: space-between;
    gap: var(--space-2);
    padding: 2px 0;
    font-size: 11.5px;
    color: var(--color-neutral-700);
  }
</style>
