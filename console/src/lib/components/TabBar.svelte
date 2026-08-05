<script lang="ts">
  import type { Tab } from "../stores/uiStore.svelte";

  let {
    tabs,
    active,
    onSelect,
  }: {
    tabs: readonly Tab[];
    active: Tab;
    onSelect: (tab: Tab) => void;
  } = $props();
</script>

<!-- `role="tablist"` on a `<nav>` trips Svelte's a11y check (a landmark
     element can't carry an interactive/composite role) — a plain `<div>`
     already carries everything the ARIA Tabs pattern needs (tablist/tab),
     no separate nav landmark required. -->
<div class="tabbar rule-ink-b" role="tablist">
  {#each tabs as tab (tab)}
    <button
      type="button"
      class="ghost-btn tab"
      class:is-active={tab === active}
      role="tab"
      aria-selected={tab === active}
      onclick={() => onSelect(tab)}
    >
      {tab}
    </button>
  {/each}
</div>

<style>
  .tabbar {
    flex: 0 0 32px;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 0 var(--space-3);
  }
  .tab {
    font-size: 14px;
    font-weight: 400;
    text-transform: capitalize;
  }
  .tab.is-active {
    font-weight: 600;
  }
</style>
