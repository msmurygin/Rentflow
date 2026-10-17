import { ru, type Key } from './locales_ru';

// Язык один, но все тексты идут через t(): добавить локаль — значит добавить файл и выбрать словарь здесь.
const dict: Record<Key, string> = ru;

export function t(key: Key, vars: Record<string, string | number> = {}): string {
  let s: string = dict[key] ?? key;
  for (const [k, v] of Object.entries(vars)) s = s.replaceAll(`{${k}}`, String(v));
  return s;
}

export type { Key };
