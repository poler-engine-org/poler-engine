#!/usr/bin/env node
'use strict';
/*!
 * gcp-branding-audience.js — финальные штрихи OAuth-настройки:
 *   A) Branding: App name → «POLER Engine» (это имя на consent-экране)
 *   B) Audience: добавить test user vitalijkotok18@gmail.com
 * Один запуск Chromium, терпеливые reload-ретраи (Google рейт-лимитит).
 */
const M = require('/home/z/my-project/scripts/gcp-cdp-machinery.js');
const log = M.log;
const sleep = M.sleep;

const APP_NAME = 'POLER Engine';
const TEST_EMAIL = 'vitalijkotok18@gmail.com';
const URL_BRANDING = 'https://console.cloud.google.com/auth/branding?project=verification-506705';
const URL_AUDIENCE = 'https://console.cloud.google.com/auth/audience?project=verification-506705';

const DEEPQ = `function deepAll(root, sel) {
  let out = [...root.querySelectorAll(sel)];
  for (const el of [...root.querySelectorAll('*')]) { if (el.shadowRoot) out = out.concat(deepAll(el.shadowRoot, sel)); }
  return out;
}`;
const DEEPTEXT = `(() => { let t = '';
  function walk(node) { if (!node) return; const tag = (node.tagName || '').toUpperCase(); if (tag === 'STYLE' || tag === 'SCRIPT' || tag === 'NOSCRIPT' || tag === 'LINK') return; if (node.shadowRoot) walk(node.shadowRoot); for (const k of (node.childNodes || [])) { if (k.nodeType === 3) { const v = (k.textContent || '').trim(); if (v) t += v + ' | '; } else if (k.nodeType === 1) walk(k); } }
  walk(document.body); return t; })()`;

async function navAndWait(ref, cdpPort, url, marker, tries) {
  for (let i = 0; i < (tries || 8); i++) {
    log('навигация (' + (i + 1) + '/' + tries + '): ' + url.split('?')[0].split('/').slice(-1)[0]);
    try { await ref.cdp.call('Page.navigate', { url }, 10000); } catch (_) {}
    try { ref.cdp.ws.close(); } catch (_) {}
    await sleep(7000);
    try { ref.cdp = await M.connectPageRetry(cdpPort, 4); } catch (_) { continue; }
    for (let j = 0; j < 6; j++) {
      const t = await M.evalRetry(ref, cdpPort, DEEPTEXT, 20000);
      if (marker.test(t)) { log('✓ страница загрузилась (маркер найден)'); return t; }
      if (/Failed to load/.test(t)) break;
      await sleep(3500);
    }
    log('⚠ не загрузилась — пауза 15с и retry');
    await sleep(15000);
    try { await ref.cdp.call('Page.reload', {}, 8000); await sleep(8000); } catch (_) {}
  }
  return '';
}

(async () => {
  log('запуск Chromium (профиль движка)…');
  const { child, cdpPort } = await M.launchChromium();
  log('Chromium CDP :' + cdpPort);
  const ref = { cdp: null };
  try {
    ref.cdp = await M.connectPageRetry(cdpPort, 4);

    /* ═══ A) Branding: App name → POLER Engine ═══ */
    log('═══ A) BRANDING ═══');
    let brandingText = await navAndWait(ref, cdpPort, URL_BRANDING, /App name|app information|Название приложения/i, 8);
    if (!brandingText) { log('⚠ Branding не открылся — пропускаю (не критично)'); }
    else {
      log('текущее состояние Branding: ' + brandingText.replace(/\s+/g, ' ').slice(0, 600));
      // ищем input «App name»
      const named = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ};
        const inputs = deepAll(document, 'input').filter(i => i.type !== 'hidden' && !/search/i.test(i.placeholder || ''));
        // берём первый текстовый инпут — на Branding это App name
        const inp = inputs[0];
        if (!inp) return 'NOINPUT';
        const cur = inp.value || '';
        const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
        setter.call(inp, '${APP_NAME}');
        inp.dispatchEvent(new Event('input', { bubbles: true }));
        inp.dispatchEvent(new Event('change', { bubbles: true }));
        return 'SET:' + cur + '→' + inp.value;
      })()`, 15000);
      log('app name: ' + named);
      await sleep(2000);
      // жмём Save (кнопка может быть в шапке/внизу)
      const saved = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ};
        const btns = deepAll(document, 'button, [role=button], cfc-button');
        const b = btns.find(x => /^(save|сохранить|save changes)$/i.test((x.innerText || x.textContent || '').trim()));
        if (!b) return 'NOBTN|' + btns.map(x => (x.innerText || '').trim().slice(0, 15)).filter(Boolean).join(';').slice(0, 250);
        b.click(); return 'SAVED';
      })()`, 15000);
      log('save: ' + saved.slice(0, 300));
      await sleep(5000);
      const after = await M.evalRetry(ref, cdpPort, DEEPTEXT, 20000);
      log('после save: ' + (after.includes(APP_NAME) ? '✓ имя «' + APP_NAME + '» видно на странице' : '⚠ имени не видно: ' + after.replace(/\s+/g, ' ').slice(0, 300)));
    }

    /* ═══ B) Audience: test user ═══ */
    log('═══ B) AUDIENCE (test users) ═══');
    await sleep(10000);
    let audText = await navAndWait(ref, cdpPort, URL_AUDIENCE, /test users|тестировщики|Test users/i, 8);
    if (!audText) { log('⚠ Audience не открылся'); }
    else {
      log('состояние Audience: ' + audText.replace(/\s+/g, ' ').slice(0, 700));
      // email владельца в списке тест-юзеров?
      if (new RegExp(TEST_EMAIL.replace('.', '\\.')).test(audText)) {
        log('✓ test user уже в списке: ' + TEST_EMAIL);
      } else {
        // жмём «+ Add users»
        const addClicked = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ};
          const btns = deepAll(document, 'button, [role=button], cfc-button, a[role=button]');
          const b = btns.find(x => /add users|добавить пользователей|\\+ ?add/i.test((x.innerText || x.textContent || '') + (x.getAttribute && x.getAttribute('aria-label') || '')));
          if (!b) return 'NOBTN|' + btns.map(x => (x.innerText || '').trim().slice(0, 15)).filter(Boolean).join(';').slice(0, 250);
          b.click(); return 'ADD-CLICKED';
        })()`, 15000);
        log('add users: ' + addClicked.slice(0, 300));
        await sleep(3000);
        // вводим email в появившийся инпут
        const emailSet = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ};
          const inputs = deepAll(document, 'input').filter(i => i.type !== 'hidden' && !/search/i.test(i.placeholder || ''));
          const inp = inputs[inputs.length - 1];
          if (!inp) return 'NOINPUT';
          const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
          setter.call(inp, '${TEST_EMAIL}');
          inp.dispatchEvent(new Event('input', { bubbles: true }));
          inp.dispatchEvent(new Event('change', { bubbles: true }));
          return 'EMAIL:' + inp.value;
        })()`, 15000);
        log('email: ' + emailSet);
        await sleep(1500);
        // жмём Add в диалоге
        const addFinal = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ};
          const btns = deepAll(document, 'button, [role=button], cfc-button');
          const b = btns.find(x => /^(add|добавить)$/i.test((x.innerText || x.textContent || '').trim()));
          if (!b) return 'NOBTN|' + btns.map(x => (x.innerText || '').trim().slice(0, 15)).filter(Boolean).join(';').slice(0, 200);
          b.click(); return 'ADDED';
        })()`, 15000);
        log('add: ' + addFinal.slice(0, 250));
        await sleep(5000);
        const check = await M.evalRetry(ref, cdpPort, DEEPTEXT, 20000);
        log(check.includes(TEST_EMAIL) ? '✓ TEST USER ДОБАВЛЕН: ' + TEST_EMAIL : '⚠ не видно в списке: ' + check.replace(/\s+/g, ' ').slice(0, 400));
      }
    }

    log('═══ ИТОГ ═══');
    const finalBranding = await M.evalRetry(ref, cdpPort, DEEPTEXT, 20000);
    log('финальный дамп (кусок): ' + finalBranding.replace(/\s+/g, ' ').slice(0, 500));
  } finally {
    try { child.kill('SIGTERM'); } catch (_) {}
    setTimeout(() => { try { child.kill('SIGKILL'); } catch (_) {} }, 3000);
  }
})().catch((e) => { log('ОШИБКА: ' + e.message); process.exit(1); });
