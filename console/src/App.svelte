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
  import OnboardingPanel from "./lib/components/OnboardingPanel.svelte";
  import { afootClient } from "./lib/client/afootClient.svelte";
  import { sessionStore } from "./lib/stores/sessionStore.svelte";
  import { uiStore, type Tab } from "./lib/stores/uiStore.svelte";

  // Ids double as display labels via `.tab { text-transform: capitalize }`
  // in TabBar — no separate id<->label lookup table needed.
  const TABS: Tab[] = ["timeline", "stream", "attribution", "impact", "health"];

  // The watcher is reachable (or we are still finding out) and has no
  // session to show: say what it is waiting for instead of rendering five
  // empty tabs. While disconnected the tabs stay, with whatever they last
  // held, under the banner — an onboarding screen then would be a lie.
  const onboarding = $derived(sessionStore.list.length === 0 && (afootClient.status === "live" || afootClient.status === "connecting"));

  onMount(() => {
    void afootClient.start();
  });
</script>

<div class="afoot-shell">
  <Masthead />
  <TabBar tabs={TABS} active={uiStore.tab} onSelect={(t) => uiStore.setTab(t)} />
  <DisconnectedBanner status={afootClient.status} />

  <main class="afoot-main">
    {#if onboarding}
      <OnboardingPanel />
    {:else if uiStore.tab === "timeline"}
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
