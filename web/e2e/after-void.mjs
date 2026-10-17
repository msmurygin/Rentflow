// История как у владельца: ввод с фото -> счёт -> оплата -> аннулирование -> арендатор вводит заново.
import { chromium } from 'playwright-core';
import fs from 'node:fs';
const BASE = 'http://127.0.0.1:8099';
const OUT = process.argv[2] ?? '/tmp/after-void';
fs.mkdirSync(OUT, { recursive: true });
const j = async (m, p, t, b, raw) => { const r = await fetch(BASE + '/api' + p, { method: m, headers: { ...(raw ? { 'content-type': 'image/jpeg' } : { 'content-type': 'application/json' }), ...(t ? { authorization: 'Bearer ' + t } : {}) }, body: raw ? raw : b ? JSON.stringify(b) : undefined }); return r.json().catch(() => ({})); };
const login = async (u) => (await j('POST', '/auth/telegram', null, { init_data: await (await fetch(`${BASE}/dev/init?user=${u}`)).text() })).token;
const jpeg = (seed) => Buffer.from([0xff, 0xd8, 0xff, 0xe0, seed, seed, seed, seed]);
const owner = await login(1); await j('POST', '/register', owner, {});
await j('PUT', '/property', owner, { name: 'Кв', tenant_name: 'И', baselines: { cold: '0', hot: '0', t1: '1', t2: '0.5' }, prices: { cold: '51', hot: '401', t1: '8', t2: '4' }, sewer_price: '60', proof_required: true });
const inv = (await j('POST', '/invitations', owner, {})).url.split('start=')[1];
await fetch(`${BASE}/dev/accept?user=2&token=${inv}`);
const tenant = await login(2);
// первый заход: значения и фото (как «забил давно»)
const first = { cold: '10', hot: '5', t1: '120', t2: '30' };
let n = 1;
for (const [c, v] of Object.entries(first)) {
  await j('POST', `/readings/photo?code=${c}`, tenant, null, jpeg(n++));
  console.log('PUT', c, JSON.stringify(await j('PUT', '/readings/draft', tenant, { code: c, value: v })).slice(0, 60));
}
console.log('submit:', JSON.stringify(await j('POST', '/readings/submit', tenant, {})));
const bills = (await j('GET', '/bills', owner)).bills; const id = bills[0].id;
console.log('approve', (await j('POST', `/bills/${id}/approve`, owner, {})).status, 'paid', (await j('POST', `/bills/${id}/paid`, owner, {})).status);
console.log('void', JSON.stringify(await j('POST', `/bills/${id}/void`, owner, { reason: 'тест', confirm_paid: true })));
console.log('форма после void:', JSON.stringify((await j('GET', '/readings/draft', tenant)).registers.map((r) => [r.code, r.value, r.has_photo])));

// теперь арендатор в браузере вводит заново
const b = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium-1194/chrome-linux/chrome', args: ['--no-sandbox'] });
const ctx = await b.newContext({ viewport: { width: 426, height: 700 } });
await ctx.route('**/telegram-web-app.js', (r) => r.abort());
const initData = await (await fetch(`${BASE}/dev/init?user=2`)).text();
await ctx.addInitScript((d) => { window.Telegram = { WebApp: { initData: d, colorScheme: 'light', ready() {}, expand() {}, onEvent() {}, BackButton: { show() {}, hide() {}, onClick() {}, offClick() {} } } }; }, initData);
const page = await ctx.newPage();
const errs = []; page.on('pageerror', (e) => errs.push(e.message)); page.on('console', (m) => m.type() === 'error' && !m.text().includes('ERR_FAILED') && errs.push(m.text()));
await page.goto(BASE);
await page.getByRole('button', { name: 'Показания', exact: true }).click();
await page.getByText('Текущее показание').first().waitFor();
await page.screenshot({ path: `${OUT}/1-form-after-void.png` });
const prep = await ctx.newPage(); await prep.goto('about:blank');
const files = [];
for (let i = 0; i < 4; i++) { const d = await prep.evaluate((i) => { const c = document.createElement('canvas'); c.width = 200; c.height = 100; const x = c.getContext('2d'); x.fillStyle = `hsl(${i * 70 + 10},50%,40%)`; x.fillRect(0, 0, 200, 100); return c.toDataURL('image/jpeg', 0.9).split(',')[1]; }, i); const f = `${OUT}/n${i}.jpg`; fs.writeFileSync(f, Buffer.from(d, 'base64')); files.push(f); }
await prep.close();
const cards = page.locator('section').filter({ hasText: 'Текущее показание' });
const vals = ['11', '6', '125', '33'];
for (let i = 0; i < 4; i++) {
  await cards.nth(i).locator('input[inputmode=decimal]').fill(vals[i]);
  await cards.nth(i).locator('input[type=file]').setInputFiles(files[i]);
  await cards.nth(i).locator('img').waitFor({ timeout: 8000 }).catch(() => console.log('нет превью у', i));
}
await page.screenshot({ path: `${OUT}/2-filled.png` });
const btn = page.getByRole('button', { name: /Проверить и отправить/ });
await btn.click();
await page.waitForTimeout(1500);
console.log('сводка:', await page.getByText('Проверьте показания').isVisible(), '| ошибки:', errs.length ? errs : 'нет');
const toast = await page.locator('[role=status]').allInnerTexts();
console.log('сообщение:', toast);
const sheet = await page.locator('div[role=dialog]').innerText();
const ok = ['11', '6', '125', '33'].every((v) => sheet.includes(v)) && !sheet.includes('755');
console.log('в сводке мои числа, а не подсказка ИИ:', ok);
if (!ok) { console.error('FAIL: сводка:', sheet.replace(/\n/g, ' | ')); process.exit(1); }
await page.screenshot({ path: `${OUT}/3-after-click.png` });
await b.close();
