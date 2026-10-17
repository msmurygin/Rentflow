<script lang="ts">
  import { onDestroy } from 'svelte';
  import Async from '../components/Async.svelte';
  import Badge from '../components/Badge.svelte';
  import Btn from '../components/Btn.svelte';
  import Card from '../components/Card.svelte';
  import Sheet from '../components/Sheet.svelte';
  import { api, errorText } from '../lib/api';
  import { dateLong, money, monthName, num, price } from '../lib/format';
  import { t, type Key } from '../lib/i18n';
  import { loader } from '../lib/load.svelte';
  import { go, notify } from '../lib/store.svelte';
  import { tg } from '../lib/tg';
  import type { BillDetail } from '../lib/types';

  let { id }: { id: number } = $props();

  const l = loader(async () => {
    const d = await api.get<BillDetail>(`/bills/${id}`);
    for (const r of d.readings) if (r.has_photo) void thumb(r.reading_id);
    return d;
  });

  let thumbs = $state<Record<number, string>>({});
  let zoom = $state<string | null>(null);
  const urls: string[] = [];
  async function thumb(rid: number) {
    const u = await api.photoUrl(`/photos/${rid}`);
    if (u) {
      urls.push(u);
      thumbs[rid] = u;
    }
  }
  onDestroy(() => urls.forEach((u) => URL.revokeObjectURL(u)));

  type Act = 'approve' | 'reject' | 'paid' | 'reissue';
  let ask = $state<Act | null>(null);
  let busy = $state(false);

  const texts: Record<Act, { title: Key; text?: Key; ok: Key; danger?: boolean }> = {
    approve: { title: 'bills.approve', text: 'bills.approveAsk', ok: 'bills.approve' },
    reject: { title: 'bills.reject', text: 'bills.rejectAsk', ok: 'bills.reject', danger: true },
    paid: { title: 'bills.paid', ok: 'bills.paid' },
    reissue: { title: 'bills.reissue', text: 'bills.reissueAsk', ok: 'bills.reissue' },
  };

  async function run(act: Act) {
    busy = true;
    try {
      await api.post(`/bills/${id}/${act}`);
      tg.haptic('success');
      notify(t('bills.done'));
      ask = null;
      // после выпуска заново и отклонения этого счёта уже нет «в строю» — возвращаемся к списку
      if (act === 'reissue' || act === 'reject') go('bills');
      else await l.reload();
    } catch (e) {
      notify(errorText(e, t('genericError'), t('offline')), 'err');
      tg.haptic('error');
      ask = null;
      await l.reload();
    } finally {
      busy = false;
    }
  }

  const formula = (ln: BillDetail['snapshot']['lines'][number]) =>
    ln.code === 'sewer' ? `${num(ln.usage)} × ${price(ln.price)}` : `${num(ln.current)} − ${num(ln.previous)} = ${num(ln.usage)} × ${price(ln.price)}`;
  const unitOf = (code: string) => (code === 't1' || code === 't2' ? t('unit.kwh') : t('unit.m3'));
</script>

<Async {l}>
  {#snippet children(b)}
    <div class="grid gap-3 pb-4">
      <button class="text-left text-sm font-semibold text-brand" onclick={() => go('bills')}>‹ {t('nav.bills')}</button>
      <Card>
        <div class="flex items-center justify-between">
          <h1 class="text-lg font-semibold">{monthName(b.period)}</h1>
          <Badge status={b.status} />
        </div>
        <div class="num mt-2 text-4xl font-bold tracking-tight">{money(b.total)}</div>
        {#if b.paid_at}<p class="mt-1 text-sm text-muted">{dateLong(b.paid_at)}</p>{/if}
      </Card>

      <Card>
        <h2 class="mb-1 font-semibold">{t('bills.lines')}</h2>
        {#each b.snapshot.lines as ln (ln.code)}
          <div class="border-t border-line py-3 first:border-t-0">
            <div class="flex items-baseline justify-between gap-3">
              <span class="font-medium">{t(`reg.${ln.code}` as Key)}</span>
              <span class="num font-semibold">{money(ln.amount)}</span>
            </div>
            <p class="num mt-0.5 text-sm text-muted">{formula(ln)} ₽/{unitOf(ln.code)} · {num(ln.usage)} {unitOf(ln.code)}</p>
          </div>
        {/each}
        <div class="flex items-baseline justify-between border-t border-line pt-3">
          <span class="font-semibold">{t('bills.total')}</span>
          <span class="num text-lg font-bold">{money(b.snapshot.total)}</span>
        </div>
      </Card>

      <Card>
        <h2 class="mb-1 font-semibold">{t('bills.dates')}</h2>
        {#each b.snapshot.lines.filter((x) => x.code !== 'sewer') as ln (ln.code)}
          {@const cur = b.readings.find((r) => r.code === ln.code)}
          <div class="flex items-center justify-between border-t border-line py-2.5 text-sm first:border-t-0">
            <span>{t(`reg.${ln.code}` as Key)}</span>
            <span class="num text-muted">{dateLong(ln.previous_read_on)} → {dateLong(cur?.read_on)}</span>
          </div>
        {/each}
      </Card>

      {#if b.readings.some((r) => r.has_photo)}
        <Card>
          <h2 class="mb-2 font-semibold">{t('bills.photos')}</h2>
          <div class="grid grid-cols-4 gap-2">
            {#each b.readings.filter((r) => r.has_photo) as r (r.reading_id)}
              {#if thumbs[r.reading_id]}
                <button onclick={() => (zoom = thumbs[r.reading_id])} aria-label={t(`reg.${r.code}` as Key)}>
                  <img src={thumbs[r.reading_id]} alt={t(`reg.${r.code}` as Key)} class="aspect-square w-full rounded-lg object-cover" />
                </button>
              {:else}
                <div class="aspect-square animate-pulse rounded-lg bg-surface2"></div>
              {/if}
            {/each}
          </div>
        </Card>
      {/if}

      <div class="grid gap-2">
        {#if b.can_approve}<Btn onclick={() => (ask = 'approve')}>{t('bills.approve')}</Btn>{/if}
        {#if b.can_mark_paid}<Btn onclick={() => (ask = 'paid')}>{t('bills.paid')}</Btn>{/if}
        {#if b.can_reissue}<Btn variant="secondary" onclick={() => (ask = 'reissue')}>{t('bills.reissue')}</Btn>{/if}
        {#if b.can_reject}<Btn variant="danger" onclick={() => (ask = 'reject')}>{t('bills.reject')}</Btn>{/if}
      </div>
    </div>

    {#if ask}
      {@const c = texts[ask]}
      {@const act = ask}
      <Sheet title={t(c.title)} text={c.text ? t(c.text) : ''} confirmLabel={t(c.ok)} danger={c.danger} {busy} onconfirm={() => run(act)} oncancel={() => (ask = null)}>
        <p class="num text-2xl font-bold">{money(b.total)}</p>
      </Sheet>
    {/if}
    {#if zoom}
      <div class="fixed inset-0 z-50 grid place-items-center bg-black/90 p-3" role="presentation" onclick={() => (zoom = null)}>
        <img src={zoom} alt="" class="max-h-full max-w-full rounded-lg" />
      </div>
    {/if}
  {/snippet}
</Async>
