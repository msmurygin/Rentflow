// Суммы и показания приходят строками Decimal ("2121.21"). Здесь только форматирование строк —
// никакой арифметики на float: деньги считает сервер.

const NBSP = ' ';
const THIN = ' ';

function group(int: string): string {
  return int.replace(/\B(?=(\d{3})+(?!\d))/g, THIN);
}

/** "2121.2" → "2 121,20 ₽" */
export function money(s: string | null | undefined): string {
  if (s == null || s === '') return '—';
  const [i, f = ''] = s.split('.');
  return `${group(i)},${(f + '00').slice(0, 2)}${NBSP}₽`;
}

/** "3975.4800" → "3 975,48", "144" → "144" */
export function num(s: string | null | undefined): string {
  if (s == null || s === '') return '—';
  const [i, f = ''] = s.split('.');
  const frac = f.replace(/0+$/, '');
  return group(i) + (frac ? ',' + frac : '');
}

/** Цена за единицу: минимум два знака ("312.5" → "312,50"), больше — как есть ("8.625" → "8,625"). */
export function price(s: string | null | undefined): string {
  if (s == null || s === '') return '—';
  const [i, f = ''] = s.split('.');
  return `${group(i)},${f.length < 2 ? (f + '00').slice(0, 2) : f}`;
}

/** Целая часть: накопительные показания берутся без дробной части до вычитания. */
export function usage(current: string, previous: string): string {
  const whole = (s: string): bigint | null => {
    // запятая и пробелы — тоже допустимый ввод; «0,5» не должно ронять экран
    const t = s.trim().replace(/\s/g, '').replace(',', '.');
    return /^\d+(\.\d+)?$/.test(t) ? BigInt(t.split('.')[0]) : null;
  };
  const c = whole(current);
  const p = whole(previous);
  return c === null || p === null ? '—' : (c - p).toString();
}

export function isValidNumber(s: string): boolean {
  return /^\d+([.,]\d+)?$/.test(s.trim());
}

const MONTHS = ['Январь', 'Февраль', 'Март', 'Апрель', 'Май', 'Июнь', 'Июль', 'Август', 'Сентябрь', 'Октябрь', 'Ноябрь', 'Декабрь'];
const MONTHS_GEN = ['января', 'февраля', 'марта', 'апреля', 'мая', 'июня', 'июля', 'августа', 'сентября', 'октября', 'ноября', 'декабря'];

/** "2026-10" → "Октябрь 2026" */
export function monthName(period: string): string {
  const [y, m] = period.split('-');
  const name = MONTHS[Number(m) - 1];
  return name ? `${name} ${y}` : period;
}

/** "2026-10-16" → "16 октября 2026" */
export function dateLong(iso: string | null | undefined): string {
  if (!iso) return '—';
  const [y, m, d] = iso.slice(0, 10).split('-');
  const name = MONTHS_GEN[Number(m) - 1];
  return name ? `${Number(d)} ${name} ${y}` : iso;
}

export function addMonth(period: string, n: number): string {
  const [y, m] = period.split('-').map(Number);
  const idx = y * 12 + (m - 1) + n;
  return `${Math.floor(idx / 12)}-${String((idx % 12) + 1).padStart(2, '0')}`;
}

/** "2026-10" → "октября 2026" — для оборота «с октября 2026». */
export function monthFrom(period: string): string {
  const [y, m] = period.split('-');
  const name = MONTHS_GEN[Number(m) - 1];
  return name ? `${name} ${y}` : period;
}

/** Из ссылки t.me/…?start=ТОКЕН (или startapp=) достаём токен приглашения; голый токен принимаем как есть. */
export function tokenFrom(text: string): string {
  const m = text.match(/[?&](?:start|startapp)=([A-Za-z0-9_-]+)/);
  return (m ? m[1] : text).trim();
}
