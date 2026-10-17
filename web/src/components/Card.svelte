<script lang="ts">
  import type { Snippet } from 'svelte';
  let { children, class: cls = '' }: { children: Snippet; class?: string } = $props();
</script>

<section class="rounded-2xl border border-line bg-surface p-4 {cls}">{@render children()}</section>
