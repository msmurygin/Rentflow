<script lang="ts">
  import { t, type Key } from '../lib/i18n';
  import { currentRole, go, nav, type Tab } from '../lib/store.svelte';

  const icons: Record<Tab, string> = {
    home: 'M3 11l9-8 9 8v9a1 1 0 0 1-1 1h-5v-6H9v6H4a1 1 0 0 1-1-1z',
    property: 'M4 21V5a1 1 0 0 1 1-1h8a1 1 0 0 1 1 1v16M14 9h5a1 1 0 0 1 1 1v11M8 8h2M8 12h2M8 16h2',
    readings: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zm0 4v5l3 2',
    tariffs: 'M4 7h16M4 12h16M4 17h10',
    bills: 'M6 3h12v18l-3-2-3 2-3-2-3 2zM9 8h6M9 12h6',
  };
  const tabs = $derived<Tab[]>(currentRole() === 'landlord' ? ['home', 'property', 'tariffs', 'bills'] : ['home', 'readings', 'bills']);
</script>

<nav
  class="fixed inset-x-0 bottom-0 z-30 border-t border-line bg-surface/95 pb-[env(safe-area-inset-bottom)] backdrop-blur"
  aria-label="Навигация"
>
  <ul class="mx-auto flex max-w-xl">
    {#each tabs as tab (tab)}
      <li class="flex-1">
        <button
          class="flex w-full flex-col items-center gap-0.5 py-2.5 text-[11px] font-medium {nav.tab === tab ? 'text-brand' : 'text-muted'}"
          aria-current={nav.tab === tab ? 'page' : undefined}
          onclick={() => go(tab)}
        >
          <svg class="size-6" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d={icons[tab]} /></svg>
          {t(`nav.${tab}` as Key)}
        </button>
      </li>
    {/each}
  </ul>
</nav>
