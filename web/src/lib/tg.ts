// Тонкая обёртка над Telegram.WebApp. Вне Telegram (обычный браузер) всё это безопасно ничего не делает.

interface WebApp {
  initData: string;
  colorScheme: 'light' | 'dark';
  ready(): void;
  expand(): void;
  close(): void;
  setHeaderColor?(c: string): void;
  setBackgroundColor?(c: string): void;
  onEvent(name: string, cb: () => void): void;
  openTelegramLink(url: string): void;
  BackButton: { show(): void; hide(): void; onClick(cb: () => void): void; offClick(cb: () => void): void };
  HapticFeedback?: { impactOccurred(s: string): void; notificationOccurred(s: string): void };
  disableVerticalSwipes?(): void;
}

const wa = (): WebApp | undefined => (window as unknown as { Telegram?: { WebApp?: WebApp } }).Telegram?.WebApp;

export const tg = {
  get available() {
    return !!wa()?.initData;
  },
  get initData() {
    return wa()?.initData ?? '';
  },
  init() {
    const w = wa();
    if (!w) return;
    w.ready();
    w.expand();
    w.disableVerticalSwipes?.();
  },
  scheme(): 'light' | 'dark' {
    return wa()?.colorScheme ?? (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
  },
  onThemeChange(cb: () => void) {
    wa()?.onEvent('themeChanged', cb);
    window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', cb);
  },
  paint(bg: string) {
    wa()?.setHeaderColor?.(bg);
    wa()?.setBackgroundColor?.(bg);
  },
  /** Кнопка «Назад» Telegram: показывается, пока открыт вложенный экран. */
  backButton(visible: boolean, cb: () => void): () => void {
    const b = wa()?.BackButton;
    if (!b) return () => {};
    if (visible) {
      b.show();
      b.onClick(cb);
    } else b.hide();
    return () => b.offClick(cb);
  },
  haptic(kind: 'success' | 'error' | 'light' = 'light') {
    const h = wa()?.HapticFeedback;
    if (!h) return;
    if (kind === 'light') h.impactOccurred('light');
    else h.notificationOccurred(kind);
  },
  share(url: string, text: string) {
    const link = `https://t.me/share/url?url=${encodeURIComponent(url)}&text=${encodeURIComponent(text)}`;
    const w = wa();
    if (w) w.openTelegramLink(link);
    else window.open(link, '_blank');
  },
};
