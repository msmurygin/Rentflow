// Демо: незнакомый человек нажимает «Посмотреть демо», видит квартиру со счетами, но ничего не может изменить.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const BASE = process.env.BASE ?? 'http://127.0.0.1:8099';
const OUT = process.argv[2] ?? '/tmp/demo-e2e';
fs.mkdirSync(OUT, { recursive: true });
const browser = await chromium.launch({ executablePath: process.env.CHROME ?? '/opt/pw-browsers/chromium-1194/chrome-linux/chrome', args: ['--no-sandbox'] });
let shots = 0;
const check = (c, m) => { if (!c) { console.error('FAIL:', m); process.exit(1); } console.log('ok:', m); };
const OWNER_A = 11, OWNER_B = 22, TENANT = 33;

async function open(userId) {
  const ctx = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true });
  await ctx.route('**/telegram-web-app.js', (r) => r.abort());
  const initData = await (await fetch(`${BASE}/dev/init?user=${userId}`)).text();
  await ctx.addInitScript((d) => { window.__shared = null; window.Telegram = { WebApp: { initData: d, colorScheme: 'light', ready() {}, expand() {}, close() {}, onEvent() {}, setHeaderColor() {}, setBackgroundColor() {}, disableVerticalSwipes() {}, openTelegramLink(u) { window.__shared = u; }, BackButton: { show() {}, hide() {}, onClick() {}, offClick() {} }, HapticFeedback: { impactOccurred() {}, notificationOccurred() {} } } }; }, initData);
  const page = await ctx.newPage();
  page.on('pageerror', (e) => check(false, 'ошибка страницы: ' + e.message));
  await page.goto(BASE);
  return { page, ctx, initData };
}
const shot = (p, n) => p.screenshot({ path: `${OUT}/${String(++shots).padStart(2, '0')}-${n}.png` });
const api = async (initData, method, path, body, property) => {
  const t = (await (await fetch(`${BASE}/api/auth/telegram`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ init_data: initData }) })).json()).token;
  const r = await fetch(`${BASE}/api${path}`, { method, headers: { 'content-type': 'application/json', authorization: `Bearer ${t}`, ...(property ? { 'x-property-id': String(property) } : {}) }, body: body ? JSON.stringify(body) : undefined });
  return { status: r.status, json: await r.json().catch(() => ({})) };
};


const U = await open(55);
await U.page.getByText('Добро пожаловать в RentFlow').waitFor();
await U.page.getByRole('button', { name: /Посмотреть демо/ }).click();
await U.page.getByText('Демо: можно смотреть, менять нельзя.').waitFor();
check(true, 'виден баннер демо');
await shot(U.page, 'demo-home');
await U.page.getByRole('button', { name: 'Счета', exact: true }).click();
await U.page.getByText(/Оплачен/).first().waitFor();
check(true, 'в демо есть оплаченный счёт');
await shot(U.page, 'demo-bills');
const own = await api(U.initData, 'GET', '/me');
check(own.status === 200 && own.json.properties.length === 0 && own.json.role === 'new', 'у настоящего пользователя демо-квартиры нет');
await U.page.getByRole('button', { name: 'Выйти из демо' }).click();
await U.page.getByText('Добро пожаловать в RentFlow').waitFor();
check(true, 'после выхода из демо снова приветствие');
await browser.close();
console.log('DEMO E2E OK, screenshots:', OUT);
