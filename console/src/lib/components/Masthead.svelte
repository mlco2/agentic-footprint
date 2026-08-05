<script lang="ts">
  // SSE dot + live/pause toggle read AfClient/UiStore state directly, no
  // local state (DATA-CONTRACT §3 client layer). The session picker is the
  // console's one session control: it scopes the *computed* views
  // (Impact/Attribution, via sessionStore.data) and deliberately not
  // Timeline/Stream, which show every session unfiltered. "follow latest"
  // mirrors the live-toggle's semantics: an explicit pick pins, the empty
  // value re-enters follow mode.
  import { afClient } from "../client/afClient.svelte";
  import { sessionStore } from "../stores/sessionStore.svelte";
  import { uiStore } from "../stores/uiStore.svelte";

  const sessions = $derived(sessionStore.list);
  const selectedId = $derived(sessionStore.selectedId);

  /** Grouped for the picker: agent name → its sessions, list order kept. */
  const byAgent = $derived.by(() => {
    const groups = new Map<string, typeof sessions>();
    for (const row of sessions) {
      const agent = row.agent_app?.name ?? "unknown agent";
      const group = groups.get(agent);
      if (group) group.push(row);
      else groups.set(agent, [row]);
    }
    return [...groups.entries()];
  });

  function shortId(id: string): string {
    return id.length > 10 ? `${id.slice(0, 10)}…` : id;
  }

  function onPick(event: Event & { currentTarget: HTMLSelectElement }): void {
    afClient.selectSession(event.currentTarget.value || null);
  }
</script>

<header class="masthead rule-masthead">
  <span class="masthead__title">af · debug console</span>
  <span class="masthead__subtitle">agentic-footprint control plane</span>
  <div class="masthead__spacer"></div>
  {#if sessions.length === 0}
    <span class="masthead__meta">session —</span>
  {:else}
    <label class="masthead__meta masthead__session">
      session
      <select class="masthead__picker" value={sessionStore.pinnedId ?? ""} onchange={onPick}>
        <option value="">
          follow latest{selectedId && sessionStore.pinnedId === null ? ` (${shortId(selectedId)})` : ""}
        </option>
        {#each byAgent as [agent, rows] (agent)}
          <optgroup label={agent}>
            {#each rows as row (row.session_id)}
              <option value={row.session_id}>{shortId(row.session_id)} · {row.events} ev</option>
            {/each}
          </optgroup>
        {/each}
      </select>
    </label>
  {/if}
  <button type="button" class="ghost-btn masthead__live-toggle" onclick={() => uiStore.toggleLive()}>
    {uiStore.live ? "live" : `SSE paused · buffering (${afClient.pausedBuffered})`}
  </button>
  <span class="masthead__dot masthead__dot--{afClient.status}" aria-hidden="true"></span>
  <span class="masthead__meta">SSE {afClient.status}</span>
</header>

<style>
  .masthead {
    flex: 0 0 auto;
    display: flex;
    align-items: baseline;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3) var(--space-1);
  }
  .masthead__title {
    font-family: var(--font-heading);
    font-weight: 700;
    font-size: 19px;
    letter-spacing: -0.01em;
  }
  .masthead__subtitle {
    font-size: 12px;
    color: var(--color-neutral-700);
    font-style: italic;
  }
  .masthead__spacer {
    flex: 1;
  }
  .masthead__meta {
    font-size: 12px;
    color: var(--color-neutral-700);
  }
  .masthead__live-toggle {
    font-size: 12px;
    padding: 1px var(--space-1);
  }
  .masthead__session {
    display: inline-flex;
    align-items: baseline;
    gap: var(--space-1);
  }
  /* Same quiet register as the rest of the masthead meta: the picker is
     ambient chrome, not a call to action. */
  .masthead__picker {
    font: inherit;
    font-size: 12px;
    color: inherit;
    background: transparent;
    border: none;
    border-bottom: 1px solid var(--color-neutral-400);
    padding: 0 var(--space-1) 1px 0;
    cursor: pointer;
    max-width: 22ch;
  }
  .masthead__dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--color-neutral-400);
  }
  /* connecting: neutral (nothing to report yet). live: solid ink (measured,
     healthy). reconnecting/offline: the system's one alarm color — rationed
     everywhere else, earned here by an actual disconnect (DESIGN-SYSTEM §3;
     global-constraints.md #3, #6: disconnected banner on SSE drop, never
     stale-but-plausible). Offline gets the darker step of the same hue to
     read as more severe than a mid-reconnect retry, without a new color. */
  .masthead__dot--connecting {
    background: var(--color-neutral-400);
  }
  .masthead__dot--live {
    background: var(--color-accent);
  }
  .masthead__dot--reconnecting {
    background: var(--color-accent-2);
  }
  .masthead__dot--offline {
    background: var(--color-accent-2-700);
  }
</style>
