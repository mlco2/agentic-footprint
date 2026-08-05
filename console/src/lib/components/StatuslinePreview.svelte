<script lang="ts">
  // `af statusline` stdout preview (SCREENS.md §4; docs/design-log.md
  // "statusline contract"): exactly two lines, serif (DESIGN-SYSTEM §4:
  // "there is no monospace anywhere — including... the af statusline
  // preview"), pre-wrap. The two strings are already the exact final text —
  // this component performs no formatting of its own.
  let { lines }: { lines: readonly [string, string] } = $props();
</script>

<pre class="statusline-preview">{lines[0]}
{lines[1]}</pre>

<style>
  .statusline-preview {
    margin: 0;
    font-family: var(--font-body);
    font-size: 12px;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
