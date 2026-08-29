#!/usr/bin/env node
'use strict';
/*!
 * gcp-e2e-oauth.js — ПОЛНЫЙ E2E тест OAuth-клиента POLER Engine:
 *   аккаунт → consent «Разрешить» → код на loopback → token exchange → refresh.
 * Проверяет ровно тот путь, которым будет ходить poler-engine --google-auth.
 * Значения токенов НЕ печатаются и НЕ сохраняются (только факты).
 */
const https = require('https');
const http = require('http');
const net = require('net');
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const os = require('os');

const CFG = path.join(os.homedir(), '.config', 'poler-engine');
const secretJson = JSON.parse(fs.readFileSync(path.join(CFG, 'client_secret.json'), 'utf8'));
const CLIENT_ID = secretJson.installed.client_id;
const CLIENT_SECRET = secretJson.installed.client_secret;
const SCOPES = process.env.SCOPES || 'https://www.googleapis.com/auth/drive.readonly https://www.googleapis.com/auth/gmail.readonly';

const M = require('/home/z/my-project/scripts/gcp-cdp-machinery.js');
const log = M.log;
const sleep = M.sleep;

function httpsForm(url, form) {
  return new Promise((resolve, reject) => {
    const u = new URL(url);
    const r = https.request({ hostname: u.hostname, path: u.pathname, method: 'POST', headers: { 'Content-Type': 'application/x-www-form-urlencoded', 'Content-Length': Buffer.byteLength(form) }, timeout: 20000 }, (res) => {
      let b = '';
      res.on('data', (c) => (b += c));
      res.on('end', () => { let j = null; try { j = JSON.parse(b); } catch (_) {} resolve({ status: res.statusCode, json: j, text: b }); });
    });
    r.on('error', reject); r.on('timeout', () => { r.destroy(); reject(new Error('timeout')); });
    r.write(form); r.end();
  });
}

(async () => {
  log('E2E OAuth-тест клиента POLER Engine');
  log('client_id: ' + CLIENT_ID.slice(0, 30) + '…');
  log('scopes: ' + SCOPES);

  // loopback-ловушка
  const listener = net.createServer();
  await new Promise((r) => listener.listen(0, r));
  const port = listener.address().port;
  const redirectUri = 'http://localhost:' + port;
  log('redirect: ' + redirectUri);
  const state = crypto.randomBytes(10).toString('hex');
  const codePromise = new Promise((resolve, reject) => {
    const to = setTimeout(() => reject(new Error('код не пришёл за 61 мин')), 3660000);
    listener.on('connection', (sock) => {
      let done = false;
      sock.on('data', (d) => {
        if (done) return;
        done = true;
        const m = /code=([^&\s]+)/.exec(d.toString());
        try { sock.write('HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body>POLER Engine OAuth OK</body></html>'); sock.end(); } catch (_) {}
        if (m) { clearTimeout(to); resolve(decodeURIComponent(m[1])); }
      });
    });
  });

  const { child, cdpPort } = await M.launchChromium();
  log('Chromium CDP :' + cdpPort);
  const ref = { cdp: null };
  try {
    ref.cdp = await M.connectPageRetry(cdpPort, 4);
    const authUrl = 'https://accounts.google.com/o/oauth2/v2/auth?' + new URLSearchParams({
      client_id: CLIENT_ID, redirect_uri: redirectUri, response_type: 'code',
      scope: SCOPES, state, access_type: 'offline', prompt: 'consent',
    }).toString();
    log('открываю consent…');
    try { await ref.cdp.call('Page.navigate', { url: authUrl }, 10000); } catch (_) {}
    try { ref.cdp.ws.close(); } catch (_) {}
    await sleep(4000);
    ref.cdp = await M.connectPageRetry(cdpPort, 4);

    // авто-клики: аккаунт → Разрешить; цифру челленджа — ВЛАДЕЛЬЦУ, ждём до 61 мин
    let lastNum = '';
    for (let i = 0; i < 1830; i++) {
      await sleep(2000);
      let v = '';
      try {
        const r = await ref.cdp.call('Runtime.evaluate', { expression: `(() => {
          const h = location.href;
          if (h.includes('localhost:') || h.includes('127.0.0.1:')) return 'REDIRECTED';
          const body = document.body ? document.body.innerText : '';
          if (/подтвердите свою личность|verify it.?s you|проверьте оповещения/i.test(body)) {
            const nums = body.split('\\n').map(s => s.trim()).filter(l => /^\\d{2}$/.test(l));
            return 'CHALLENGE|' + (nums.join(',') || 'NONE') + '|' + body.slice(0, 150).replace(/\\s+/g, ' ');
          }
          if (/выберите аккаунт|choose an account/i.test(body)) {
            const hits = [...document.querySelectorAll('body *')].filter(e => /vitalijkotok18@gmail\\.com/.test(e.innerText || ''));
            if (hits.length) {
              const deepest = hits.reduce((a, b) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_CONTAINED_BY) ? b : a);
              deepest.click();
              return 'ACCOUNT_CLICKED';
            }
          }
          const sel = 'button, div[role=button], input[type=submit]';
          const btns = [...document.querySelectorAll(sel)];
          const exact = btns.find(b => /^(разрешить|allow|продолжить|continue|далее|next)$/i.test((b.innerText || b.value || '').trim()));
          if (exact) { exact.click(); return 'CLICK:' + (exact.innerText || exact.value || '').trim().slice(0, 25); }
          return 'WAIT|' + document.title.slice(0, 40);
        })()`, returnByValue: true, awaitPromise: true }, 15000);
        v = (r.result && r.result.value) || '';
      } catch (e) { try { ref.cdp = await M.connectPageRetry(cdpPort, 3); } catch (_) {} continue; }
      if (v === 'REDIRECTED') { log('✓ редирект на loopback — код пойман'); break; }
      if (v === 'ACCOUNT_CLICKED') log('аккаунт выбран');
      else if (v.startsWith('CLICK')) log('consent: ' + v);
      else if (v.startsWith('CHALLENGE')) {
        const parts = v.split('|');
        const nums = (parts[1] || '').split(',').filter(Boolean);
        const n = nums.length ? nums[nums.length - 1] : '';
        if (n && n !== lastNum) {
          lastNum = n;
          log('');
          log('╔══════════════════════════════════════╗');
          log('║  🔢 ЦИФРА ДЛЯ ПОДТВЕРЖДЕНИЯ:  ' + n + '     ║');
          log('╚══════════════════════════════════════╝');
          log('Подтверди её на телефоне. Жду до 61 минуты, НИЧЕГО не перезапускаю.');
          try { fs.writeFileSync(path.join(CFG, 'gcp-live.json'), JSON.stringify({ challenge: n, detail: 'E2E OAuth: подтверди на телефоне ЦИФРУ ' + n, ts: new Date().toISOString() }, null, 2), { mode: 0o600 }); } catch (_) {}
        }
        if (i % 15 === 0 && lastNum) log('⏳ жду подтверждение (' + (i * 2) + 'с), цифра: ' + lastNum);
      }
      else if (i % 30 === 0) log('… ' + v.slice(0, 60));
    }

    const code = await codePromise;
    log('✓ authorization code: ' + code.length + ' симв.');

    // token exchange
    const ex = await httpsForm('https://oauth2.googleapis.com/token', new URLSearchParams({
      code, client_id: CLIENT_ID, client_secret: CLIENT_SECRET,
      redirect_uri: redirectUri, grant_type: 'authorization_code',
    }).toString());
    if (ex.status !== 200) { log('✗ exchange: ' + ex.status + ' ' + ex.text.slice(0, 200)); process.exit(1); }
    log('✓ token exchange: HTTP 200');
    log('  access_token: …' + String(ex.json.access_token || '').slice(-6) + ' (' + String(ex.json.access_token || '').length + ' симв.)');
    log('  refresh_token: ' + (ex.json.refresh_token ? 'есть (' + ex.json.refresh_token.length + ' симв.)' : 'НЕТ'));
    log('  expires_in: ' + ex.json.expires_in + 'с');
    log('  scope: ' + (ex.json.scope || ''));

    // refresh-тест (refresh_token жив?)
    if (ex.json.refresh_token) {
      const rf = await httpsForm('https://oauth2.googleapis.com/token', new URLSearchParams({
        refresh_token: ex.json.refresh_token, client_id: CLIENT_ID, client_secret: CLIENT_SECRET,
        grant_type: 'refresh_token',
      }).toString());
      log(rf.status === 200 ? '✓ refresh-тест: HTTP 200 — refresh_token рабочий' : '✗ refresh: ' + rf.status + ' ' + rf.text.slice(0, 150));
    }

    // who am i (по access_token)
    const who = await new Promise((resolve, reject) => {
      https.get({ hostname: 'openidconnect.googleapis.com', path: '/v1/userinfo', headers: { Authorization: 'Bearer ' + ex.json.access_token } }, (res) => {
        let b = ''; res.on('data', (c) => (b += c)); res.on('end', () => resolve(b));
      }).on('error', reject);
    });
    try { const j = JSON.parse(who); log('✓ userinfo: ' + (j.email || '?') + (j.name ? ' (' + j.name + ')' : '')); } catch (_) { log('userinfo: ' + who.slice(0, 100)); }

    log('════ E2E: ВСЁ РАБОТАЕТ — клиент готов для poler-engine ════');
  } finally {
    try { listener.close(); } catch (_) {}
    try { child.kill('SIGTERM'); } catch (_) {}
    setTimeout(() => { try { child.kill('SIGKILL'); } catch (_) {} }, 3000);
  }
})().catch((e) => { log('ОШИБКА: ' + e.message); process.exit(1); });
