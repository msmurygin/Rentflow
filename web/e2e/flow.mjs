// Сквозная проверка Mini App в настоящем браузере: владелец и арендатор в двух контекстах.
// Запуск: стенд `cargo run --example mini_dev -- web/dist 8099`, затем `node e2e/flow.mjs [outDir]`.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const BASE = process.env.BASE ?? 'http://127.0.0.1:8099';
const OUT = process.argv[2] ?? '/tmp/rentflow-shots';
fs.mkdirSync(OUT, { recursive: true });
const chromePath = process.env.CHROME ?? '/opt/pw-browsers/chromium-1194/chrome-linux/chrome';

const browser = await chromium.launch({ executablePath: chromePath, args: ['--no-sandbox'] });
let step = 0;
const fail = (m) => { throw new Error(m); };
const check = (c, m) => { if (!c) fail(m); console.log('ok:', m); };

async function openAs(userId, scheme = 'light') {
  const ctx = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, colorScheme: scheme });
  const initData = await (await fetch(`${BASE}/dev/init?user=${userId}`)).text();
  // настоящий telegram-web-app.js вне Telegram затёр бы заглушку — не даём ему загрузиться
  await ctx.route('https://telegram.org/**', (r) => r.abort());
  await ctx.addInitScript(({ initData, scheme }) => {
    window.__back = [];
    window.Telegram = { WebApp: { initData, colorScheme: scheme, ready() {}, expand() {}, close() {}, onEvent() {}, setHeaderColor() {}, setBackgroundColor() {}, disableVerticalSwipes() {},
      openTelegramLink(u) { window.__shared = u; },
      BackButton: { show() { window.__back.push('show'); }, hide() { window.__back.push('hide'); }, onClick() {}, offClick() {} },
      HapticFeedback: { impactOccurred() {}, notificationOccurred() {} } } };
  }, { initData, scheme });
  const page = await ctx.newPage();
  page.on('pageerror', (e) => fail('page error: ' + e.message));
  page.on('console', (m) => m.type() === 'error' && console.log('console.error:', m.text()));
  await page.goto(BASE);
  return page;
}
const shot = (page, name) => page.screenshot({ path: `${OUT}/${String(++step).padStart(2, '0')}-${name}.png` });

// 1. владелец: пустая главная → создание квартиры
const owner = await openAs(1);
await owner.getByText('Квартира ещё не настроена').waitFor();
await shot(owner, 'owner-home-empty');
await owner.getByRole('button', { name: 'Настройте квартиру →' }).click();
await owner.getByText('Создать квартиру').first().waitFor();
await owner.getByLabel('Название или адрес').fill('Квартира на Тверской');
await owner.getByLabel('Арендатор', { exact: true }).fill('Иван');
const nums = { 'Холодная вода (ХВС)': ['753', '65,77'], 'Горячая вода (ГВС)': ['416', '312,50'], 'Электричество T1': ['3831', '8,60'], 'Электричество T2': ['1118', '3,71'] };
for (const [label, [base, price]] of Object.entries(nums)) {
  const inputs = owner.getByLabel(label);
  await inputs.nth(0).fill(base);
  await inputs.nth(1).fill(price);
}
await owner.getByLabel('Водоотведение', { exact: false }).last().fill('51,62');
await shot(owner, 'owner-property-form');
await owner.getByRole('button', { name: 'Создать квартиру' }).click();
await owner.getByText('Квартира на Тверской').first().waitFor();
check(await owner.getByText('Не подключён').isVisible(), 'квартира создана, арендатор не подключён');
await owner.getByRole('button', { name: 'Пригласить арендатора' }).click();
const link = await owner.locator('p.break-all').innerText();
check(link.includes('start='), 'приглашение показано');
await shot(owner, 'owner-invite');
await owner.getByRole('button', { name: 'Отправить в Telegram' }).click();
check((await owner.evaluate(() => window.__shared))?.startsWith('https://t.me/share/url'), 'отправка через Telegram');

// 2. арендатор принимает приглашение (как это делает бот) и открывает приложение
const token = link.split('start=')[1].trim();
await fetch(`${BASE}/dev/accept?user=2&token=${token}`);
const tenant = await openAs(2);
await tenant.getByText('Передайте показания за Октябрь 2026').waitFor();
await shot(tenant, 'tenant-home');
await tenant.getByRole('button', { name: 'Показания', exact: true }).click();
await tenant.getByText('Холодная вода (ХВС)').first().waitFor();
await shot(tenant, 'tenant-readings-empty');

// 3. фото и показания
const prep = await tenant.context().newPage();
await prep.goto('about:blank');
const files = [];
for (let i = 0; i < 4; i++) {
  const data = await prep.evaluate((i) => { const c = document.createElement('canvas'); c.width = 300; c.height = 200; const x = c.getContext('2d'); x.fillStyle = `hsl(${i * 80},50%,40%)`; x.fillRect(0, 0, 300, 200); x.fillStyle = '#fff'; x.font = '48px sans-serif'; x.fillText('00' + (755 + i), 40, 120); return c.toDataURL('image/jpeg', 0.9).split(',')[1]; }, i);
  const f = `${OUT}/meter${i}.jpg`; fs.writeFileSync(f, Buffer.from(data, 'base64')); files.push(f);
}
await prep.close();
const cards = tenant.locator('section').filter({ hasText: 'Текущее показание' });
const values = ['755', '417', '3975,48', '1179'];
for (let i = 0; i < 4; i++) {
  const card = cards.nth(i);
  await card.locator('input[type=file]').setInputFiles(files[i]);
  await card.locator('img').waitFor();
  if (i === 0) {
    await card.getByText('Распознано: 755').waitFor();
    await shot(tenant, 'tenant-ai-hint');
  }
  await card.locator('input[inputmode=decimal]').fill(values[i]);
  await card.locator('input[inputmode=decimal]').blur();
  await card.getByText('✓').first().waitFor();
}
await shot(tenant, 'tenant-readings-filled');
await tenant.getByRole('button', { name: 'Проверить и отправить' }).click();
await tenant.getByText('Проверьте показания').waitFor();
check(await tenant.getByText('Расход: 144 кВт·ч').isVisible(), 'расход в сводке считается по целым частям');
const cancel = await tenant.locator('div[role=dialog]').getByRole('button', { name: 'Отмена' }).boundingBox();
check(cancel && cancel.y + cancel.height <= 844, 'нижняя кнопка шторки целиком на экране');
await shot(tenant, 'tenant-summary');
await tenant.getByRole('button', { name: 'Отправить владельцу' }).click();
await tenant.getByText('Показания отправлены').first().waitFor();
await shot(tenant, 'tenant-sent');

// 4. владелец: проверка расчёта, смена тарифа, утверждение
await owner.reload();
await owner.getByText('Проверьте расчёт за Октябрь 2026').waitFor();
await shot(owner, 'owner-home-review');
await owner.getByRole('button', { name: /Проверьте расчёт/ }).click();
await owner.getByText('2 063,61').first().waitFor();
await shot(owner, 'owner-bill-draft');
await owner.getByRole('button', { name: 'Тарифы' }).click();
await owner.getByRole('heading', { name: 'Тарифы' }).waitFor();
await owner.locator('section').filter({ hasText: 'Электричество T1' }).locator('input[inputmode=decimal]').fill('9');
await shot(owner, 'owner-tariffs');
await owner.getByRole('button', { name: /Применить с/ }).click();
await owner.getByText('Тарифы сохранены, черновик пересчитан').waitFor();
await owner.getByRole('button', { name: 'Счета' }).click();
await owner.getByText('2 121,21').first().waitFor();
check(true, 'черновик пересчитан до 2 121,21');
await owner.getByRole('button', { name: /Октябрь 2026/ }).first().click();
await owner.getByRole('button', { name: 'Утвердить и отправить' }).click();
await shot(owner, 'owner-approve-sheet');
await owner.locator('div[role=dialog]').getByRole('button', { name: 'Утвердить и отправить' }).click();
await owner.getByText('К оплате').first().waitFor();

// 5. арендатор видит счёт
await tenant.reload();
await tenant.getByText('2 121,21').first().waitFor();
await shot(tenant, 'tenant-home-bill');
await tenant.getByRole('button', { name: 'Счета' }).click();
await tenant.getByRole('button', { name: /Октябрь 2026/ }).click();
await tenant.getByText('Расчёт').first().waitFor();
check(!(await tenant.getByRole('button', { name: 'Утвердить и отправить' }).count()), 'у арендатора нет кнопок владельца');
await shot(tenant, 'tenant-bill');

// 6. владелец отмечает оплату; тёмная тема
await owner.reload();
await owner.getByRole('button', { name: 'Счета' }).click();
await owner.getByRole('button', { name: /Октябрь 2026/ }).first().click();
await owner.getByRole('button', { name: 'Отметить оплаченным' }).click();
await owner.locator('div[role=dialog]').getByRole('button', { name: 'Отметить оплаченным' }).click();
await owner.getByText('Оплачен').first().waitFor();
await shot(owner, 'owner-bill-paid');
const dark = await openAs(1, 'dark');
await dark.getByText('Квартира на Тверской').first().waitFor();
await shot(dark, 'owner-home-dark');
await dark.getByRole('button', { name: 'Тарифы' }).click();
await dark.getByRole('heading', { name: 'Тарифы' }).waitFor();
await shot(dark, 'owner-tariffs-dark');

// 7. не из Telegram
const plain = await browser.newPage({ viewport: { width: 390, height: 844 } });
await plain.route('https://telegram.org/**', (r) => r.abort());
await plain.goto(BASE);
await plain.getByText(/Не загрузился скрипт Telegram/).waitFor();
check(true, 'вне Telegram показывается подсказка');

await browser.close();
console.log('E2E OK, screenshots:', OUT);
