#!/usr/bin/env node
'use strict';
/* gcp-verify-state.js — проверка фактического состояния GCP-настройки через API:
 *   1) access_token по refresh_token (gcp-tokens.json)
 *   2) проект verification-506705
 *   3) brand (consent screen): applicationTitle должно быть «POLER Engine»
 *   4) статусы Drive/Gmail API
 *   5) OAuth-клиенты проекта (credential view, без секретов) — через CRM? нет,
 *      используем serviceusage + brand. Токены НЕ печатаем. */
const fs = require('fs');
const path = require('path');
const os = require('os');
const https = require('https');
const M = require('/home/z/my-project/scripts/gcp-cdp-machinery.js');

const CFG = path.join(os.homedir(), '.config', 'poler-engine');
const PROJECT = 'verification-506705';
const TOKENS = JSON.parse(fs.readFileSync(path.join(CFG, 'gcp-tokens.json'), 'utf8'));
// refresh_token привязан к клиенту, которым выдан (gcloud SDK из gcp-oauth.json)
const OAUTH = JSON.parse(fs.readFileSync(path.join(CFG, 'gcp-oauth.json'), 'utf8'));

function req(method, url, token, form) {
  return new Promise((resolve, reject) => {
    const u = new URL(url);
    const headers = {};
    let data = null;
    if (form) { data = form; headers['Content-Type'] = 'application/x-www-form-urlencoded'; headers['Content-Length'] = Buffer.byteLength(data); }
    if (token) headers['Authorization'] = 'Bearer ' + token;
    const r = https.request({ hostname: u.hostname, path: u.pathname + u.search, method, headers, timeout: 20000 }, (res) => {
      let b = ''; res.on('data', (c) => (b += c));
      res.on('end', () => { let j = null; try { j = JSON.parse(b); } catch (_) {} resolve({ status: res.statusCode, json: j, text: b }); });
    });
    r.on('error', reject); r.on('timeout', () => { r.destroy(); reject(new Error('timeout')); });
    r.end(data || undefined);
  });
}

(async () => {
  // 1) свежий access_token
  const rf = await req('POST', 'https://oauth2.googleapis.com/token', null,
    new URLSearchParams({ client_id: OAUTH.client_id, client_secret: OAUTH.client_secret, refresh_token: TOKENS.refresh_token, grant_type: 'refresh_token' }).toString());
  if (rf.status !== 200) { console.log('refresh: HTTP ' + rf.status + ' — ' + rf.text.slice(0, 200)); process.exit(1); }
  const tok = rf.json.access_token;
  console.log('✓ access_token обновлён (' + tok.length + ' симв.), scope: ' + rf.json.scope);
  // сохраним обратно
  TOKENS.access_token = tok; TOKENS.ts = new Date().toISOString();
  fs.writeFileSync(path.join(CFG, 'gcp-tokens.json'), JSON.stringify(TOKENS, null, 2), { mode: 0o600 });
  fs.chmodSync(path.join(CFG, 'gcp-tokens.json'), 0o600);

  // 2) проект
  const pj = await req('GET', 'https://cloudresourcemanager.googleapis.com/v1/projects/' + PROJECT, tok);
  console.log('проект: HTTP ' + pj.status + ' — ' + (pj.json && pj.json.name ? pj.json.name + ' (' + pj.json.lifecycleState + ')' : pj.text.slice(0, 100)));

  // 3) brand (consent screen)
  const br = await req('GET', 'https://iap.googleapis.com/v1/projects/' + PROJECT + '/brands', tok);
  if (br.status === 200 && br.json.brands) {
    for (const b of br.json.brands) {
      console.log('brand: ' + (b.applicationTitle || '(без имени)') + ' | support: ' + (b.supportEmail || '—') + ' | org: ' + (b.orgDisplayName || '—'));
    }
  } else {
    console.log('brands: HTTP ' + br.status + ' — ' + br.text.slice(0, 150));
  }

  // 4) API-статусы
  for (const api of ['drive.googleapis.com', 'gmail.googleapis.com']) {
    const s = await req('GET', 'https://serviceusage.googleapis.com/v1/projects/' + PROJECT + '/services/' + api, tok);
    console.log(api + ': ' + (s.json && s.json.state ? s.json.state : 'HTTP ' + s.status));
  }
})().catch((e) => { console.error('ОШИБКА: ' + e.message); process.exit(1); });
