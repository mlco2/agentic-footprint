<script lang="ts">
  // "showing N of M" (global-constraints.md #6: the ring truncates, so this
  // must always be visible, never silently implied), plus the three fields
  // Task 9's brief calls out as remaining: otlp endpoint, a byte-offsets
  // summary, and the spool path — all from `healthStore`/`sessionStore`
  // (DATA-CONTRACT §4: "Footer byte offsets, spool paths | /debug/health,
  // /debug/session"). methodology/grid/clock stay "—" placeholders — out of
  // this task's scope.
  //
  // The Footer renders unconditionally (App.svelte mounts it outside the
  // `{#if uiStore.tab === ...}` gate), so it CANNOT reuse
  // selectors/health.ts's `selectHealthAside`/`selectCollectorTable` —
  // global-constraints.md #5 ("only the visible tab computes") means those
  // only run while the Health tab itself is active. This file reads
  // `healthStore`/`sessionStore` directly and formats through `format.ts`
  // itself, exactly like its own pre-existing `showing` derivation does for
  // `eventStore`.
  import { eventStore } from "../stores/eventStore.svelte";
  import { healthStore } from "../stores/healthStore.svelte";
  import { sessionStore } from "../stores/sessionStore.svelte";
  import { fmtCount } from "../format";

  const showing = $derived.by(() => {
    void eventStore.rev; // track the batched revision — retained/totalSeen are plain fields
    return `showing ${eventStore.retained} of ${eventStore.totalSeen}`;
  });

  const endpointLabel = $derived.by(() => {
    const otlp = healthStore.data?.otlp_receiver;
    if (!otlp) return "—";
    return otlp.endpoint === null ? (otlp.note ?? "no receiver") : `${otlp.endpoint} · ${otlp.protocol}`;
  });

  /** Per-collector `byte_offset`, verbatim (a collector with no spool file,
   * e.g. an HTTP-fed one, has none and is skipped — not fabricated as 0). */
  const offsetsLabel = $derived.by(() => {
    const collectors = healthStore.data?.collectors ?? [];
    const parts = collectors.filter((c) => c.byte_offset !== undefined).map((c) => `${c.name} ${fmtCount(c.byte_offset as number)}`);
    return parts.length > 0 ? parts.join(" · ") : "—";
  });

  /** `session.state_dir` + the documented `/spool` suffix (docs/design-log.md:
   * "All spool files live under ... `spool/`") — the spool directory, not
   * each collector's individual file (the Health tab's Ingestion panel
   * already itemises those; the footer's job is one persistent summary
   * line visible from every tab). */
  const spoolLabel = $derived.by(() => {
    const stateDir = sessionStore.data?.state_dir;
    return stateDir !== undefined ? `${stateDir}/spool` : "—";
  });
</script>

<footer class="footer rule-ink-t">
  <span>endpoint {endpointLabel}</span>
  <span>{showing}</span>
  <span>offsets {offsetsLabel}</span>
  <span>spool {spoolLabel}</span>
  <div class="footer__spacer"></div>
  <span>methodology —</span>
  <span>grid —</span>
  <span>clock —</span>
</footer>

<style>
  .footer {
    flex: 0 0 auto;
    display: flex;
    align-items: baseline;
    gap: var(--space-3);
    padding: var(--space-1) var(--space-3);
    font-size: 11px;
    color: var(--color-neutral-600);
  }
  .footer__spacer {
    flex: 1;
  }
</style>
