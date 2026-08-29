#!/usr/bin/env node
'use strict';
/* poler-auth-fakecode.js — детерминированный репро висячки:
 * движок --google-auth в фоне; сами стучимся на его loopback с фейковым кодом
 * (state берём из URL). Обмен упадёт 400 — но весь путь (loopback, spawn
 * sensible-browser, GoogleHttp, ошибка, выход) будет пройден. Ждём exit. */
const { spawn } = require('child_process');
const http = require('http');
const fs = require('fs');
const M = require('/home/z/my-project/scripts/gcp-cdp-machinery.js');

const ENGINE = '/home/z/my-project/poler-engine-gh/target/debug/poler-engine';
const CHROME = '/home/z/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome';
const sleep = M.sleep;

(async () => {
  // браузер НА 9223 не нужен: exchange пойдёт через ensure_google_browser →
  // движок сам поднимет headless на 9223 (найдёт POLER_CHROME_BIN)

  const eng = spawn(ENGINE, ['--google-auth'], {
    env: { ...process.env, POLER_CHROME_BIN: CHROME },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let out = '';
  eng.stdout.on('data', (d) => (out += d));
  eng.stderr.on('data', (d) => (out += d));
  console.log('движок pid ' + eng.pid);

  let url = '';
  for (let i = 0; i < 50; i++) {
    const m = /https:\/\/accounts\.google\.com\/o\/oauth2\/v2\/auth\?[^\s]+/.exec(out);
    if (m) { url = m[0]; break; }
    await sleep(400);
  }
  if (!url) { console.log('URL не появился:\n' + out); process.exit(1); }
  const port = Number(new URL(url).searchParams.get('redirect_uri').match(/:(\d+)$/)[1]);
  const state = new URL(url).searchParams.get('state');
  console.log('loopback порт: ' + port + ', state: ' + state.slice(0, 8) + '…');

  // стучимся с фейковым кодом (loopback слушает 127.0.0.1)
  await new Promise((resolve) => {
    http.get('http://127.0.0.1:' + port + '/?code=4%2F0FAKECODE_for_hang_repro&state=' + state, (res) => {
      let b = ''; res.on('data', (c) => (b += c)); res.on('end', () => { console.log('loopback ответил: ' + b.slice(0, 60)); resolve(); });
    }).on('error', (e) => { console.log('loopback error: ' + e.message); resolve(); });
  });

  // ждём exit до 60с
  const t0 = Date.now();
  const rc = await new Promise((resolve) => {
    const to = setTimeout(() => resolve('TIMEOUT-60s'), 60000);
    eng.on('exit', (code) => { clearTimeout(to); resolve('exit=' + code); });
    // если завис — инспекция на 30-й секунде
    setTimeout(() => {
      if (fs.existsSync('/proc/' + eng.pid)) {
        try {
          const status = fs.readFileSync('/proc/' + eng.pid + '/status', 'utf8');
          console.log('30с: ещё жив, state=' + ((status.match(/State:\s+(\S+)/) || [])[1]) + ' threads=' + ((status.match(/Threads:\s+(\d+)/) || [])[1]));
          const tasks = fs.readdirSync('/proc/' + eng.pid + '/task');
          for (const t of tasks) {
            const w = fs.readFileSync('/proc/' + eng.pid + '/task/' + t + '/wchan', 'utf8').trim();
            console.log('  tid ' + t + ' wchan=' + w);
          }
          const children = fs.readFileSync('/proc/' + eng.pid + '/task/' + eng.pid + '/children', 'utf8').trim();
          console.log('дети: ' + (children || 'нет'));
          if (children) for (const c of children.split(/\s+/)) {
            try { console.log('  ребёнок ' + c + ': ' + fs.readlinkSync('/proc/' + c + '/exe')); } catch (_) {}
          }
        } catch (e) { console.log('инспекция: ' + e.message); }
      }
    }, 30000);
  });
  console.log('результат: ' + rc + ' (' + ((Date.now() - t0) / 1000).toFixed(1) + 'с)');
  console.log('— вывод движка (хвост) —');
  console.log(out.split('\n').filter(Boolean).slice(-6).join('\n'));
  try { eng.kill('SIGKILL'); } catch (_) {}
  setTimeout(() => process.exit(0), 1000);
})().catch((e) => { console.error('ОШИБКА: ' + e.message); process.exit(1); });
