#!/usr/bin/env node
'use strict';
/* gcp-e2e-retry.js — реанимация E2E OAuth после Error 500:
 * навигация живой страницы обратно на consent-URL того же клиента и loopback-порта.
 * Дальше цикл gcp-e2e-oauth.js (живой) сам кликает аккаунт/Разрешить и ловит код. */
const fs = require('fs');
const path = require('path');
const os = require('os');
const crypto = require('crypto');
const M = require('/home/z/my-project/scripts/gcp-cdp-machinery.js');

const CFG = path.join(os.homedir(), '.config', 'poler-engine');
const secretJson = JSON.parse(fs.readFileSync(path.join(CFG, 'client_secret.json'), 'utf8'));
const CLIENT_ID = secretJson.installed.client_id;
const SCOPES = 'https://www.googleapis.com/auth/drive.readonly https://www.googleapis.com/auth/gmail.readonly';
const PORT = Number(process.argv[3] || 35577);
const CDP = Number(process.argv[2] || 59707);

(async () => {
  const authUrl = 'https://accounts.google.com/o/oauth2/v2/auth?' + new URLSearchParams({
    client_id: CLIENT_ID,
    redirect_uri: 'http://localhost:' + PORT,
    response_type: 'code',
    scope: SCOPES,
    state: crypto.randomBytes(10).toString('hex'),
    access_type: 'offline',
    prompt: 'consent',
  }).toString();
  M.log('реанимация: CDP :' + CDP + ', loopback :' + PORT);
  M.log('URL: ' + authUrl.slice(0, 90) + '…');
  const cdp = await M.connectPageRetry(CDP, 4);
  await cdp.call('Page.navigate', { url: authUrl }, 15000);
  M.log('✓ навигация отправлена — дальше цикл e2e сам кликает и ловит код');
  try { cdp.ws.close(); } catch (_) {}
  process.exit(0);
})().catch((e) => { console.error('ОШИБКА: ' + e.message); process.exit(1); });
