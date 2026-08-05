<script lang="ts">
  // Pre-wrap/break-word serif JSON block — NO monospace anywhere in this
  // console (DESIGN-SYSTEM §4). Reused by the Inspector's raw JSONL line and,
  // later, anywhere else a raw record needs showing verbatim.
  let { json, label = "Raw JSONL line" }: { json: string; label?: string } = $props();
</script>

<div class="json-pane">
  <div class="json-pane__label">{label}</div>
  <pre class="json-pane__body">{json}</pre>
</div>

<style>
  .json-pane__label {
    font-family: var(--font-heading);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
    margin-bottom: 3px;
  }
  .json-pane__body {
    margin: 0;
    font-family: var(--font-body);
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--color-neutral-800);
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
