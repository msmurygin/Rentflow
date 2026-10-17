<script lang="ts">
  import Async from '../components/Async.svelte';
  import Btn from '../components/Btn.svelte';
  import Card from '../components/Card.svelte';
  import { api, errorText } from '../lib/api';
  import { addMonth, isValidNumber, money, monthFrom, monthName } from '../lib/format';
  import { t, type Key } from '../lib/i18n';
  import { loader } from '../lib/load.svelte';
  import { notify } from '../lib/store.svelte';
  import { tg } from '../lib/tg';
  import type { TariffVersion, Tariffs } from '../lib/types';

  const l = loader(async () => {
    const d = await api.get<Tariffs>('/tariffs');
    month = d.month;
    return d;
  });

  let month = $state('');
  let inputs = $state<Record<string, string>>({});
  let turnOff = $state(false);
  let open = $state<string | null>(null);
  let busy = $state(false);
  let errors = $state<Record<string, string>>({});

  // Какой тариф действует в выбранном месяце: последняя версия не позже него (то же правило, что на сервере; здесь только показ).
  function at(versions: TariffVersion[], m: string): TariffVersion | null {
    return versions.filter((v) => v.month <= m).sort((a, b) => b.month.localeCompare(a.month))[0] ?? null;
  }

  const months = (current: string) => Array.from({ length: 8 }, (_, i) => addMonth(current, i - 1));
  const unit = (code: string) => (code === 't1' || code === 't2' ? t('price.kwh') : t('price.m3'));

  async function save(d: Tariffs) {
    const changes: { code: string; price?: string; off?: boolean }[] = [];
    const e: Record<string, string> = {};
    for (const it of d.items) {
      const v = (inputs[it.code] ?? '').trim();
      if (v) {
        if (isValidNumber(v)) changes.push({ code: it.code, price: v });
        else e[it.code] = 'Нужно число';
      }
    }
    if (turnOff) changes.push({ code: 'sewer', off: true });
    errors = e;
    if (Object.keys(e).length) return;
    if (!changes.length) return notify(t('tariffs.nothing'), 'err');
    busy = true;
    try {
      const r = await api.put<Tariffs>('/tariffs', { month, changes });
      const hadDraft = r.draft_id != null;
      l.data = r;
      inputs = {};
      turnOff = false;
      tg.haptic('success');
      notify(hadDraft ? t('tariffs.savedDraft') : t('tariffs.saved'));
    } catch (err) {
      notify(errorText(err, t('genericError'), t('offline')), 'err');
      tg.haptic('error');
    } finally {
      busy = false;
    }
  }
</script>

<Async {l}>
  {#snippet children(d)}
    <div class="grid gap-3 pb-4">
      <h1 class="text-xl font-bold">{t('tariffs.title')}</h1>

      <Card class="grid gap-2">
        <label class="block">
          <span class="mb-1 block text-sm text-muted">{t('tariffs.month')}</span>
          <select class="min-h-12 w-full rounded-xl border border-line bg-bg px-3 text-fg" bind:value={month}>
            {#each months(d.month) as m (m)}<option value={m}>{monthName(m)}</option>{/each}
          </select>
        </label>
        <p class="text-sm text-muted">{t('tariffs.policy')}</p>
      </Card>

      {#each d.items as it (it.code)}
        {@const eff = at(it.versions, month)}
        <Card class="grid gap-3">
          <div class="flex items-start justify-between gap-2">
            <div>
              <h2 class="font-semibold">{t(`reg.${it.code}` as Key)}</h2>
              <p class="text-xs text-muted">{unit(it.code)}</p>
            </div>
            <div class="num text-right">
              {#if eff && eff.enabled}
                <div class="text-xl font-bold">{money(eff.price)}</div>
              {:else}
                <div class="text-sm font-semibold text-muted">{t('tariffs.off')}</div>
              {/if}
              {#if eff}<div class="text-xs text-muted">с {monthFrom(eff.month)}</div>{/if}
            </div>
          </div>

          <label class="block">
            <span class="mb-1 block text-sm text-muted">{t('tariffs.new')}</span>
            <span class="flex items-center rounded-xl border bg-bg px-3 focus-within:border-brand {errors[it.code] ? 'border-bad' : 'border-line'}">
              <input class="num min-h-12 w-full bg-transparent outline-none" inputmode="decimal" autocomplete="off" placeholder="—" bind:value={inputs[it.code]} />
              <span class="ml-2 shrink-0 text-sm text-muted">₽</span>
            </span>
            {#if errors[it.code]}<span class="mt-1 block text-sm text-bad">{errors[it.code]}</span>{/if}
          </label>

          {#if it.code === 'sewer' && eff?.enabled}
            <label class="flex items-center justify-between gap-3">
              <span class="text-sm">{t('tariffs.turnOff')}</span>
              <input type="checkbox" class="size-6 accent-[var(--brand)]" bind:checked={turnOff} />
            </label>
          {/if}

          <button class="text-left text-sm font-semibold text-brand" onclick={() => (open = open === it.code ? null : it.code)}>
            {t('tariffs.history')} {open === it.code ? '▾' : '▸'}
          </button>
          {#if open === it.code}
            <div>
              {#each it.versions as v (v.month)}
                <div class="flex justify-between border-t border-line py-2 text-sm first:border-t-0">
                  <span>{monthName(v.month)}</span>
                  <span class="num">{v.enabled ? money(v.price) : t('tariffs.off')}</span>
                </div>
              {/each}
            </div>
          {/if}
        </Card>
      {/each}

      <div class="sticky bottom-[calc(4.5rem+env(safe-area-inset-bottom))] z-10">
        <Btn {busy} onclick={() => save(d)}>{t('tariffs.apply', { month: monthFrom(month) })}</Btn>
      </div>
      <p class="text-center text-xs text-muted">{t('tariffs.approvedKept')}</p>
    </div>
  {/snippet}
</Async>
