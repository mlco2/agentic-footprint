<script lang="ts">
  import { onMount } from "svelte";
  import Masthead from "./lib/components/Masthead.svelte";
  import TabBar from "./lib/components/TabBar.svelte";
  import Footer from "./lib/components/Footer.svelte";
  import DisconnectedBanner from "./lib/components/DisconnectedBanner.svelte";
  import Timeline from "./lib/tabs/Timeline.svelte";
  import Stream from "./lib/tabs/Stream.svelte";
  import Attribution from "./lib/tabs/Attribution.svelte";
  import Impact from "./lib/tabs/Impact.svelte";
  import Health from "./lib/tabs/Health.svelte";
  import { afClient } from "./lib/client/afClient.svelte";
  import { uiStore, type Tab } from "./lib/stores/uiStore.svelte";

  // Ids double as display labels via `.tab { text-transform: capitalize }`
  // in TabBar — no separate id<->label lookup table needed.
  const TABS: Tab[] = ["timeline", "stream", "attribution", "impact", "health"];

  onMount(() => {
    void afClient.start();
  });
</script>

<div class="af-shell">
  <Masthead />
  <TabBar tabs={TABS} active={uiStore.tab} onSelect={(t) => uiStore.setTab(t)} />
  <DisconnectedBanner status={afClient.status} />

  <main class="af-main">
    {#if uiStore.tab === "timeline"}
      <Timeline />
    {:else if uiStore.tab === "stream"}
      <Stream />
    {:else if uiStore.tab === "attribution"}
      <Attribution />
    {:else if uiStore.tab === "impact"}
      <Impact />
    {:else if uiStore.tab === "health"}
      <Health />
    {/if}
  </main>

  <Footer />
</div>
