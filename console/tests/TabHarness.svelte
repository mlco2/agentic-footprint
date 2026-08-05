<script lang="ts">
  // Test-only harness (never imported by console/src/): mirrors App.svelte's
  // actual `{#if uiStore.tab === ...}` conditional-mount structure for just
  // the two tabs this suite cares about, so the hidden-tab-discipline test
  // exercises the REAL mechanism (Svelte destroying/creating the tab
  // component on tab switch) rather than asserting something trivial.
  import Timeline from "../src/lib/tabs/Timeline.svelte";
  import Stream from "../src/lib/tabs/Stream.svelte";
  import { uiStore } from "../src/lib/stores/uiStore.svelte";
</script>

{#if uiStore.tab === "timeline"}
  <Timeline />
{:else if uiStore.tab === "stream"}
  <Stream />
{/if}
