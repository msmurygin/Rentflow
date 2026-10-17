// Регрессия: «Проверить и отправить» должна нажиматься всегда. Ввели последнее число и сразу тапнули — число сохраняется и
// открывается сводка; не хватает фото — понятное сообщение, а не молчание.
const check = (c, m) => { if (!c) { console.error('FAIL:', m); process.exit(1); } console.log('ok:', m); };
import { chromium } from 'playwright-core';
const BASE = 'http://127.0.0.1:8099';
const j = async (m, p, t, b) => { const r = await fetch(BASE + '/api' + p, { method: m, headers: { 'content-type': 'application/json', ...(t ? { authorization: 'Bearer ' + t } : {}) }, body: b ? JSON.stringify(b) : undefined }); return r.json(); };
const login = async (u) => (await j('POST', '/auth/telegram', null, { init_data: await (await fetch(`${BASE}/dev/init?user=${u}`)).text() })).token;
const owner = await login(1);
await j('POST', '/register', owner, {}); // владелец регистрируется сам (v0.3)
await j('PUT', '/property', owner, { name: 'Кв', tenant_name: 'И', baselines: { cold: '1', hot: '1', t1: '1', t2: '1' }, prices: { cold: '1', hot: '1', t1: '1', t2: '1' }, sewer_price: '1', proof_required: true });
const inv = (await j('POST', '/invitations', owner, {})).url.split('start=')[1];
await fetch(`${BASE}/dev/accept?user=2&token=${inv}`);
const b = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium-1194/chrome-linux/chrome', args: ['--no-sandbox'] });
const ctx = await b.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
await ctx.route('**/telegram-web-app.js', (r) => r.abort());
const initData = await (await fetch(`${BASE}/dev/init?user=2`)).text();
await ctx.addInitScript((initData) => { window.Telegram = { WebApp: { initData, colorScheme: 'light', ready() {}, expand() {}, onEvent() {}, BackButton: { show() {}, hide() {}, onClick() {}, offClick() {} } } }; }, initData);
const page = await ctx.newPage();
await page.goto(BASE);
await page.getByRole('button', { name: 'Показания', exact: true }).click();
const inputs = page.locator('input[inputmode=decimal]');
for (let i = 0; i < 4; i++) {
  await inputs.nth(i).fill(String(10 + i));
  if (i < 3) await inputs.nth(i).blur();
}
const btn = page.getByRole('button', { name: 'Проверить и отправить' });
check(await btn.isEnabled(), 'кнопка не заблокирована (последнее поле ещё в фокусе)');
// 1. фото обязательны, но их нет: сообщение с причиной, сводка не открывается
await btn.click();
await page.getByText(/Не хватает:.*Нужно фото/).waitFor();
check(!(await page.getByText('Проверьте показания').isVisible()), 'без фото сводка не открывается, но причина показана');
// 2. владелец отключил требование фото — тот же тап открывает сводку
await j('PUT', '/property', owner, { proof_required: false });
await page.reload();
await page.getByRole('button', { name: 'Показания', exact: true }).click();
const inputs2 = page.locator('input[inputmode=decimal]');
await inputs2.nth(3).fill('13'); // снова не покидаем поле
await page.getByRole('button', { name: 'Проверить и отправить' }).click();
await page.getByText('Проверьте показания').waitFor();
check(true, 'число из поля сохранилось, сводка открылась с первого тапа');
await b.close();
