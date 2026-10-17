<script lang="ts">
  import Async from '../components/Async.svelte';
  import Badge from '../components/Badge.svelte';
  import Card from '../components/Card.svelte';
  import { api } from '../lib/api';
  import { dateLong, money, monthName } from '../lib/format';
  import { t } from '../lib/i18n';
  import { loader } from '../lib/load.svelte';
  import { go, session } from '../lib/store.svelte';
  import type { BillSummary, Home } from '../lib/types';

  const landlord = $derived(session.me?.role === 'landlord');
  const l = loader(async () => {
    const [home, bills] = await Promise.all([api.get<Home>('/home'), api.get<{ bills: BillSummary[] }>('/bills')]);
    return { home, bills: bills.bills };
  });

  type Action = { text: string; go: () => void };
  function actions(home: Home, bills: BillSummary[]): Action[] {
    const out: Action[] = [];
    if (!home.property) return [{ text: t('home.act.setup'), go: () => go('property') }];
    if (landlord && !home.property.tenant_connected) out.push({ text: t('home.act.invite'), go: () => go('property') });
    const draft = bills.find((b) => b.status === 'draft');
    const approved = bills.find((b) => b.status === 'approved');
    if (landlord) {
      if (draft) out.push({ text: t('home.act.review', { period: monthName(draft.period) }), go: () => go('bills', draft.id) });
      if (approved) out.push({ text: t('home.act.pay', { period: monthName(approved.period) }), go: () => go('bills', approved.id) });
      if (!draft && !home.billed && home.property.tenant_connected && (home.received ?? 0) < (home.total_registers ?? 4))
        out.push({ text: t('home.act.waiting'), go: () => go('readings') });
    } else {
      if (approved) out.push({ text: t('home.act.unpaid', { period: monthName(approved.period) }), go: () => go('bills', approved.id) });
      if (!home.billed) out.push({ text: t('home.act.send', { period: monthName(home.period ?? '') }), go: () => go('readings') });
    }
    return out;
  }
</script>

<Async {l}>
  {#snippet children(d)}
    {@const home = d.home}
    {#if !home.property}
      <Card>
        <h2 class="text-lg font-semibold">{t('home.noProperty')}</h2>
        {#if landlord}
          <button class="mt-3 font-semibold text-brand" onclick={() => go('property')}>{t('home.act.setup')} →</button>
        {/if}
      </Card>
    {:else}
      <div class="grid gap-3">
        <div>
          <h1 class="text-xl font-bold">{home.property.name}</h1>
          <p class="text-sm text-muted">{landlord ? home.property.tenant_name : monthName(home.period ?? '')}</p>
        </div>

        {#if d.home.last_bill}
          {@const b = d.home.last_bill}
          <button class="text-left" onclick={() => go('bills', b.id)}>
            <Card class="bg-brand! text-brandfg!">
              <div class="flex items-center justify-between text-sm opacity-80">
                <span>{t('home.lastBill')} · {monthName(b.period)}</span>
                <Badge status={b.status} onBrand />
              </div>
              <div class="num mt-2 text-4xl font-bold tracking-tight">{money(b.total)}</div>
            </Card>
          </button>
        {:else}
          <Card><p class="text-muted">{t('home.noBills')}</p></Card>
        {/if}

        <Card>
          <div class="flex items-center justify-between">
            <span class="text-sm text-muted">{t('home.period')}</span>
            <span class="font-semibold">{monthName(home.period ?? '')}</span>
          </div>
          <div class="mt-3 flex items-center gap-3">
            <div class="h-2 flex-1 overflow-hidden rounded-full bg-surface2">
              <div class="h-full rounded-full bg-ok transition-all" style="width:{((home.received ?? 0) / (home.total_registers ?? 4)) * 100}%"></div>
            </div>
            <span class="num text-sm text-muted">{home.received}/{home.total_registers}</span>
          </div>
          <p class="mt-2 text-sm">
            {#if home.billed}{home.due_date ?? home.next_due ? `${t('home.nextDue')}: ${dateLong(home.due_date ?? home.next_due)}` : t('home.readingsDone')}
            {:else if (home.received ?? 0) >= (home.total_registers ?? 4)}{t('home.readingsDone')}
            {:else if home.due_date}{t('home.dueOn')}: {dateLong(home.due_date)}{landlord ? ` · ${t('home.readingsProgress', { n: home.received ?? 0, total: home.total_registers ?? 4 })}` : ''}
            {:else}{landlord ? t('home.readingsProgress', { n: home.received ?? 0, total: home.total_registers ?? 4 }) : t('home.dueNow')}{/if}
          </p>
        </Card>

        <Card>
          <h2 class="mb-2 font-semibold">{t('home.actions')}</h2>
          {#each actions(home, d.bills) as a (a.text)}
            <button class="flex w-full items-center justify-between border-t border-line py-3 text-left first:border-t-0" onclick={a.go}>
              <span>{a.text}</span><span class="text-muted">›</span>
            </button>
          {:else}
            <p class="text-sm text-muted">{t('home.noActions')}</p>
          {/each}
        </Card>

        {#if !landlord && d.bills.length}
          <Card>
            <h2 class="mb-2 font-semibold">{t('home.history')}</h2>
            {#each d.bills.slice(0, 6) as b (b.id)}
              <button class="flex w-full items-center justify-between border-t border-line py-3 first:border-t-0" onclick={() => go('bills', b.id)}>
                <span>{monthName(b.period)}</span>
                <span class="flex items-center gap-2"><span class="num font-semibold">{money(b.total)}</span><Badge status={b.status} /></span>
              </button>
            {/each}
          </Card>
        {/if}
        <p class="pt-2 text-center text-xs text-muted">RentFlow · сборка {__BUILD__}</p>
      </div>
    {/if}
  {/snippet}
</Async>
