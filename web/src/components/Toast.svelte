<script lang="ts">
  import { toast } from '../lib/store.svelte';
</script>

{#if toast.text}
  {#key toast.id}
    <div
      role="status"
      class="fixed inset-x-4 bottom-[calc(5.5rem+env(safe-area-inset-bottom))] z-50 mx-auto max-w-md rounded-xl px-4 py-3 text-center text-sm font-medium shadow-lg {toast.kind ===
      'ok'
        ? 'bg-fg text-bg'
        : 'bg-bad text-white'}"
    >
      {toast.text}
    </div>
  {/key}
{/if}
