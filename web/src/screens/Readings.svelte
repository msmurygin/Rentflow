<script lang="ts">
  import { onDestroy } from 'svelte';
  import Async from '../components/Async.svelte';
  import Btn from '../components/Btn.svelte';
  import Card from '../components/Card.svelte';
  import Sheet from '../components/Sheet.svelte';
  import { api, errorText } from '../lib/api';
  import { dateLong, isValidNumber, monthName, num, usage } from '../lib/format';
  import { t, type Key } from '../lib/i18n';
  import { loader } from '../lib/load.svelte';
  import { toJpeg } from '../lib/photo';
  import { go, notify, session } from '../lib/store.svelte';
  import { tg } from '../lib/tg';
  import type { FormView, Home, MeterRow } from '../lib/types';

  const landlord = $derived(session.me?.role === 'landlord');

  // ---- владелец: что уже передано в этом периоде ----
  const owner = loader(async () => (landlord ? await api.get<{ period: string; registers: MeterRow[] }>('/meters') : null));

  // ---- арендатор: форма ----
  let values = $state<Record<string, string>>({});
  let errors = $state<Record<string, string>>({});
  let uploading = $state<Record<string, boolean>>({});
  let thumbs = $state<Record<string, string>>({});
  let summary = $state(false);
  let sending = $state(false);
  let sent = $state(false);
  const urls: string[] = [];

  const form = loader(async () => {
    if (landlord) return null as unknown as FormView;
    const f = await api.post<FormView>('/readings/draft');
    sync(f);
    for (const r of f.registers) if (r.has_photo) void thumb(r.code);
    return f;
  });
  const home = loader(async () => (landlord ? null : await api.get<Home>('/home')));

  function sync(f: FormView) {
    for (const r of f.registers) if (r.value != null) values[r.code] = r.value.replace(/\.0+$/, '').replace('.', ',');
    form.data = f;
  }

  async function thumb(code: string) {
    const u = await api.photoUrl(`/readings/photo?code=${code}`);
    if (u) {
      urls.push(u);
      thumbs[code] = u;
    }
  }
  onDestroy(() => urls.forEach((u) => URL.revokeObjectURL(u)));

  async function commit(code: string) {
    const v = (values[code] ?? '').trim();
    if (!v) return;
    if (!isValidNumber(v)) {
      errors[code] = 'Нужно число, например 761 или 761,5';
      return;
    }
    try {
      sync(await api.put<FormView>('/readings/draft', { code, value: v }));
      errors[code] = '';
    } catch (e) {
      errors[code] = errorText(e, t('genericError'), t('offline'));
      tg.haptic('error');
    }
  }

  async function pick(code: string, ev: Event) {
    const input = ev.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file) return;
    uploading[code] = true;
    errors[code] = '';
    try {
      const f = await api.upload<FormView>(`/readings/photo?code=${code}`, await toJpeg(file));
      sync(f);
      await thumb(code);
      tg.haptic('light');
    } catch (e) {
      errors[code] = errorText(e, t('genericError'), t('offline'));
      tg.haptic('error');
    } finally {
      uploading[code] = false;
    }
  }

  async function send() {
    sending = true;
    try {
      await api.post('/readings/submit');
      tg.haptic('success');
      sent = true;
      summary = false;
    } catch (e) {
      notify(errorText(e, t('genericError'), t('offline')), 'err');
      tg.haptic('error');
    } finally {
      sending = false;
    }
  }

  const ready = (f: FormView) => f.complete && f.registers.every((r) => !errors[r.code]);
</script>

{#if landlord}
  <Async l={owner}>
    {#snippet children(d)}
      {#if d && d.period}
        <div class="grid gap-3 pb-4">
          <div>
            <h1 class="text-xl font-bold">{t('readings.ownerTitle')}</h1>
            <p class="text-sm text-muted">{monthName(d.period ?? '')}</p>
          </div>
          <Card>
            {#each d.registers as r (r.code)}
              <div class="flex items-center justify-between border-t border-line py-3 first:border-t-0">
                <div>
                  <div>{t(`reg.${r.code}` as Key)}</div>
                  <div class="text-xs text-muted">{t('readings.was')}: {r.previous ? `${num(r.previous.value)} · ${dateLong(r.previous.read_on)}` : '—'}</div>
                </div>
                <div class="num text-right">
                  {#if r.current}
                    <div class="text-lg font-semibold">{num(r.current)}</div>
                    <div class="text-xs text-ok">{t('readings.received')}</div>
                  {:else}
                    <div class="text-sm text-muted">{t('readings.waiting')}</div>
                  {/if}
                </div>
              </div>
            {/each}
          </Card>
          <Btn variant="secondary" onclick={() => go('bills')}>{t('nav.bills')}</Btn>
        </div>
      {:else}
        <Card><p class="text-muted">{t('home.noProperty')}</p></Card>
      {/if}
    {/snippet}
  </Async>
{:else}
  <Async l={form}>
    {#snippet children(f)}
      {#if sent || f.billed}
        <Card class="mt-6 text-center">
          <div class="mx-auto mb-3 grid size-14 place-items-center rounded-full bg-ok/15 text-3xl text-ok">✓</div>
          <h1 class="text-xl font-bold">{sent ? t('readings.sent') : t('readings.closed')}</h1>
          {#if sent}<p class="mt-2 text-sm text-muted">{t('readings.sentHint')}</p>{/if}
          {#if home.data?.next_due && !sent}<p class="mt-2 text-sm text-muted">{t('home.nextDue')}: {dateLong(home.data.next_due)}</p>{/if}
          <div class="mt-4"><Btn variant="secondary" onclick={() => go('home')}>{t('nav.home')}</Btn></div>
        </Card>
      {:else}
        <div class="grid gap-3 pb-28">
          <div>
            <h1 class="text-xl font-bold">{t('readings.title')}</h1>
            <p class="text-sm text-muted">{monthName(f.period)}</p>
          </div>

          {#each f.registers as r (r.code)}
            <Card class="grid gap-3">
              <div class="flex items-start justify-between gap-2">
                <div>
                  <h2 class="font-semibold">{t(`reg.${r.code}` as Key)}</h2>
                  {#if r.previous}<p class="num text-sm text-muted">{t('readings.was')}: {num(r.previous)} {r.unit}</p>{/if}
                </div>
                {#if r.value && (r.has_photo || !f.proof_required)}<span class="text-ok">✓</span>{/if}
              </div>

              <label class="block">
                <span class="mb-1 block text-sm text-muted">{t('readings.enter')}</span>
                <span class="flex items-center rounded-xl border bg-bg px-3 focus-within:border-brand {errors[r.code] ? 'border-bad' : 'border-line'}">
                  <input
                    class="num min-h-14 w-full bg-transparent text-2xl font-semibold text-fg outline-none"
                    inputmode="decimal"
                    autocomplete="off"
                    placeholder="0"
                    bind:value={values[r.code]}
                    onchange={() => commit(r.code)}
                    onfocus={(e) => { const el = e.currentTarget; setTimeout(() => el.scrollIntoView({ block: 'center', behavior: 'smooth' }), 250); }}
                  />
                  <span class="ml-2 text-sm text-muted">{r.unit}</span>
                </span>
                {#if errors[r.code]}<span class="mt-1 block text-sm text-bad">{errors[r.code]}</span>{/if}
              </label>

              {#if r.source === 'photo' && r.ai_value}
                <p class="rounded-lg bg-warn/10 px-3 py-2 text-sm text-warn">{t('readings.recognized', { v: num(r.ai_value) })}</p>
              {/if}

              <div class="flex items-center gap-3">
                {#if thumbs[r.code]}<img src={thumbs[r.code]} alt={r.label} class="size-16 rounded-lg object-cover" />{/if}
                <label class="flex min-h-12 flex-1 cursor-pointer items-center justify-center gap-2 rounded-xl bg-surface2 px-4 font-semibold {uploading[r.code] ? 'opacity-60' : ''}">
                  <svg class="size-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 8h3l2-3h6l2 3h3v11H4zM12 17a4 4 0 1 0 0-8 4 4 0 0 0 0 8z" /></svg>
                  {uploading[r.code] ? t('readings.uploading') : r.has_photo ? t('readings.retake') : t('readings.photo')}
                  <input type="file" accept="image/*" capture="environment" class="hidden" disabled={uploading[r.code]} onchange={(e) => pick(r.code, e)} />
                </label>
              </div>
              {#if f.proof_required && !r.has_photo && r.value}<p class="text-sm text-warn">{t('readings.photoRequired')}</p>{/if}
            </Card>
          {/each}
        </div>

        <div class="fixed inset-x-0 bottom-[calc(4.25rem+env(safe-area-inset-bottom))] z-20 border-t border-line bg-bg/95 p-3 backdrop-blur">
          <div class="mx-auto max-w-xl">
            <Btn disabled={!ready(f)} onclick={() => (summary = true)}>{ready(f) ? t('readings.review') : t('readings.incomplete')}</Btn>
          </div>
        </div>

        {#if summary}
          <Sheet title={t('readings.summary')} confirmLabel={t('readings.send')} busy={sending} onconfirm={send} oncancel={() => (summary = false)}>
            {#each f.registers as r (r.code)}
              <div class="flex items-center justify-between border-t border-line py-2.5 first:border-t-0">
                <div>
                  <div>{t(`reg.${r.code}` as Key)}</div>
                  <div class="num text-xs text-muted">{t('readings.usage')}: {r.previous && r.value ? `${usage(r.value, r.previous)} ${r.unit}` : '—'}</div>
                </div>
                <div class="num text-lg font-semibold">{num(r.value)}</div>
              </div>
            {/each}
          </Sheet>
        {/if}
      {/if}
    {/snippet}
  </Async>
{/if}
