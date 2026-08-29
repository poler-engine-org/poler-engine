#!/usr/bin/env node
'use strict';
/* gcp-e2e-probe.js — пробой текущего состояния живого браузера e2e (CDP 59707):
 * что на странице сейчас: URL, title, первые строки body. Только чтение. */
const M = require('/home/z/my-project/scripts/gcp-cdp-machinery.js');

(async () => {
  const cdpPort = Number(process.argv[2] || 59707);
  const cdp = await M.connectPageRetry(cdpPort, 3);
  const r = await M.evalPage(cdp, `(() => ({
    href: location.href,
    title: document.title,
    body: (document.body ? document.body.innerText : '').slice(0, 400)
  }))()`, 15000);
  console.log(JSON.stringify(r, null, 2));
  try { cdp.ws.close(); } catch (_) {}
  process.exit(0);
})().catch((e) => { console.error('ОШИБКА: ' + e.message); process.exit(1); });
