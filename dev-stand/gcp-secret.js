#!/usr/bin/env node
'use strict';
/*!
 * gcp-secret.js v2 — поймать client secret OAuth-клиента POLER Engine.
 * v1 создала новый секрет (клик Add secret сработал), но диалог живёт в
 * shadow DOM (cfc-* компоненты) — innerText его не видит. Здесь:
 *   1) pierce shadow DOM при поиске секрета
 *   2) если секрет уже замаскирован — кликаем copy и читаем буфер обмена
 *      через Browser.grantPermissions + navigator.clipboard.readText()
 */
const fs = require('fs');
const path = require('path');
const os = require('os');

const CFG = path.join(os.homedir(), '.config', 'poler-engine');
const SECRET_FILE = path.join(CFG, 'client_secret.json');
const CLIENT_ID = '1033330882193-hb70kn51assup71b2paf1k4emaa2ab57.apps.googleusercontent.com';
const URL_CLIENT = `https://console.cloud.google.com/apis/credentials/oauthclient/${CLIENT_ID}?project=verification-506705`;

const M = require('/home/z/my-project/scripts/gcp-cdp-machinery.js');
const log = M.log;
const sleep = M.sleep;

// рекурсивный сбор текста с пробитием shadow DOM (без STYLE/SCRIPT)
const DEEP_TEXT = `(() => {
  let t = '';
  function walk(node) {
    if (!node) return;
    const tag = (node.tagName || '').toUpperCase();
    if (tag === 'STYLE' || tag === 'SCRIPT' || tag === 'NOSCRIPT' || tag === 'LINK') return;
    if (node.shadowRoot) walk(node.shadowRoot);
    const kids = (node.childNodes && node.childNodes.length) ? node.childNodes : (node.children || []);
    for (let i = 0; i < kids.length; i++) {
      const k = kids[i];
      if (k.nodeType === 3) { const v = (k.textContent || '').trim(); if (v) t += v + ' '; }
      else if (k.nodeType === 1) walk(k);
    }
  }
  walk(document.body);
  return t.replace(/\\s+/g, ' ');
})()`;

(async () => {
  log('запуск Chromium (профиль движка)…');
  const { child, cdpPort } = await M.launchChromium();
  log('Chromium CDP :' + cdpPort);
  const ref = { cdp: null };
  try {
    ref.cdp = await M.connectPageRetry(cdpPort, 4);
    log('навигация: карточка клиента');
    try { await ref.cdp.call('Page.navigate', { url: URL_CLIENT }, 10000); } catch (_) {}
    try { ref.cdp.ws.close(); } catch (_) {}
    await sleep(6000);
    ref.cdp = await M.connectPageRetry(cdpPort, 4);

    // даём странице прогрузиться; при «Failed to load» — reload
    for (let i = 0; i < 12; i++) {
      const t = await M.evalRetry(ref, cdpPort, 'document.body ? document.body.innerText.slice(0,400) : ""', 15000);
      if (/client for Desktop|Add secret/i.test(t)) { log('✓ карточка клиента загружена'); break; }
      if (/Failed to load|error while loading/i.test(t) && i < 8) {
        log('⚠ страница не загрузилась — reload');
        try { await ref.cdp.call('Page.reload', {}, 8000); } catch (_) {}
        await sleep(8000);
        continue;
      }
      await sleep(3000);
    }

    // 1) ПОПЫТКА: вдруг секрет уже виден где-то в shadow DOM (диалог ещё открыт)
    for (let i = 0; i < 6; i++) {
      const v = await M.evalRetry(ref, cdpPort, `(document.body.innerText.match(/GOCSPX-[A-Za-z0-9_-]{20,}/) || [''])[0]`, 15000);
      if (v && v.startsWith('GOCSPX-')) { await saveSecret(v, 'plain DOM'); return; }
      const deep = await M.evalRetry(ref, cdpPort, `(${DEEP_TEXT}.match(/GOCSPX-[A-Za-z0-9_-]{20,}/) || [''])[0]`, 20000);
      if (deep && deep.startsWith('GOCSPX-')) { await saveSecret(deep, 'shadow DOM'); return; }
      await sleep(2500);
    }
    log('секрет не висит в DOM — жму «Add secret» и ловлю диалог…');

    // 2) клик Add secret (с пробитием shadow DOM)
    const clicked = await M.evalRetry(ref, cdpPort, `(() => {
      function deepQueryAll(root, sel) {
        let out = [...root.querySelectorAll(sel)];
        const all = [...root.querySelectorAll('*')];
        for (const el of all) { if (el.shadowRoot) out = out.concat(deepQueryAll(el.shadowRoot, sel)); }
        return out;
      }
      const btns = deepQueryAll(document, 'button, [role=button], cfc-button');
      const b = btns.find(x => /add secret|добавить секрет/i.test((x.innerText || '') + ' ' + (x.getAttribute && x.getAttribute('aria-label') || '')));
      if (!b) return 'NOBTN';
      b.click(); return 'CLICKED';
    })()`, 20000);
    log('add-secret: ' + clicked);

    // 3) ловим секрет в диалоге (каждые 1.5с, до 45с), пробивая shadow DOM
    let secret = '';
    for (let i = 0; i < 30 && !secret; i++) {
      await sleep(1500);
      const v = await M.evalRetry(ref, cdpPort, `(${DEEP_TEXT}.match(/GOCSPX-[A-Za-z0-9_-]{20,}/) || [''])[0]`, 20000);
      if (v && v.startsWith('GOCSPX-')) { secret = v; break; }
    }
    if (secret) { await saveSecret(secret, 'диалог после Add secret'); return; }

    // 4) ФОЛЛБЭК: копируем НОВЫЙ секрет кнопкой copy → читаем буфер
    log('секрет в DOM не пойман — пробую copy-кнопку + чтение буфера обмена…');
    try {
      // разрешаем clipboard через CDP
      const ws = ref.cdp.ws;
      // grantPermissions идёт на browser-level WS; на page-level можно так:
      await ref.cdp.call('Browser.grantPermissions', { permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite'], origin: 'https://console.cloud.google.com' }, 8000).catch(() => {});
    } catch (_) {}
    const copied = await M.evalRetry(ref, cdpPort, `(async () => {
      function deepQueryAll(root, sel) {
        let out = [...root.querySelectorAll(sel)];
        for (const el of [...root.querySelectorAll('*')]) { if (el.shadowRoot) out = out.concat(deepQueryAll(el.shadowRoot, sel)); }
        return out;
      }
      // ищем контейнер НОВОГО секрета (рядом с бейджем NEW)
      const badges = deepQueryAll(document, '*').filter(x => x.childElementCount === 0 && /^NEW$/i.test((x.textContent || '').trim()));
      for (const badge of badges) {
        let row = badge.parentElement;
        for (let i = 0; i < 6 && row; i++) {
          const copyBtn = [...(row.querySelectorAll ? row.querySelectorAll('button, [role=button], cfc-button') : [])].find(b => /copy|копир/i.test((b.getAttribute && b.getAttribute('aria-label') || '') + ' ' + (b.innerText || '')));
          if (copyBtn) { copyBtn.click(); await new Promise(r => setTimeout(r, 800));
            try { return 'CLIP:' + (await navigator.clipboard.readText()); } catch (e) { return 'CLIPERR:' + e.message; }
          }
          row = row.parentElement;
        }
      }
      return 'NOROW';
    })()`, 25000);
    log('copy: ' + String(copied).slice(0, 80));
    const m = /CLIP:(GOCSPX-[A-Za-z0-9_-]{20,})/.exec(String(copied));
    if (m) { await saveSecret(m[1], 'clipboard'); return; }

    // 5) финальный дамп для диагностики
    log('⚠ секрет не пойман — дамп:');
    const dump = await M.evalRetry(ref, cdpPort, `document.body.innerText.slice(0, 2000)`, 15000);
    const deepDump = await M.evalRetry(ref, cdpPort, `${DEEP_TEXT}.slice(0, 2500)`, 20000);
    console.log('\n===== INNER =====\n' + dump + '\n===== DEEP =====\n' + deepDump + '\n===== /DUMP =====\n');
  } finally {
    try { child.kill('SIGTERM'); } catch (_) {}
    setTimeout(() => { try { child.kill('SIGKILL'); } catch (_) {} }, 3000);
  }

  async function saveSecret(secret, how) {
    log('🎉 СЕКРЕТ ПОЙМАН (' + how + '): GOCSPX-…' + secret.slice(-4));
    const secretJson = {
      installed: {
        client_id: CLIENT_ID, client_secret: secret,
        auth_uri: 'https://accounts.google.com/o/oauth2/v2/auth',
        token_uri: 'https://oauth2.googleapis.com/token',
        auth_provider_x509_cert_url: 'https://www.googleapis.com/oauth2/v1/certs',
        redirect_uris: ['http://localhost', 'http://127.0.0.1'],
      },
    };
    fs.writeFileSync(SECRET_FILE, JSON.stringify(secretJson, null, 2), { mode: 0o600 });
    fs.chmodSync(SECRET_FILE, 0o600);
    log('✓ client_secret.json записан (0600): ' + SECRET_FILE);
  }
})().catch((e) => { log('ОШИБКА: ' + e.message); process.exit(1); });
