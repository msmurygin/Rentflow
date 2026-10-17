<script lang="ts">
  import Async from '../components/Async.svelte';
  import Btn from '../components/Btn.svelte';
  import Card from '../components/Card.svelte';
  import Field from '../components/Field.svelte';
  import { api, errorText } from '../lib/api';
  import { dateLong, isValidNumber, num } from '../lib/format';
  import { t, type Key } from '../lib/i18n';
  import { loader } from '../lib/load.svelte';
  import { notify, session } from '../lib/store.svelte';
  import { tg } from '../lib/tg';
  import type { Property } from '../lib/types';

  const landlord = $derived(session.me?.role === 'landlord');
  const codes = ['cold', 'hot', 't1', 't2'] as const;
  let none = $state(false);
  const l2 = loader(async () => {
    const p = (await api.get<{ property: Property | null }>('/property')).property;
    none = p === null;
    return p ?? ({} as Property);
  });

  let name = $state('');
  let tenant = $state('');
  let baseDate = $state('');
  let baselines = $state<Record<string, string>>({ cold: '', hot: '', t1: '', t2: '' });
  let prices = $state<Record<string, string>>({ cold: '', hot: '', t1: '', t2: '' });
  let sewerOn = $state(true);
  let sewerPrice = $state('');
  let proof = $state(true);
  let errors = $state<Record<string, string>>({});
  let busy = $state(false);

  let editing = $state(false);
  let invite = $state<{ url: string; ttl_days: number } | null>(null);

  function validate(): boolean {
    const e: Record<string, string> = {};
    if (!name.trim()) e.name = 'Укажите название';
    if (!tenant.trim()) e.tenant = 'Укажите имя арендатора';
    for (const c of codes) {
      if (!isValidNumber(baselines[c])) e[`b_${c}`] = 'Нужно число';
      if (!isValidNumber(prices[c])) e[`p_${c}`] = 'Нужно число';
    }
    if (sewerOn && !isValidNumber(sewerPrice)) e.sewer = 'Нужно число';
    errors = e;
    return Object.keys(e).length === 0;
  }

  async function create() {
    if (!validate()) return;
    busy = true;
    try {
      await api.put('/property', {
        name,
        tenant_name: tenant,
        base_date: baseDate || null,
        baselines,
        prices,
        sewer_price: sewerOn ? sewerPrice : null,
        proof_required: proof,
      });
      if (session.me) session.me.property_configured = true;
      tg.haptic('success');
      notify(t('save'));
      await l2.reload();
    } catch (e) {
      notify(errorText(e, t('genericError'), t('offline')), 'err');
    } finally {
      busy = false;
    }
  }

  async function patch(body: Record<string, unknown>) {
    busy = true;
    try {
      await api.put('/property', body);
      await l2.reload();
      editing = false;
    } catch (e) {
      notify(errorText(e, t('genericError'), t('offline')), 'err');
    } finally {
      busy = false;
    }
  }

  async function makeInvite() {
    busy = true;
    try {
      invite = await api.post('/invitations');
    } catch (e) {
      notify(errorText(e, t('genericError'), t('offline')), 'err');
    } finally {
      busy = false;
    }
  }

  async function copy() {
    if (!invite) return;
    try {
      await navigator.clipboard.writeText(invite.url);
      notify(t('property.copied'));
    } catch {
      notify(invite.url);
    }
  }
</script>

<Async l={l2}>
  {#snippet children(p)}
    {#if none}
      {#if landlord}
        <div class="grid gap-3 pb-4">
          <h1 class="text-xl font-bold">{t('property.create')}</h1>
          <Card class="grid gap-3">
            <Field label={t('property.name')} bind:value={name} error={errors.name} placeholder="Квартира на Тверской" />
            <Field label={t('property.tenant')} bind:value={tenant} error={errors.tenant} />
            <Field label={t('property.baseDate')} bind:value={baseDate} type="date" hint={t('property.baseDateHint')} />
          </Card>
          <Card class="grid gap-3">
            <div>
              <h2 class="font-semibold">{t('property.baselines')}</h2>
              <p class="text-sm text-muted">{t('property.baselinesHint')}</p>
            </div>
            {#each codes as c (c)}
              <Field label={t(`reg.${c}` as Key)} bind:value={baselines[c]} inputmode="decimal" error={errors[`b_${c}`]} suffix={c.startsWith('t') ? t('unit.kwh') : t('unit.m3')} />
            {/each}
          </Card>
          <Card class="grid gap-3">
            <h2 class="font-semibold">{t('property.prices')}</h2>
            {#each codes as c (c)}
              <Field label={t(`reg.${c}` as Key)} bind:value={prices[c]} inputmode="decimal" error={errors[`p_${c}`]} suffix={c.startsWith('t') ? t('price.kwh') : t('price.m3')} />
            {/each}
            <label class="flex items-center justify-between gap-3 py-1">
              <span>{t('property.sewer')}</span>
              <input type="checkbox" class="size-6 accent-[var(--brand)]" bind:checked={sewerOn} />
            </label>
            {#if sewerOn}
              <Field label={t('reg.sewer')} bind:value={sewerPrice} inputmode="decimal" error={errors.sewer} suffix={t('price.m3')} />
            {/if}
          </Card>
          <Card>
            <label class="flex items-center justify-between gap-3">
              <span>{t('property.proof')}</span>
              <input type="checkbox" class="size-6 accent-[var(--brand)]" bind:checked={proof} />
            </label>
          </Card>
          <Btn {busy} onclick={create}>{t('property.create')}</Btn>
        </div>
      {:else}
        <Card><p class="text-muted">{t('home.noProperty')}</p></Card>
      {/if}
    {:else}
      <div class="grid gap-3 pb-4">
        <div class="flex items-start justify-between gap-3">
          <div>
            <h1 class="text-xl font-bold">{p.name}</h1>
            <p class="text-sm text-muted">{p.tenant_name} · {p.tenant_connected ? t('property.connected') : t('property.notConnected')}</p>
          </div>
          {#if landlord}
            <button class="text-sm font-semibold text-brand" onclick={() => { name = p.name; tenant = p.tenant_name; editing = !editing; }}>{t('property.edit')}</button>
          {/if}
        </div>

        {#if editing}
          <Card class="grid gap-3">
            <Field label={t('property.name')} bind:value={name} />
            <Field label={t('property.tenant')} bind:value={tenant} />
            <Btn {busy} onclick={() => patch({ name, tenant_name: tenant })}>{t('save')}</Btn>
          </Card>
        {/if}

        <Card>
          <h2 class="mb-2 font-semibold">{t('property.registers')}</h2>
          {#each p.registers as r (r.code)}
            <div class="flex items-center justify-between border-t border-line py-3 first:border-t-0">
              <div>
                <div>{t(`reg.${r.code}` as Key)}</div>
                <div class="text-xs text-muted">{t('property.baseline')} · {dateLong(r.baseline_date)}</div>
              </div>
              <div class="num text-lg font-semibold">{num(r.baseline)} <span class="text-sm font-normal text-muted">{r.unit}</span></div>
            </div>
          {/each}
        </Card>

        {#if landlord}
          <Card>
            <label class="flex items-center justify-between gap-3">
              <span>{t('property.proof')}</span>
              <input type="checkbox" class="size-6 accent-[var(--brand)]" checked={p.proof_required} onchange={(e) => patch({ proof_required: e.currentTarget.checked })} />
            </label>
          </Card>

          {#if !p.tenant_connected}
            <Card class="grid gap-3">
              {#if invite}
                <h2 class="font-semibold">{t('property.inviteLink')}</h2>
                <p class="num break-all rounded-xl bg-surface2 p-3 text-sm">{invite.url}</p>
                <p class="text-sm text-muted">{t('property.inviteHint', { days: invite.ttl_days })}</p>
                <Btn onclick={() => invite && tg.share(invite.url, 'RentFlow')}>{t('property.share')}</Btn>
                <Btn variant="secondary" onclick={copy}>{t('property.copy')}</Btn>
              {:else}
                <Btn {busy} onclick={makeInvite}>{t('property.invite')}</Btn>
              {/if}
            </Card>
          {/if}
        {/if}
      </div>
    {/if}
  {/snippet}
</Async>
