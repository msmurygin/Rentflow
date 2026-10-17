import { mount } from 'svelte';
import './app.css';
import App from './App.svelte';

// Любая необработанная ошибка видна человеку (а не «ничего не происходит») — и по тексту можно найти причину.
import { notify } from './lib/store.svelte';
let lastError = '';
const report = (msg: string) => {
  if (msg === lastError) return;
  lastError = msg;
  notify(`Ошибка приложения: ${msg}`, 'err');
};
window.addEventListener('error', (e) => report(e.message));
window.addEventListener('unhandledrejection', (e) => report(String((e.reason as Error)?.message ?? e.reason)));

export default mount(App, { target: document.getElementById('app')! });
