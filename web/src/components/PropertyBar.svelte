<script lang="ts">
  import Btn from './Btn.svelte';
  import { t } from '../lib/i18n';
  import { selectProperty, session } from '../lib/store.svelte';

  let { onadd }: { onadd: () => void } = $props();
  let open = $state(false);

  const list = $derived(session.me?.properties ?? []);
  const current = $derived(list.find((p) => p.id === session.propertyId));
  const canAdd = $derived(list.some((p) => p.role === 'owner') || session.me?.role === 'landlord');
  // панель нужна, когда есть из чего выбирать или куда добавлять
  const visible = $derived(list.length > 1 || canAdd);
</script>

{#if visible && current}
  <button class="mb-3 flex w-full items-center justify-between rounded-xl bg-surface px-3 py-2 text-left text-sm" onclick={() => (open = true)}>
    <span class="min-w-0 truncate font-semibold">{current.name}</span>
    <span class="ml-2 shrink-0 text-muted">{list.length > 1 ? `${list.length} ▾` : '▾'}</span>
  </button>
{/if}

{#if open}
  <div class="fixed inset-0 z-40 flex items-end bg-black/50" role="presentation" onclick={() => (open = false)}>
    <div class="max-h-[85vh] w-full overflow-y-auto rounded-t-3xl bg-surface p-5 pb-[calc(1.25rem+env(safe-area-inset-bottom))]" role="dialog" aria-modal="true" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && (open = false)}>
      <h2 class="mb-2 text-lg font-semibold">{t('bar.switch')}</h2>
      {#each list as p (p.id)}
        <button class="flex w-full items-center justify-between border-t border-line py-3 text-left first:border-t-0" onclick={() => { open = false; if (p.id !== session.propertyId) selectProperty(p.id); }}>
          <span class="min-w-0 truncate">{p.id === session.propertyId ? '✓ ' : ''}{p.name}</span>
          <span class="ml-2 shrink-0 text-xs text-muted">{p.role === 'owner' ? t('bar.owner') : t('bar.tenant')}</span>
        </button>
      {/each}
      {#if canAdd}
        <div class="mt-3"><Btn variant="secondary" onclick={() => { open = false; onadd(); }}>＋ {t('bar.add')}</Btn></div>
      {/if}
    </div>
  </div>
{/if}
