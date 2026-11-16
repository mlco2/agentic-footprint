<script lang="ts">
  // One row per coding agent the watcher knows about: mark, name, state in
  // words, then — when there is something to say — why, and the command to
  // run. Shared by the onboarding panel and the Health tab. Display only:
  // rows arrive already worded from selectors/agents.ts.
  import type { AgentRowView } from "../selectors/agents";

  let { rows }: { rows: readonly AgentRowView[] } = $props();
</script>

<div class="agent-table">
  {#each rows as row (row.id)}
    <div class="agent-row" data-agent={row.id}>
      <span class="agent-row__mark" aria-hidden="true">{row.mark}</span>
      <span class="agent-row__name">{row.name}</span>
      <span class="agent-row__label">{row.label}</span>
      {#if row.note || row.action}
        <span class="agent-row__more">
          {row.note ?? ""}{#if row.action}{row.note ? " · " : ""}run <code class="agent-row__action">{row.action}</code>{/if}
        </span>
      {/if}
      {#each row.processes as process (process.pid)}
        <span class="agent-row__process" data-pid={process.pid}>
          <span aria-hidden="true">{process.mark}</span>
          {`PID ${process.pid}${process.where ? ` · ${process.where}` : ""} — ${process.label}`}
          {#if process.note || process.action}
            <span class="agent-row__process-more">
              {process.note ?? ""}{#if process.action}{process.note ? " · " : ""}run <code class="agent-row__action">{process.action}</code>{/if}
            </span>
          {/if}
        </span>
      {/each}
    </div>
  {/each}
</div>

<style>
  .agent-table {
    font-size: 12.5px;
  }
  /* mark | name | label, with the explanation on its own line under the
     label column — a grid so that second line aligns without a spacer. */
  .agent-row {
    display: grid;
    grid-template-columns: 1.4em 9em 1fr;
    column-gap: var(--space-1);
    align-items: baseline;
    padding: 3px 0;
  }
  .agent-row__mark {
    color: var(--color-neutral-700);
  }
  .agent-row__name {
    font-weight: 600;
  }
  .agent-row__more {
    grid-column: 3;
    font-size: 11.5px;
    color: var(--color-neutral-600);
  }
  /* One line per process under the agent, its reason on a line of its own. */
  .agent-row__process {
    grid-column: 3;
    font-size: 11.5px;
    color: var(--color-neutral-700);
  }
  .agent-row__process-more {
    display: block;
    padding-left: 1.4em;
    color: var(--color-neutral-600);
    overflow-wrap: anywhere;
  }
  .agent-row__action {
    font-size: 11px;
    color: var(--color-neutral-700);
  }
</style>
