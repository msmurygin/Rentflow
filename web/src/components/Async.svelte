<script lang="ts" generics="T">
  import type { Snippet } from 'svelte';
  import type { Loader } from '../lib/load.svelte';
  import { t } from '../lib/i18n';
  import Btn from './Btn.svelte';
  let { l, children }: { l: Loader<T>; children: Snippet<[T]> } = $props();
</script>

{#if l.data}
  {@render children(l.data)}
{:else if l.error}
  <div class="rounded-2xl border border-line bg-surface p-5 text-center">
    <p class="text-sm text-muted">{l.error}</p>
    <div class="mt-4"><Btn variant="secondary" onclick={() => l.reload()}>{t('retry')}</Btn></div>
  </div>
{:else}
  <div class="grid gap-3" aria-busy="true" aria-label={t('loading')}>
    {#each [0, 1, 2] as i (i)}<div class="h-24 animate-pulse rounded-2xl bg-surface"></div>{/each}
  </div>
{/if}
