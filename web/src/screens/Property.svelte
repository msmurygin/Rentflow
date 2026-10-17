<script lang="ts">
  import Async from '../components/Async.svelte';
  import Btn from '../components/Btn.svelte';
  import Card from '../components/Card.svelte';
  import Field from '../components/Field.svelte';
  import { api, errorText } from '../lib/api';
  import { dateLong, isValidNumber, num } from '../lib/format';
  import { t, type Key } from '../lib/i18n';
  import { loader } from '../lib/load.svelte';
  import { currentRole, notify, session } from '../lib/store.svelte';
  import { tg } from '../lib/tg';
  import type { Property } from '../lib/types';

  const landlord = $derived(currentRole() === 'landlord');
  let none = $state(false);
  const l2 = loader(async () => {
    const p = (await api.get<{ property: Property | null }>('/property')).property;
    none = p === null;
    return p ?? ({} as Property);
  });

  let name = $state('');
  let tenant = $state('');
  let readingDay = $state('');
  let errors = $state<Record<string, string>>({});
  let busy = $state(false);

  let editing = $state(false);
  let invite = $state<{ url: string; ttl_days: number } | null>(null);

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
      <Card><p class="text-muted">{t('home.noProperty')}</p></Card>
    {:else}
      <div class="grid gap-3 pb-4">
        <div class="flex items-start justify-between gap-3">
          <div>
            <h1 class="text-xl font-bold">{p.name}</h1>
            <p class="text-sm text-muted">{p.tenant_name} · {p.tenant_connected ? t('property.connected') : t('property.notConnected')}</p>
          </div>
          {#if landlord}
            <button class="text-sm font-semibold text-brand" onclick={() => { name = p.name; tenant = p.tenant_name; readingDay = p.reading_day ? String(p.reading_day) : ''; errors = {}; editing = !editing; }}>{t('property.edit')}</button>
          {/if}
        </div>

        {#if editing}
          <Card class="grid gap-3">
            <Field label={t('property.name')} bind:value={name} />
            <Field label={t('property.tenant')} bind:value={tenant} />
            <Field label={t('property.readingDay')} bind:value={readingDay} inputmode="numeric" hint={t('property.readingDayHint')} error={errors.readingDay} />
            <Btn {busy} onclick={() => { if (readingDay.trim() && !/^([1-9]|1\d|2[0-8])$/.test(readingDay.trim())) { errors.readingDay = 'Число от 1 до 28'; return; } errors.readingDay = ''; void patch({ name, tenant_name: tenant, reading_day: readingDay.trim() ? Number(readingDay) : 0 }); }}>{t('save')}</Btn>
          </Card>
        {/if}

        <Card>
          <p class="text-sm">{p.reading_day ? t('property.readingDayShow', { day: p.reading_day }) : t('property.readingDayNone')}</p>
        </Card>

        <Card>
          <h2 class="mb-2 font-semibold">{t('property.registers')}</h2>
          {#each p.registers as r (r.code)}
            <div class="flex items-center justify-between border-t border-line py-3 first:border-t-0">
              <div>
                <div>{t(`reg.${r.code}` as Key)}</div>
                <div class="text-xs text-muted">
                  {r.last && r.last.period !== '0000-00' ? `${t('property.last')} · ${dateLong(r.last.read_on)}` : `${t('property.baseline')} · ${dateLong(r.baseline_date)}`}
                </div>
                {#if r.last && r.last.period !== '0000-00'}
                  <div class="text-xs text-muted">{t('property.baseline')}: {num(r.baseline)}</div>
                {/if}
              </div>
              <div class="num text-lg font-semibold">{num(r.last?.value ?? r.baseline)} <span class="text-sm font-normal text-muted">{r.unit}</span></div>
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
