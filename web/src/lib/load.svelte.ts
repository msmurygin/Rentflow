import { errorText } from './api';
import { t } from './i18n';

/** Загрузка данных экрана: идёт / ошибка (с повтором) / готово. */
export function loader<T>(fn: () => Promise<T>) {
  let data = $state<T | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  async function reload() {
    loading = true;
    error = null;
    try {
      data = await fn();
    } catch (e) {
      error = errorText(e, t('genericError'), t('offline'));
    } finally {
      loading = false;
    }
  }
  void reload();
  return {
    get data() {
      return data;
    },
    set data(v: T | null) {
      data = v;
    },
    get error() {
      return error;
    },
    get loading() {
      return loading;
    },
    reload,
  };
}
export type Loader<T> = ReturnType<typeof loader<T>>;
