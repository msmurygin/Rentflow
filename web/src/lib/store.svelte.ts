import type { Me } from './types';

export type Tab = 'home' | 'property' | 'readings' | 'tariffs' | 'bills';

export const session = $state<{ me: Me | null }>({ me: null });
export const nav = $state<{ tab: Tab; billId: number | null }>({ tab: 'home', billId: null });
export const toast = $state<{ text: string; kind: 'ok' | 'err'; id: number }>({ text: '', kind: 'ok', id: 0 });

let timer: ReturnType<typeof setTimeout> | undefined;
export function notify(text: string, kind: 'ok' | 'err' = 'ok') {
  toast.text = text;
  toast.kind = kind;
  toast.id++;
  clearTimeout(timer);
  timer = setTimeout(() => (toast.text = ''), 3200);
}

export function go(tab: Tab, billId: number | null = null) {
  nav.tab = tab;
  nav.billId = billId;
  window.scrollTo({ top: 0 });
}
