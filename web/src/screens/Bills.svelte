<script lang="ts">
  import Async from '../components/Async.svelte';
  import Badge from '../components/Badge.svelte';
  import Card from '../components/Card.svelte';
  import { api } from '../lib/api';
  import { money, monthName, num } from '../lib/format';
  import { t, type Key } from '../lib/i18n';
  import { loader } from '../lib/load.svelte';
  import { go } from '../lib/store.svelte';
  import type { BillSummary, HistoryMonth } from '../lib/types';

  let tab = $state<'bills' | 'usage'>('bills');
  const l = loader(async () => (await api.get<{ bills: BillSummary[] }>('/bills')).bills);
  const h = loader(async () => (await api.get<{ months: HistoryMonth[] }>('/history')).months);
  const unit = (code: string) => (code === 't1' || code === 't2' ? t('unit.kwh') : t('unit.m3'));
</script>

<div class="grid gap-3 pb-4">
  <h1 class="text-xl font-bold">{t('bills.title')}</h1>
  <div class="grid grid-cols-2 gap-1 rounded-xl bg-surface2 p-1 text-sm font-semibold" role="tablist">
    {#each [['bills', t('bills.tabBills')], ['usage', t('bills.tabUsage')]] as [k, name] (k)}
      <button role="tab" aria-selected={tab === k} class="rounded-lg py-2 {tab === k ? 'bg-surface text-fg' : 'text-muted'}" onclick={() => (tab = k as 'bills' | 'usage')}>{name}</button>
    {/each}
  </div>

  {#if tab === 'bills'}
    <Async {l}>
      {#snippet children(bills)}
        {#if bills.length === 0}
          <Card><p class="text-center text-muted">{t('bills.empty')}</p></Card>
        {:else}
          <Card class="py-1">
            {#each bills as b (b.id)}
              <button class="flex w-full items-center justify-between gap-3 border-t border-line py-3.5 text-left first:border-t-0" onclick={() => go('bills', b.id)}>
                <div>
                  <div class="font-medium">{monthName(b.period)}</div>
                  <div class="mt-0.5"><Badge status={b.status} overdue={b.overdue} /></div>
                </div>
                <div class="num text-lg font-bold">{money(b.total)}</div>
              </button>
            {/each}
          </Card>
        {/if}
      {/snippet}
    </Async>
  {:else}
    <Async l={h}>
      {#snippet children(months)}
        {#if months.length === 0}
          <Card><p class="text-center text-muted">{t('bills.usageEmpty')}</p></Card>
        {:else}
          {#each months as m (m.bill_id)}
            <Card>
              <button class="flex w-full items-baseline justify-between text-left" onclick={() => go('bills', m.bill_id)}>
                <span class="font-semibold">{monthName(m.period)}</span>
                <span class="num font-bold">{money(m.total)}</span>
              </button>
              <div class="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm">
                {#each m.lines as ln (ln.code)}
                  <div class="flex justify-between"><span class="text-muted">{t(`reg.${ln.code}` as Key)}</span><span class="num">{num(ln.usage)} {unit(ln.code)}</span></div>
                {/each}
              </div>
            </Card>
          {/each}
        {/if}
      {/snippet}
    </Async>
  {/if}
</div>
