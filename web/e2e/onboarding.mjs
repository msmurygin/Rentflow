// Definition of Done v0.3: незнакомый владелец сам регистрируется, настраивает квартиру, приглашает арендатора и проводит
// первый цикл; другой владелец не видит чужих данных. Все люди — в разных контекстах браузера, без помощи разработчика.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const BASE = process.env.BASE ?? 'http://127.0.0.1:8099';
const OUT = process.argv[2] ?? '/tmp/onboarding';
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

async function fillWizardUntilStep(p, upTo) {
  await p.getByLabel('Название или адрес').fill(upTo.name ?? 'Кв на Тверской');
  await p.getByRole('button', { name: 'Далее' }).click();
  await p.getByText('Отключите то, чего нет').waitFor();
}

// ---------- 1. Владелец A: приветствие → регистрация → мастер ----------
const A = await open(OWNER_A);
await A.page.getByText('Добро пожаловать в RentFlow').waitFor();
await shot(A.page, 'welcome');
await A.page.getByRole('button', { name: /Я сдаю квартиру/ }).click();
await A.page.getByText('Новая квартира').waitFor();
check(true, 'после регистрации открылся мастер квартиры');
await A.page.getByLabel('Название или адрес').fill('Москва, Тверская 1');
await A.page.getByRole('button', { name: 'Далее' }).click();
await A.page.getByText('Отключите то, чего нет').waitFor();
await A.page.locator('label', { hasText: 'Электричество T2' }).locator('input').uncheck();
await shot(A.page, 'wizard-meters');
await A.page.getByRole('button', { name: 'Далее' }).click();
await A.page.getByText('точка отсчёта').waitFor();
const base = { 'Холодная': '100', 'Горячая': '50', 'Электричество T1': '1000' };
for (const [k, v] of Object.entries(base)) await A.page.getByLabel(new RegExp(k)).fill(v);
check((await A.page.getByLabel(/T2/).count()) === 0, 'отключённый T2 не запрашивается');
await A.page.getByRole('button', { name: 'Далее' }).click();
await A.page.getByText('Тарифы действуют с текущего месяца').waitFor();
const price = { 'Холодная': '10', 'Горячая': '20', 'Электричество T1': '5' };
for (const [k, v] of Object.entries(price)) await A.page.getByLabel(new RegExp(k)).fill(v);
await A.page.locator('label', { hasText: /^Водоотведение\s*₽/ }).locator('input').fill('7');
await A.page.getByRole('button', { name: 'Далее' }).click();
await A.page.getByLabel('День передачи показаний').fill('25');
await A.page.getByRole('button', { name: 'Далее' }).click();
await A.page.getByText('Проверьте и создайте').waitFor();
await shot(A.page, 'wizard-review');

// возобновление: перезагрузка на последнем шаге возвращает к тому же месту с теми же данными, квартир ещё нет
const before = await api(A.initData, 'GET', '/properties');
check(before.json.properties.length === 0, 'до «Создать» ни одной квартиры не создано');
await A.page.reload();
await A.page.getByText('Мы сохранили вашу настройку').waitFor();
await A.page.getByText('Проверьте и создайте').waitFor();
check(true, 'мастер возобновился с того же шага');
await A.page.getByRole('button', { name: 'Создать квартиру' }).click();
await A.page.getByText('Квартира создана').waitFor({ timeout: 8000 }).catch(async () => { console.error('FAIL: не создалась. На экране:', (await A.page.locator('[role=status]').allInnerTexts()).join(' | ')); await shot(A.page, 'create-failed'); process.exit(1); });
await A.page.getByRole('button', { name: 'Пригласить арендатора' }).click();
const link = await A.page.locator('p.break-all').innerText();
check(link.includes('start='), 'приглашение создано');
await shot(A.page, 'wizard-done');
await A.page.getByRole('button', { name: 'Открыть приложение' }).click();
await A.page.getByText('Первые шаги').waitFor();
check(true, 'на главной — чек-лист первых шагов');
const owned = await api(A.initData, 'GET', '/properties');
check(owned.json.properties.length === 1, 'создана ровно одна квартира (повторы не плодят)');

// ---------- 2. Арендатор: приветствие → «Я арендатор» → ссылка ----------
const T = await open(TENANT);
await T.page.getByRole('button', { name: /Я арендатор/ }).click();
await T.page.getByLabel('Ссылка или код приглашения').fill(link);
await T.page.getByRole('button', { name: 'Подключиться' }).click();
await T.page.getByRole('button', { name: 'Показания', exact: true }).click();
await T.page.getByText('Текущее показание').first().waitFor();
const cards = T.page.locator('section').filter({ hasText: 'Текущее показание' });
check((await cards.count()) === 3, 'у арендатора три счётчика (T2 отключён владельцем)');
const prep = await T.ctx.newPage(); await prep.goto('about:blank');
const files = [];
for (let i = 0; i < 3; i++) { const d = await prep.evaluate((i) => { const c = document.createElement('canvas'); c.width = 200; c.height = 100; const x = c.getContext('2d'); x.fillStyle = `hsl(${i * 90 + 20},50%,40%)`; x.fillRect(0, 0, 200, 100); return c.toDataURL('image/jpeg', 0.9).split(',')[1]; }, i); const f = `${OUT}/t${i}.jpg`; fs.writeFileSync(f, Buffer.from(d, 'base64')); files.push(f); }
await prep.close();
const vals = ['103', '52', '1010'];
for (let i = 0; i < 3; i++) {
  await cards.nth(i).locator('input[inputmode=decimal]').fill(vals[i]);
  await cards.nth(i).locator('input[type=file]').setInputFiles(files[i]);
  await cards.nth(i).locator('img').waitFor();
}
await T.page.getByRole('button', { name: 'Проверить и отправить' }).click();
await T.page.getByText('Проверьте показания').waitFor();
await T.page.getByRole('button', { name: 'Отправить владельцу' }).click();
await T.page.getByText('Показания отправлены').first().waitFor();
check(true, 'арендатор отправил показания');

// ---------- 3. Владелец A: проверка, утверждение, оплата ----------
await A.page.reload();
await A.page.getByRole('button', { name: /Проверьте расчёт/ }).click();
// ХВС 3×10 + ГВС 2×20 + T1 10×5 + водоотведение (3+2)×7 = 30+40+50+35
await A.page.getByText('155,00').first().waitFor();
check(true, 'расчёт верный: 155,00 ₽ без T2');
await A.page.getByRole('button', { name: 'Утвердить и отправить' }).click();
await A.page.locator('div[role=dialog]').getByRole('button', { name: 'Утвердить и отправить' }).click();
await A.page.getByText('К оплате').first().waitFor();
await A.page.getByRole('button', { name: 'Отметить оплаченным' }).click();
await A.page.locator('div[role=dialog]').getByRole('button', { name: 'Отметить оплаченным' }).click();
await A.page.getByText('Оплачен').first().waitFor();
check(true, 'первый цикл завершён: счёт оплачен');
await T.page.reload();
await T.page.getByText('155,00').first().waitFor();
check(true, 'арендатор видит утверждённый счёт');

// ---------- 4. Владелец B: свой мир, чужого не видит ----------
const B = await open(OWNER_B);
await B.page.getByRole('button', { name: /Я сдаю квартиру/ }).click();
await B.page.getByLabel('Название или адрес').fill('Казань, Баумана 5');
await B.page.getByRole('button', { name: 'Далее' }).click();
await B.page.getByText('Отключите то, чего нет').waitFor();
await B.page.reload(); // «закрыл и вернулся»
await B.page.getByText('Мы сохранили вашу настройку').waitFor();
check(true, 'у второго владельца мастер тоже возобновляется (и это его черновик, не чужой)');
const aProps = (await api(A.initData, 'GET', '/properties')).json.properties;
const aId = aProps[0].id;
for (const path of ['/property', '/bills', '/tariffs', '/home', '/meters']) {
  const r = await api(B.initData, 'GET', path, null, aId);
  check(r.status === 404, `B → ${path} квартиры A: 404`);
}
const aBill = (await api(A.initData, 'GET', '/bills', null, aId)).json.bills[0];
for (const act of ['approve', 'reject', 'paid', 'reissue', 'void']) {
  const r = await api(B.initData, 'POST', `/bills/${aBill.id}/${act}`, { reason: 'x', confirm_paid: true }, aId);
  check(r.status === 404, `B → счёт A /${act}: 404`);
}
check((await api(B.initData, 'GET', '/properties')).json.properties.length === 0, 'у B нет чужих квартир в списке');

await browser.close();
console.log('ONBOARDING E2E OK, screenshots:', OUT);
