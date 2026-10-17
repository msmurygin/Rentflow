<script lang="ts">
  import { onMount } from 'svelte';
  import Btn from '../components/Btn.svelte';
  import Card from '../components/Card.svelte';
  import Field from '../components/Field.svelte';
  import { api, errorText } from '../lib/api';
  import { isValidNumber } from '../lib/format';
  import { t, type Key } from '../lib/i18n';
  import { notify } from '../lib/store.svelte';
  import { tg } from '../lib/tg';

  /** `onfinished` вызывается, когда квартира создана и человек закрыл экран приглашения. */
  let {
    onfinished,
    oncreated,
    oncancel,
  }: { onfinished: (propertyId: number) => void; oncreated: (propertyId: number) => Promise<void>; oncancel?: () => void } = $props();

  const CODES = ['cold', 'hot', 't1', 't2'] as const;
  const ZONES: [number, string][] = [
    [2, 'Калининград (UTC+2)'], [3, 'Москва (UTC+3)'], [4, 'Самара (UTC+4)'], [5, 'Екатеринбург (UTC+5)'], [6, 'Омск (UTC+6)'],
    [7, 'Красноярск (UTC+7)'], [8, 'Иркутск (UTC+8)'], [9, 'Якутск (UTC+9)'], [10, 'Владивосток (UTC+10)'], [11, 'Магадан (UTC+11)'], [12, 'Камчатка (UTC+12)'],
  ];
  const TOTAL = 6;

  let step = $state(0);
  let loaded = $state(false);
  let resumed = $state(false);
  let busy = $state(false);
  let errors = $state<Record<string, string>>({});
  let created = $state<number | null>(null);
  let invite = $state<{ url: string } | null>(null);

  let d = $state({
    name: '',
    tenant_name: '',
    tz: 3,
    enabled: { cold: true, hot: true, t1: true, t2: true } as Record<string, boolean>,
    base_date: '',
    baselines: { cold: '', hot: '', t1: '', t2: '' } as Record<string, string>,
    prices: { cold: '', hot: '', t1: '', t2: '' } as Record<string, string>,
    sewer_on: true,
    sewer_price: '',
    reading_day: '',
    proof_required: true,
  });

  const on = (c: string) => d.enabled[c];
  const label = (c: string) => t(`reg.${c}` as Key);
  const unit = (c: string) => (c.startsWith('t') ? t('unit.kwh') : t('unit.m3'));

  /** Тело в формате сервера (PropertyBody) — его же хранит черновик. */
  function payload() {
    const disabled = CODES.filter((c) => !d.enabled[c]);
    const pick = (m: Record<string, string>) => Object.fromEntries(CODES.filter((c) => d.enabled[c]).map((c) => [c, m[c]]));
    return {
      name: d.name.trim(),
      tenant_name: d.tenant_name.trim() || null,
      tz_offset_hours: d.tz,
      disabled,
      base_date: d.base_date || null,
      baselines: pick(d.baselines),
      prices: pick(d.prices),
      sewer_price: d.sewer_on ? d.sewer_price : null,
      reading_day: d.reading_day.trim() ? Number(d.reading_day) : 0,
      proof_required: d.proof_required,
      // служебные поля интерфейса: нужны, чтобы при возобновлении экран выглядел как прежде
      _ui: { sewer_on: d.sewer_on, sewer_price: d.sewer_price, baselines: d.baselines, prices: d.prices, enabled: d.enabled, reading_day: d.reading_day },
    };
  }

  function restore(data: Record<string, any>) {
    d.name = data.name ?? '';
    d.tenant_name = data.tenant_name ?? '';
    d.tz = data.tz_offset_hours ?? d.tz;
    d.base_date = data.base_date ?? '';
    d.proof_required = data.proof_required ?? true;
    const ui = data._ui ?? {};
    d.enabled = { ...d.enabled, ...(ui.enabled ?? {}) };
    d.baselines = { ...d.baselines, ...(ui.baselines ?? {}) };
    d.prices = { ...d.prices, ...(ui.prices ?? {}) };
    d.sewer_on = ui.sewer_on ?? true;
    d.sewer_price = ui.sewer_price ?? '';
    d.reading_day = ui.reading_day ?? '';
  }

  onMount(async () => {
    // часовой пояс по умолчанию — как на устройстве, если он есть в списке
    const local = -new Date().getTimezoneOffset() / 60;
    if (ZONES.some((z) => z[0] === local)) d.tz = local;
    try {
      const r = await api.get<{ draft: { step: number; data: Record<string, any>; property_id: number | null } | null }>('/setup/draft');
      if (r.draft && r.draft.property_id === null) {
        restore(r.draft.data);
        step = Math.min(r.draft.step, TOTAL - 1);
        resumed = step > 0 || !!d.name;
      }
    } catch {
      /* нет черновика — начинаем с нуля */
    }
    loaded = true;
  });

  async function save(to: number) {
    // «Далее» только сохраняет черновик на сервере — квартира не создаётся до «Создать»
    await api.put('/setup/draft', { step: to, data: payload() });
  }

  function validate(s: number): boolean {
    const e: Record<string, string> = {};
    if (s === 0 && !d.name.trim()) e.name = 'Укажите название или адрес';
    if (s === 1 && !CODES.some((c) => d.enabled[c])) e.enabled = 'Включите хотя бы один счётчик';
    if (s === 2) for (const c of CODES) if (on(c) && !isValidNumber(d.baselines[c])) e[`b_${c}`] = 'Нужно число';
    if (s === 3) {
      for (const c of CODES) if (on(c) && !isValidNumber(d.prices[c])) e[`p_${c}`] = 'Нужно число';
      if (d.sewer_on && !isValidNumber(d.sewer_price)) e.sewer = 'Нужно число';
    }
    if (s === 4 && d.reading_day.trim() && !/^([1-9]|1\d|2[0-8])$/.test(d.reading_day.trim())) e.day = 'Число от 1 до 28';
    errors = e;
    return Object.keys(e).length === 0;
  }

  async function go(to: number) {
    if (to > step && !validate(step)) return;
    busy = true;
    try {
      await save(to);
      step = to;
      window.scrollTo({ top: 0 });
    } catch (e) {
      notify(errorText(e, t('genericError'), t('offline')), 'err');
    } finally {
      busy = false;
    }
  }

  async function finish() {
    busy = true;
    try {
      await save(TOTAL - 1);
      const r = await api.post<{ property_id: number }>('/setup/finish');
      // сначала приложение узнаёт о новой квартире (список, выбранный объект), потом показываем приглашение
      await oncreated(r.property_id);
      created = r.property_id;
      tg.haptic('success');
    } catch (e) {
      notify(errorText(e, t('genericError'), t('offline')), 'err');
      tg.haptic('error');
    } finally {
      busy = false;
    }
  }

  async function makeInvite() {
    busy = true;
    try {
      invite = await api.post<{ url: string }>('/invitations');
    } catch (e) {
      notify(errorText(e, t('genericError'), t('offline')), 'err');
    } finally {
      busy = false;
    }
  }
</script>

{#if !loaded}
  <div class="h-40 animate-pulse rounded-2xl bg-surface"></div>
{:else if created !== null}
  <Card class="mt-4 grid gap-3 text-center">
    <div class="mx-auto grid size-14 place-items-center rounded-full bg-ok/15 text-3xl text-ok">✓</div>
    <h1 class="text-xl font-bold">{t('wizard.done')}</h1>
    <p class="text-sm text-muted">{t('wizard.doneHint')}</p>
    {#if invite}
      <p class="num break-all rounded-xl bg-surface2 p-3 text-left text-sm">{invite.url}</p>
      <Btn onclick={() => tg.share(invite!.url, 'RentFlow')}>{t('property.share')}</Btn>
    {:else}
      <Btn {busy} onclick={makeInvite}>{t('property.invite')}</Btn>
    {/if}
    <Btn variant="secondary" onclick={() => onfinished(created!)}>{invite ? t('wizard.open') : t('wizard.later')}</Btn>
  </Card>
{:else}
  <div class="grid gap-3 pb-6">
    <div>
      <h1 class="text-xl font-bold">{t('wizard.title')}</h1>
      <div class="mt-2 flex items-center gap-3">
        <div class="h-2 flex-1 overflow-hidden rounded-full bg-surface2"><div class="h-full rounded-full bg-brand transition-all" style="width:{((step + 1) / TOTAL) * 100}%"></div></div>
        <span class="num text-sm text-muted">{t('wizard.step', { n: step + 1, total: TOTAL })}</span>
      </div>
      <p class="mt-1 text-sm font-semibold">{t(`wizard.s${step}` as Key)}</p>
    </div>
    {#if resumed && step > 0}<p class="rounded-lg bg-brand/10 px-3 py-2 text-sm text-brand">{t('wizard.resume')}</p>{/if}

    {#if step === 0}
      <Card class="grid gap-3">
        <Field label={t('property.name')} bind:value={d.name} error={errors.name} placeholder="Москва, Волжский бульвар 25к1" />
        <Field label={t('wizard.tenantOptional')} bind:value={d.tenant_name} />
        <label class="block">
          <span class="mb-1 block text-sm text-muted">{t('wizard.tz')}</span>
          <select class="min-h-12 w-full rounded-xl border border-line bg-bg px-3 text-fg" bind:value={d.tz}>
            {#each ZONES as [v, name] (v)}<option value={v}>{name}</option>{/each}
          </select>
        </label>
      </Card>
    {:else if step === 1}
      <Card class="grid gap-2">
        <p class="text-sm text-muted">{t('wizard.metersHint')}</p>
        {#each CODES as c (c)}
          <label class="flex items-center justify-between gap-3 border-t border-line py-3 first:border-t-0">
            <span>{label(c)}</span>
            <input type="checkbox" class="size-6 accent-[var(--brand)]" bind:checked={d.enabled[c]} />
          </label>
        {/each}
        {#if errors.enabled}<p class="text-sm text-bad">{errors.enabled}</p>{/if}
      </Card>
    {:else if step === 2}
      <Card class="grid gap-3">
        <p class="text-sm text-muted">{t('wizard.baseHint')}</p>
        {#each CODES.filter(on) as c (c)}
          <Field label={label(c)} bind:value={d.baselines[c]} inputmode="decimal" suffix={unit(c)} error={errors[`b_${c}`]} />
        {/each}
        <Field label={t('property.baseDate')} bind:value={d.base_date} type="date" hint={t('property.baseDateHint')} />
        <p class="text-xs text-muted">{t('wizard.baseSource')}</p>
      </Card>
    {:else if step === 3}
      <Card class="grid gap-3">
        <p class="text-sm text-muted">{t('wizard.pricesHint')}</p>
        {#each CODES.filter(on) as c (c)}
          <Field label={label(c)} bind:value={d.prices[c]} inputmode="decimal" suffix={c.startsWith('t') ? t('price.kwh') : t('price.m3')} error={errors[`p_${c}`]} />
        {/each}
        <label class="flex items-center justify-between gap-3 py-1">
          <span>{t('property.sewer')}</span>
          <input type="checkbox" class="size-6 accent-[var(--brand)]" bind:checked={d.sewer_on} />
        </label>
        {#if d.sewer_on}<Field label={t('reg.sewer')} bind:value={d.sewer_price} inputmode="decimal" suffix={t('price.m3')} error={errors.sewer} />{/if}
      </Card>
    {:else if step === 4}
      <Card class="grid gap-3">
        <Field label={t('wizard.dayOptional')} bind:value={d.reading_day} inputmode="numeric" hint={t('wizard.dayHint')} error={errors.day} placeholder="25" />
        <label class="flex items-center justify-between gap-3">
          <span>{t('property.proof')}</span>
          <input type="checkbox" class="size-6 accent-[var(--brand)]" bind:checked={d.proof_required} />
        </label>
      </Card>
    {:else}
      <Card class="grid gap-2">
        <h2 class="font-semibold">{t('wizard.review')}</h2>
        <p class="text-sm"><b>{d.name}</b>{d.tenant_name ? ` · ${d.tenant_name}` : ''}</p>
        {#each CODES.filter(on) as c (c)}
          <div class="flex justify-between border-t border-line py-2 text-sm"><span>{label(c)}</span><span class="num">{d.baselines[c]} {unit(c)} · {d.prices[c]} ₽</span></div>
        {/each}
        <div class="flex justify-between border-t border-line py-2 text-sm"><span>{t('reg.sewer')}</span><span class="num">{d.sewer_on ? `${d.sewer_price} ₽` : t('tariffs.off')}</span></div>
        <div class="flex justify-between border-t border-line py-2 text-sm"><span>{t('wizard.dayOptional')}</span><span>{d.reading_day.trim() || '—'}</span></div>
      </Card>
    {/if}

    <div class="grid gap-2">
      {#if step < TOTAL - 1}
        <Btn {busy} onclick={() => go(step + 1)}>{t('wizard.next')}</Btn>
      {:else}
        <Btn {busy} onclick={finish}>{t('wizard.create')}</Btn>
      {/if}
      {#if step > 0}<Btn variant="secondary" disabled={busy} onclick={() => go(step - 1)}>{t('wizard.back')}</Btn>{/if}
      {#if oncancel}<Btn variant="ghost" disabled={busy} onclick={oncancel}>{t('wizard.cancel')}</Btn>{/if}
    </div>
  </div>
{/if}
