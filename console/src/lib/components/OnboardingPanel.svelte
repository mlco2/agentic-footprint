<script lang="ts">
  // Shown instead of the tabs' content while the watcher has no session
  // yet: the watcher is up, here is what it is listening for, and here is
  // what each agent still needs. Not an error state — a watcher started
  // before any agent is the ordinary first run.
  import { statusStore } from "../stores/statusStore.svelte";
  import { uiStore } from "../stores/uiStore.svelte";
  import { selectAgentRows, selectWatcherLine } from "../selectors/agents";
  import AgentTable from "./AgentTable.svelte";

  const rows = $derived.by(() => selectAgentRows(statusStore.data, uiStore.nowMs));
  const watcherLine = $derived.by(() => selectWatcherLine(statusStore.data, uiStore.nowMs));
</script>

<section class="onboarding tab-pane" aria-label="Watcher status">
  <div class="rail-section">
    <div class="rail-section__heading">Watcher</div>
    {#if statusStore.data}
      <p class="onboarding__line"><span aria-hidden="true">● </span>{watcherLine}</p>
    {:else if statusStore.unavailable}
      <p class="onboarding__line">Connected. This version of af does not report watcher or agent status.</p>
    {:else}
      <p class="onboarding__line">Connecting to the watcher…</p>
    {/if}
  </div>

  {#if rows.length > 0}
    <div class="rail-section">
      <div class="rail-section__heading">Agents</div>
      <AgentTable {rows} />
    </div>
  {/if}

  <p class="onboarding__hint">Events appear here within a minute of an agent's first activity.</p>
</section>

<style>
  .onboarding {
    flex: 1;
    min-width: 0;
    max-width: 72ch;
    padding: var(--space-3);
  }
  .onboarding__line {
    margin: 0;
    font-size: 12.5px;
  }
  .onboarding__hint {
    margin: 0;
    font-size: 12.5px;
    font-style: italic;
    color: var(--color-neutral-700);
  }
</style>
