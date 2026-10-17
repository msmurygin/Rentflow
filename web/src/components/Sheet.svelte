<script lang="ts">
  import type { Snippet } from 'svelte';
  import { t } from '../lib/i18n';
  import Btn from './Btn.svelte';
  let {
    title,
    text = '',
    confirmLabel,
    danger = false,
    busy = false,
    disabled = false,
    onconfirm,
    oncancel,
    children,
  }: {
    title: string;
    text?: string;
    confirmLabel: string;
    danger?: boolean;
    busy?: boolean;
    disabled?: boolean;
    onconfirm: () => void;
    oncancel: () => void;
    children?: Snippet;
  } = $props();
</script>

<div class="fixed inset-0 z-40 flex items-end bg-black/50" role="presentation" onclick={oncancel}>
  <div
    class="max-h-[85vh] w-full overflow-y-auto rounded-t-3xl bg-surface p-5 pb-[calc(1.25rem+env(safe-area-inset-bottom))]"
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.key === 'Escape' && oncancel()}
  >
    <h2 class="text-lg font-semibold">{title}</h2>
    {#if text}<p class="mt-2 text-sm text-muted">{text}</p>{/if}
    {#if children}<div class="mt-3">{@render children()}</div>{/if}
    <div class="mt-5 grid gap-2">
      <Btn variant={danger ? 'danger' : 'primary'} {busy} {disabled} onclick={onconfirm}>{confirmLabel}</Btn>
      <Btn variant="ghost" disabled={busy} onclick={oncancel}>{t('cancel')}</Btn>
    </div>
  </div>
</div>
