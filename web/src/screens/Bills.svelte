<script lang="ts">
  import Async from '../components/Async.svelte';
  import Badge from '../components/Badge.svelte';
  import Card from '../components/Card.svelte';
  import { api } from '../lib/api';
  import { money, monthName } from '../lib/format';
  import { t } from '../lib/i18n';
  import { loader } from '../lib/load.svelte';
  import { go } from '../lib/store.svelte';
  import type { BillSummary } from '../lib/types';

  const l = loader(async () => (await api.get<{ bills: BillSummary[] }>('/bills')).bills);
</script>

<Async {l}>
  {#snippet children(bills)}
    <div class="grid gap-3 pb-4">
      <h1 class="text-xl font-bold">{t('bills.title')}</h1>
      {#if bills.length === 0}
        <Card><p class="text-center text-muted">{t('bills.empty')}</p></Card>
      {:else}
        <Card class="py-1">
          {#each bills as b (b.id)}
            <button class="flex w-full items-center justify-between gap-3 border-t border-line py-3.5 text-left first:border-t-0" onclick={() => go('bills', b.id)}>
              <div>
                <div class="font-medium">{monthName(b.period)}</div>
                <div class="mt-0.5"><Badge status={b.status} /></div>
              </div>
              <div class="num text-lg font-bold">{money(b.total)}</div>
            </button>
          {/each}
        </Card>
      {/if}
    </div>
  {/snippet}
</Async>
