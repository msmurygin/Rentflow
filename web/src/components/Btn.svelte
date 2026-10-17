<script lang="ts">
  import type { Snippet } from 'svelte';
  let {
    children,
    onclick,
    variant = 'primary',
    busy = false,
    disabled = false,
    class: cls = '',
  }: {
    children: Snippet;
    onclick?: () => void;
    variant?: 'primary' | 'secondary' | 'danger' | 'ghost';
    busy?: boolean;
    disabled?: boolean;
    class?: string;
  } = $props();
  const styles = {
    primary: 'bg-brand text-brandfg',
    secondary: 'bg-surface2 text-fg',
    danger: 'bg-bad/10 text-bad',
    ghost: 'text-brand',
  };
</script>

<button
  type="button"
  class="flex min-h-12 w-full items-center justify-center gap-2 rounded-xl px-4 text-base font-semibold transition active:scale-[0.98] disabled:opacity-50 {styles[variant]} {cls}"
  disabled={disabled || busy}
  {onclick}
>
  {#if busy}<span class="size-4 animate-spin rounded-full border-2 border-current border-t-transparent"></span>{/if}
  {@render children()}
</button>
