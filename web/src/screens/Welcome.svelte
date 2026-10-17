<script lang="ts">
  import Btn from '../components/Btn.svelte';
  import Card from '../components/Card.svelte';
  import Field from '../components/Field.svelte';
  import { api, errorText } from '../lib/api';
  import { t } from '../lib/i18n';
  import { tokenFrom } from '../lib/format';
  import { notify } from '../lib/store.svelte';

  let { onchange }: { onchange: () => void } = $props();
  let busy = $state(false);
  let tenant = $state(false);
  let link = $state('');

  async function registerOwner() {
    busy = true;
    try {
      await api.post('/register');
      onchange();
    } catch (e) {
      notify(errorText(e, t('genericError'), t('offline')), 'err');
    } finally {
      busy = false;
    }
  }

  async function join() {
    const token = tokenFrom(link);
    if (!token) return;
    busy = true;
    try {
      await api.post('/invitations/accept', { token });
      onchange();
    } catch (e) {
      notify(errorText(e, t('welcome.badInvite'), t('offline')), 'err');
    } finally {
      busy = false;
    }
  }
</script>

<div class="grid gap-4 pt-6">
  <div class="text-center">
    <div class="mx-auto mb-3 grid size-16 place-items-center rounded-2xl bg-brand text-3xl text-brandfg">⌂</div>
    <h1 class="text-2xl font-bold">{t('welcome.title')}</h1>
    <p class="mt-2 text-sm text-muted">{t('welcome.text')}</p>
  </div>
  <Btn {busy} onclick={registerOwner}>🏠 {t('welcome.owner')}</Btn>
  {#if !tenant}
    <Btn variant="secondary" onclick={() => (tenant = true)}>🔑 {t('welcome.tenant')}</Btn>
  {:else}
    <Card class="grid gap-3">
      <p class="text-sm text-muted">{t('welcome.tenantHint')}</p>
      <Field label={t('welcome.invite')} bind:value={link} placeholder="https://t.me/…?start=…" />
      <Btn {busy} disabled={!link.trim()} onclick={join}>{t('welcome.join')}</Btn>
    </Card>
  {/if}
</div>
