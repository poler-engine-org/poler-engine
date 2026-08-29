#!/usr/bin/env node
'use strict';
/* gcp-e2e-enter-code.js — ввод Защитного кода (ootp) в форму challenge-страницы
 * и атомарный клик «Далее» (значение+клик одним eval — e2e-цикл не успеет помешать).
 * Использование: node gcp-e2e-enter-code.js <CDP-порт> <код> */
const M = require('/home/z/my-project/scripts/gcp-cdp-machinery.js');

const CDP = Number(process.argv[2] || 59707);
const CODE = String(process.argv[3] || '');

if (!CODE || !/^[0-9A-Za-z\\-]{4,12}$/.test(CODE)) {
  console.error('укажи код: node gcp-e2e-enter-code.js <порт> <код>');
  process.exit(1);
}

(async () => {
  const cdp = await M.connectPageRetry(CDP, 3);
  const expr = `(() => {
    const inp = document.querySelector('input[name=Pin], input[type=tel], input[name=code]');
    if (!inp) return 'NO_INPUT';
    // нативный сеттер + события — чтобы фреймворк Google увидел значение
    const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
    setter.call(inp, ${JSON.stringify(CODE)});
    inp.dispatchEvent(new Event('input', { bubbles: true }));
    inp.dispatchEvent(new Event('change', { bubbles: true }));
    const btns = [...document.querySelectorAll('button')];
    const next = btns.find(b => /^(далее|next)$/i.test((b.innerText || '').trim()));
    if (!next) return 'NO_NEXT_BUTTON';
    next.click();
    return 'ENTERED_AND_CLICKED';
  })()`;
  const r = await M.evalPage(cdp, expr, 15000);
  console.log('ввод кода: ' + r);
  await M.sleep(8000);
  const bodyExpr = `(() => ({
    href: location.href.slice(0, 160),
    body: (document.body ? document.body.innerText : '').split('\\n').filter(l => l.trim()).slice(0, 12).join('\\n')
  }))()`;
  const r2 = await M.evalPage(cdp, bodyExpr, 15000);
  console.log(JSON.stringify(r2, null, 2));
  try { cdp.ws.close(); } catch (_) {}
  process.exit(0);
})().catch((e) => { console.error('ОШИБКА: ' + e.message); process.exit(1); });
