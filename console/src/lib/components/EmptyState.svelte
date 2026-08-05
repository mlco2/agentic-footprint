<script lang="ts">
  let { label, message }: { label: string; message: string } = $props();
</script>

<div class="empty-state">
  <div class="empty-state__eyebrow">{label}</div>
  <p class="empty-state__message">{message}</p>
</div>

<style>
  .empty-state {
    padding: var(--space-2);
  }
  .empty-state__eyebrow {
    font-family: var(--font-heading);
    font-weight: 600;
    font-size: 11px;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--color-neutral-700);
    margin-bottom: var(--space-1);
  }
  .empty-state__message {
    margin: 0;
    font-size: 12.5px;
    color: var(--color-neutral-700);
  }
</style>
