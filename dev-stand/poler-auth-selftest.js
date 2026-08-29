#!/usr/bin/env node
'use strict';
/*!
 * poler-auth-selftest.js — SELF-TEST poler-engine --google-auth (настоящий путь движка):
 *   1) Chromium на CDP 9223 с профилем движка (движок переиспользует его для обмена)
 *   2) poler-engine --google-auth в фоне → ловим consent URL из stdout
 *   3) навигация на URL, авто-клики (аккаунт → Разрешить)
 *   4) редирект на loopback движка → движок ловит код → обмен → google_tokens.json
 * Токены НЕ печатаются. Челлендж Google (если вдруг) — печатается цифрой и ждём.
 */
const { spawn } = require('child_process');
const fs = require('fs');
const M = require('/home/z/my-project/scripts/gcp-cdp-machinery.js');

const ENGINE = '/home/z/my-project/poler-engine-gh/target/debug/poler-engine';
const LOG = '/home/z/my-project/logs/poler-auth-selftest.log';
const CDP = 9223;
const CHROME = '/home/z/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome';

const sleep = M.sleep;

(async () => {
  fs.writeFileSync(LOG, '');
  // 1) браузер на порту движка
  const { child } = await M.launchChromium();
  // launchChromium берёт случайный порт — поднимем свой на 9223
  try { child.kill('SIGKILL'); } catch (_) {}
  await sleep(1500);
  const chrome = spawn(CHROME, [
    '--user-data-dir=/home/z/.cache/poler-engine/google-profile',
    '--remote-debugging-port=' + CDP,
    '--remote-debugging-address=127.0.0.1',
    '--no-first-run', '--no-default-browser-check', '--no-sandbox',
    '--headless=new', '--disable-gpu', '--hide-crash-restore-bubble',
    '--window-size=1280,900', 'about:blank',
  ], { stdio: 'ignore' });
  for (let i = 0; i < 50; i++) {
    await sleep(400);
    try { await M.getJsonPort(CDP, '/json/version'); break; } catch (_) {}
  }
  M.log('браузер на CDP :' + CDP + ' (pid ' + chrome.pid + ')');

  // 2) движок в фоне
  const eng = spawn(ENGINE, ['--google-auth'], {
    env: { ...process.env, POLER_CHROME_BIN: CHROME },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let out = '';
  eng.stdout.on('data', (d) => { out += d; fs.appendFileSync(LOG, d); });
  eng.stderr.on('data', (d) => { fs.appendFileSync(LOG, d); });
  M.log('движок запущен (pid ' + eng.pid + ')');

  // 3) ждём consent URL
  let url = '';
  for (let i = 0; i < 50; i++) {
    const m = /https:\/\/accounts\.google\.com\/o\/oauth2\/v2\/auth\?[^\s]+/.exec(out);
    if (m) { url = m[0]; break; }
    await sleep(400);
  }
  if (!url) {
    M.log('✗ движок не напечатал URL. Вывод:\n' + out.slice(0, 1500));
    try { eng.kill('SIGKILL'); } catch (_) {}
    try { chrome.kill('SIGKILL'); } catch (_) {}
    process.exit(1);
  }
  M.log('✓ consent URL получен (' + url.length + ' симв.)');

  // 4) навигация + авто-клики
  let cdp = await M.connectPageRetry(CDP, 4);
  try { await cdp.call('Page.navigate', { url }, 10000); } catch (_) {}
  try { cdp.ws.close(); } catch (_) {}
  await sleep(4000);
  cdp = await M.connectPageRetry(CDP, 4);

  let done = false;
  for (let i = 0; i < 150 && !done; i++) {
    let v = '';
    try {
      const r = await cdp.call('Runtime.evaluate', { expression: `(() => {
        const h = location.href;
        if (h.startsWith('http://127.0.0.1:')) return 'REDIRECTED';
        const body = document.body ? document.body.innerText : '';
        if (/подтвердите свою личность|verify it.?s you/i.test(body)) {
          const nums = body.split('\\n').map(s => s.trim()).filter(l => /^\\d{2}$/.test(l));
          return 'CHALLENGE|' + (nums.join(',') || 'CODE') + '|' + body.slice(0, 120).replace(/\\s+/g, ' ');
        }
        if (/выберите аккаунт|choose an account/i.test(body)) {
          const hits = [...document.querySelectorAll('body *')].filter(e => /vitalijkotok18@gmail\\.com/.test(e.innerText || ''));
          if (hits.length) {
            const deepest = hits.reduce((a, b) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_CONTAINED_BY) ? b : a);
            deepest.click();
            return 'ACCOUNT_CLICKED';
          }
        }
        const btns = [...document.querySelectorAll('button, div[role=button], input[type=submit]')];
        const exact = btns.find(b => /^(разрешить|allow|продолжить|continue|далее|next)$/i.test((b.innerText || b.value || '').trim()));
        if (exact) { exact.click(); return 'CLICK:' + (exact.innerText || exact.value || '').trim().slice(0, 25); }
        return 'WAIT|' + document.title.slice(0, 40);
      })()`, returnByValue: true, awaitPromise: true }, 15000);
      v = (r.result && r.result.value) || '';
    } catch (e) {
      try { cdp = await M.connectPageRetry(CDP, 3); } catch (_) {}
      continue;
    }
    if (v === 'REDIRECTED') { M.log('✓ редирект на loopback движка — код у движка'); done = true; break; }
    if (v === 'ACCOUNT_CLICKED') M.log('аккаунт выбран');
    else if (v.startsWith('CLICK')) M.log('consent: ' + v);
    else if (v.startsWith('CHALLENGE')) {
      const parts = v.split('|');
      M.log('⚠ ЧЕЛЛЕНДЖ GOOGLE: ' + (parts[1] || '?') + ' — ' + (parts[2] || '').slice(0, 100));
    }
    else if (i % 25 === 0) M.log('… ' + v.slice(0, 60));
    await sleep(2000);
  }

  if (!done) M.log('⚠ consent не дошёл до редиректа за таймаут — движок сам выйдет по 180с');

  // 5) ждём завершения движка (обмен + сохранение токенов)
  const rc = await new Promise((resolve) => {
    const to = setTimeout(() => { try { eng.kill('SIGKILL'); } catch (_) {} resolve('timeout'); }, 120000);
    eng.on('exit', (code) => { clearTimeout(to); resolve(code); });
  });
  M.log('движок завершился: exit=' + rc);

  // 6) результат
  const tail = out.split('\n').filter(Boolean).slice(-12).join('\n');
  console.log('════ ВЫВОД ДВИЖКА (хвост) ════');
  console.log(tail);
  const tok = '/home/z/.config/poler-engine/google_tokens.json';
  if (fs.existsSync(tok)) {
    const st = fs.statSync(tok);
    const j = JSON.parse(fs.readFileSync(tok, 'utf8'));
    console.log('════ ТОКЕНЫ ════');
    console.log('google_tokens.json: ' + st.size + ' байт, права ' + (st.mode & 0o777).toString(8));
    console.log('refresh_token: ' + (j.refresh_token ? 'есть (' + j.refresh_token.length + ' симв.)' : 'НЕТ'));
    console.log('scope: ' + (j.scope || '?'));
    console.log('════ SELF-TEST: ' + (j.refresh_token ? '✅ ПРОЙДЕН' : '⚠ без refresh') + ' ════');
  } else {
    console.log('google_tokens.json НЕ создан');
  }
  try { chrome.kill('SIGKILL'); } catch (_) {}
  setTimeout(() => process.exit(0), 1500);
})().catch((e) => {
  console.error('ОШИБКА: ' + e.message);
  process.exit(1);
});
