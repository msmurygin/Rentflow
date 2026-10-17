<script lang="ts">
  import { t, type Key } from '../lib/i18n';
  let { status, onBrand = false, overdue = false }: { status: string; onBrand?: boolean; overdue?: boolean } = $props();
  const shown = $derived(overdue && status === 'approved' ? 'overdue' : status);
  const tone: Record<string, string> = {
    draft: 'bg-warn/15 text-warn',
    approved: 'bg-brand/15 text-brand',
    paid: 'bg-ok/15 text-ok',
    rejected: 'bg-bad/15 text-bad',
    void: 'bg-surface2 text-muted',
    overdue: 'bg-bad/15 text-bad',
  };
</script>

<span class="inline-flex rounded-full px-2.5 py-0.5 text-xs font-semibold {onBrand ? 'bg-white/25 text-white' : (tone[status] ?? tone.void)}">
  {t(`status.${shown}` as Key)}
</span>
