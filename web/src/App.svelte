<script lang="ts">
  import { onMount } from 'svelte';
  import BottomNav from './components/BottomNav.svelte';
  import Btn from './components/Btn.svelte';
  import PropertyBar from './components/PropertyBar.svelte';
  import Toast from './components/Toast.svelte';
  import { ApiError, api, onReauth, setToken } from './lib/api';
  import { t } from './lib/i18n';
  import { applyMe, go, nav, selectProperty, session } from './lib/store.svelte';
  import { tg } from './lib/tg';
  import type { Me } from './lib/types';
  import BillView from './screens/BillView.svelte';
  import Bills from './screens/Bills.svelte';
  import Home from './screens/Home.svelte';
  import Property from './screens/Property.svelte';
  import Readings from './screens/Readings.svelte';
  import Tariffs from './screens/Tariffs.svelte';
  import Welcome from './screens/Welcome.svelte';
  import Wizard from './screens/Wizard.svelte';

  let boot = $state<'loading' | 'ready' | 'closed' | 'error'>('loading');
  /** Владелец добавляет ещё одну квартиру поверх уже работающего приложения. */
  let adding = $state(false);
  /** Квартира только что создана: мастер остаётся на экране, пока человек не пригласит арендатора или не закроет его. */
  let finishing = $state(false);

  async function login() {
    const r = await api.post<{ token: string }>('/auth/telegram', { init_data: tg.initData });
    setToken(r.token);
  }

  async function refreshMe() {
    applyMe(await api.get<Me>('/me'));
  }

  async function start() {
    boot = 'loading';
    if (!tg.available) {
      boot = 'closed';
      return;
    }
    try {
      await login();
      await refreshMe();
      // приглашение пришло параметром запуска: арендатор подключается без лишних шагов
      if (session.me?.role === 'new' && tg.startParam) {
        try {
          await api.post('/invitations/accept', { token: tg.startParam });
          await refreshMe();
        } catch {
          /* ссылка устарела — человек увидит экран приветствия и сможет вставить новую */
        }
      }
      boot = 'ready';
    } catch (e) {
      boot = e instanceof ApiError && e.status === 0 ? 'error' : 'error';
    }
  }

  function applyTheme() {
    const dark = tg.scheme() === 'dark';
    document.documentElement.dataset.theme = dark ? 'dark' : 'light';
    tg.paint(dark ? '#0e1015' : '#f2f3f7');
  }

  onMount(() => {
    tg.init();
    applyTheme();
    tg.onThemeChange(applyTheme);
    onReauth(login);
    void start();
  });

  // Кнопка «Назад» Telegram: открыт вложенный экран (счёт) — возвращаемся к списку.
  $effect(() => {
    const nested = nav.billId !== null;
    return tg.backButton(nested, () => go('bills'));
  });

  const stage = $derived(
    !session.me ? 'none' : session.me.role === 'new' ? 'welcome' : session.me.properties.length === 0 || adding || finishing ? 'wizard' : 'app',
  );

  async function created(id: number) {
    finishing = true;
    await refreshMe();
    selectProperty(id);
  }
</script>

<main class="mx-auto min-h-dvh max-w-xl px-4 pt-4 pb-[calc(6rem+env(safe-area-inset-bottom))]">
  {#if boot === 'ready' && session.me}
    {#if stage === 'welcome'}
      <Welcome onchange={refreshMe} />
    {:else if stage === 'wizard'}
      <Wizard oncreated={created} onfinished={() => { adding = false; finishing = false; }} oncancel={session.me.properties.length ? () => (adding = false) : undefined} />
    {:else}
      <PropertyBar onadd={() => (adding = true)} />
      {#key `${session.epoch}:${nav.tab}:${nav.billId}`}
        {#if nav.tab === 'bills' && nav.billId !== null}
          <BillView id={nav.billId} />
        {:else if nav.tab === 'home'}
          <Home />
        {:else if nav.tab === 'property'}
          <Property />
        {:else if nav.tab === 'readings'}
          <Readings />
        {:else if nav.tab === 'tariffs'}
          <Tariffs />
        {:else}
          <Bills />
        {/if}
      {/key}
    {/if}
  {:else if boot === 'loading'}
    <div class="grid h-[70vh] place-items-center" aria-busy="true">
      <span class="size-8 animate-spin rounded-full border-4 border-brand border-t-transparent" aria-label={t('loading')}></span>
    </div>
  {:else}
    <div class="grid h-[70vh] place-content-center gap-4 text-center">
      <div class="text-2xl font-bold">{t('appName')}</div>
      <p class="max-w-xs text-muted">
        {boot === 'closed' ? (tg.scriptLoaded ? t('openFromMenu') : t('scriptBlocked')) : t('offline')}
      </p>
      {#if boot === 'error'}<Btn variant="secondary" onclick={start}>{t('retry')}</Btn>{/if}
    </div>
  {/if}
</main>

{#if boot === 'ready' && stage === 'app'}<BottomNav />{/if}
<Toast />
