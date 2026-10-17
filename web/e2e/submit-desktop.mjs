// Условия Telegram Desktop: окно 426×770, мышь (не касание), обязательные фото. Полный путь как у человека.
import { chromium } from 'playwright-core';
import fs from 'node:fs';
const BASE = 'http://127.0.0.1:8099';
const OUT = process.argv[2] ?? '/tmp/desk';
fs.mkdirSync(OUT, { recursive: true });
const j = async (m, p, t, b) => { const r = await fetch(BASE + '/api' + p, { method: m, headers: { 'content-type': 'application/json', ...(t ? { authorization: 'Bearer ' + t } : {}) }, body: b ? JSON.stringify(b) : undefined }); return r.json(); };
const login = async (u) => (await j('POST', '/auth/telegram', null, { init_data: await (await fetch(`${BASE}/dev/init?user=${u}`)).text() })).token;
const owner = await login(1);
await j('POST', '/register', owner, {}); // владелец регистрируется сам (v0.3)
await j('PUT', '/property', owner, { name: 'Кв', tenant_name: 'И', baselines: { cold: '1', hot: '1', t1: '1', t2: '1' }, prices: { cold: '1', hot: '1', t1: '1', t2: '1' }, sewer_price: '1', proof_required: true });
const inv = (await j('POST', '/invitations', owner, {})).url.split('start=')[1];
await fetch(`${BASE}/dev/accept?user=2&token=${inv}`);
const b = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium-1194/chrome-linux/chrome', args: ['--no-sandbox'] });
const ctx = await b.newContext({ viewport: { width: 426, height: 700 }, deviceScaleFactor: 1 });
await ctx.route('**/telegram-web-app.js', (r) => r.abort());
const initData = await (await fetch(`${BASE}/dev/init?user=2`)).text();
await ctx.addInitScript((initData) => { window.Telegram = { WebApp: { initData, colorScheme: 'light', ready() {}, expand() {}, onEvent() {}, BackButton: { show() {}, hide() {}, onClick() {}, offClick() {} } } }; }, initData);
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
page.on('console', (m) => m.type() === 'error' && !m.text().includes('ERR_FAILED') && errors.push(m.text()));
await page.goto(BASE);
await page.getByRole('button', { name: 'Показания', exact: true }).click();
await page.getByText('Текущее показание').first().waitFor();
// файлы фото
const prep = await ctx.newPage(); await prep.goto('about:blank');
const files = [];
for (let i = 0; i < 4; i++) { const d = await prep.evaluate((i) => { const c = document.createElement('canvas'); c.width = 200; c.height = 100; const x = c.getContext('2d'); x.fillStyle = `hsl(${i * 80},50%,40%)`; x.fillRect(0, 0, 200, 100); return c.toDataURL('image/jpeg', 0.9).split(',')[1]; }, i); const f = `${OUT}/m${i}.jpg`; fs.writeFileSync(f, Buffer.from(d, 'base64')); files.push(f); }
await prep.close();
const cards = page.locator('section').filter({ hasText: 'Текущее показание' });
const values = ['10', '11', '12', '13'];
for (let i = 0; i < 4; i++) {
  const input = cards.nth(i).locator('input[inputmode=decimal]');
  await input.click();
  await page.keyboard.type(values[i]);
  await page.keyboard.press('Tab');           // как человек: перешёл к следующему полю
  await cards.nth(i).locator('input[type=file]').setInputFiles(files[i]);
  await cards.nth(i).locator('img').waitFor();
}
await page.screenshot({ path: `${OUT}/before-click.png` });
const btn = page.getByRole('button', { name: /Проверить и отправить/ });
console.log('кнопка:', await btn.innerText(), '| disabled:', await btn.isDisabled(), '| видима:', await btn.isVisible());
const box = await btn.boundingBox();
console.log('координаты кнопки:', JSON.stringify(box), 'высота окна 700');
// что реально лежит под курсором в центре кнопки?
const top = await page.evaluate(([x, y]) => { const e = document.elementFromPoint(x, y); return e ? e.tagName + ' ' + (e.textContent || '').slice(0, 30) : 'null'; }, [box.x + box.width / 2, box.y + box.height / 2]);
console.log('элемент под центром кнопки:', top);
await btn.click();
await page.waitForTimeout(1200);
console.log('сводка открылась:', await page.getByText('Проверьте показания').isVisible());
await page.screenshot({ path: `${OUT}/after-click.png` });
console.log('ошибки страницы:', errors.length ? errors : 'нет');
await b.close();
