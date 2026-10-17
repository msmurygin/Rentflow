import { setProperty } from './api';
import type { Me } from './types';

export type Tab = 'home' | 'property' | 'readings' | 'tariffs' | 'bills';

/** `epoch` растёт при смене квартиры: экраны пересоздаются и заново загружают данные нужного объекта. */
export const session = $state<{ me: Me | null; propertyId: number | null; epoch: number; demo: boolean }>({ me: null, propertyId: null, epoch: 0, demo: false });
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

/** Роль в выбранной квартире (у одного человека она может быть разной в разных квартирах). */
export function currentRole(): 'landlord' | 'tenant' | 'new' {
  const me = session.me;
  if (!me) return 'new';
  const p = me.properties.find((x) => x.id === session.propertyId);
  if (p) return p.role === 'owner' ? 'landlord' : 'tenant';
  return me.role === 'new' ? 'new' : 'landlord'; // владелец без квартир: мастер ещё не закончен
}

export function selectProperty(id: number | null) {
  session.propertyId = id;
  setProperty(id);
  session.epoch++;
  nav.tab = 'home';
  nav.billId = null;
}

/** После /me: выбираем активную квартиру (сохранённую сервером) и запоминаем роль. */
export function applyMe(me: Me) {
  session.me = me;
  const active = me.properties.find((p) => p.active) ?? me.properties[0];
  session.propertyId = active?.id ?? null;
  setProperty(session.propertyId);
}
