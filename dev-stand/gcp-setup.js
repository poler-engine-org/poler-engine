#!/usr/bin/env node
'use strict';
/*!
 * gcp-setup.js — настройка GCP-проекта POLER Engine (verification-506705)
 * автономно, через живую сессию Google в изолированном профиле движка.
 *
 * Фазы (идемпотентны, токен переиспользуется из gcp-tokens.json):
 *   token   — OAuth code-flow (loopback + клик «Разрешить» в headless Chromium)
 *   probe   — доступ к проекту + включение Drive/Gmail API + список brands
 *   brand   — consent screen (создать External brand «POLER Engine»)
 *   client  — OAuth-клиент Desktop (clientauthconfig) → client_secret.json
 *   user    — добавить email владельца в test users
 *   all     — всё по порядку (default)
 *
 * Секреты — только в ~/.config/poler-engine/ (0600). Логи без значений.
 */

const { spawn } = require('child_process');
const https = require('https');
const http = require('http');
const net = require('net');
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const os = require('os');

const CHROME = '/home/z/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome';
const PROFILE = '/home/z/.cache/poler-engine/google-profile';
const CFG = path.join(os.homedir(), '.config', 'poler-engine');
const TOKENS_FILE = path.join(CFG, 'gcp-tokens.json');
const SECRET_FILE = path.join(CFG, 'client_secret.json');
const LIVE_FILE = path.join(CFG, 'gcp-live.json');
const PROJECT = process.argv[2] || 'verification-506705';
const PHASE = process.argv[3] || 'all';

// Креды gcloud SDK (open-source google-cloud-sdk) для получения
// cloud-platform токена владельца через его же сессию.
// В репозиторий НЕ попадают: берутся из env (GC_ID/GC_SEC) или из
// ~/.config/poler-engine/gcp-oauth.json (0600), формат {"client_id","client_secret"}.
const GC_OAUTH_FILE = path.join(CFG, 'gcp-oauth.json');
function loadGcCreds() {
  const fromEnv = { id: process.env.GC_ID, sec: process.env.GC_SEC };
  if (fromEnv.id && fromEnv.sec) return fromEnv;
  try {
    const j = JSON.parse(fs.readFileSync(GC_OAUTH_FILE, 'utf8'));
    if (j.client_id && j.client_secret) return { id: j.client_id, sec: j.client_secret };
  } catch (_) {}
  console.error('[gcp-setup] НЕТ КРЕДОВ: задай env GC_ID/GC_SEC или создай ' + GC_OAUTH_FILE +
    ' {"client_id":"...","client_secret":"..."} (chmod 600)');
  process.exit(2);
}
const GC_CREDS = loadGcCreds();
const GC_ID = GC_CREDS.id;
const GC_SEC = GC_CREDS.sec;

const log = (m) => process.stdout.write(`[${new Date().toISOString()}] ${m}\n`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const b64json = (s) => { try { return JSON.parse(Buffer.from(s, 'base64').toString('utf8')); } catch (_) { return null; } };

/* ═══════════════ MiniWs + CdpClient (проверенные, из auth-preview.js) ═══════════════ */
class MiniWs {
  constructor(sock) {
    this.sock = sock; this.buf = Buffer.alloc(0);
    this.fragments = []; this.fragOpcode = 0;
    this.onMessage = null; this.onClose = null; this._closeEmitted = false;
  }
  static connect(port, wsPath, timeoutMs = 10000) {
    return new Promise((resolve, reject) => {
      const key = crypto.randomBytes(16).toString('base64');
      const sock = net.connect({ host: '127.0.0.1', port });
      let ws = null, handshaked = false, buf = Buffer.alloc(0);
      const timer = setTimeout(() => { sock.destroy(); reject(new Error('WS connect: таймаут')); }, timeoutMs);
      sock.once('error', (e) => { clearTimeout(timer); reject(e); });
      sock.once('connect', () => {
        sock.write(`GET ${wsPath} HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`);
      });
      sock.on('data', (d) => {
        if (!handshaked) {
          buf = Buffer.concat([buf, d]);
          const i = buf.indexOf('\r\n\r\n');
          if (i === -1) return;
          const head = buf.slice(0, i).toString('latin1');
          if (!/^HTTP\/1\.1 101/.test(head)) { clearTimeout(timer); sock.destroy(); reject(new Error('WS handshake: ' + head.split('\r\n')[0])); return; }
          handshaked = true; clearTimeout(timer);
          ws = new MiniWs(sock);
          const rest = buf.slice(i + 4);
          resolve(ws);
          if (rest.length) ws._feed(rest);
          return;
        }
        if (ws) ws._feed(d);
      });
      sock.once('close', () => { if (ws) ws._shutdown(); else if (!handshaked) { clearTimeout(timer); reject(new Error('WS закрыт до handshake')); } });
    });
  }
  get isOpen() { return !this._closeEmitted && this.sock && !this.sock.destroyed; }
  send(text) { this._sendFrame(0x1, Buffer.from(String(text), 'utf8')); }
  close() { try { if (this.isOpen) this._sendFrame(0x8, Buffer.alloc(0)); this.sock.end(); } catch (_) {} this._shutdown(); }
  _feed(d) {
    this.buf = this.buf.length ? Buffer.concat([this.buf, d]) : d;
    for (;;) { const f = this._parseFrame(); if (!f) break; this._handleFrame(f); if (this._closeEmitted) break; }
  }
  _parseFrame() {
    const b = this.buf; if (b.length < 2) return null;
    const fin = (b[0] & 0x80) !== 0, opcode = b[0] & 0x0f, masked = (b[1] & 0x80) !== 0;
    let len = b[1] & 0x7f, off = 2;
    if (len === 126) { if (b.length < off + 2) return null; len = b.readUInt16BE(off); off += 2; }
    else if (len === 127) { if (b.length < off + 8) return null; len = b.readUInt32BE(off) * 4294967296 + b.readUInt32BE(off + 4); off += 8; }
    let maskKey = null;
    if (masked) { if (b.length < off + 4) return null; maskKey = b.slice(off, off + 4); off += 4; }
    if (b.length < off + len) return null;
    let payload = b.slice(off, off + len);
    this.buf = b.slice(off + len);
    if (maskKey) { const out = Buffer.allocUnsafe(payload.length); for (let i = 0; i < payload.length; i++) out[i] = payload[i] ^ maskKey[i & 3]; payload = out; }
    return { fin, opcode, payload };
  }
  _handleFrame(f) {
    switch (f.opcode) {
      case 0x0: if (this.fragOpcode) { this.fragments.push(f.payload); if (f.fin) { const full = Buffer.concat(this.fragments); const op = this.fragOpcode; this.fragments = []; this.fragOpcode = 0; this._deliver(op, full); } } break;
      case 0x1: case 0x2: if (f.fin) this._deliver(f.opcode, f.payload); else { this.fragments = [f.payload]; this.fragOpcode = f.opcode; } break;
      case 0x8: try { if (this.isOpen) this._sendFrame(0x8, f.payload); } catch (_) {} this._shutdown(); break;
      case 0x9: this._sendFrame(0xA, f.payload); break;
      default: break;
    }
  }
  _deliver(opcode, payload) { if (opcode === 0x1 && this.onMessage) { try { this.onMessage(payload.toString('utf8')); } catch (_) {} } }
  _sendFrame(opcode, payload) {
    if (!this.sock || this.sock.destroyed) return;
    const mask = crypto.randomBytes(4), len = payload.length;
    let header;
    if (len < 126) { header = Buffer.alloc(2); header[1] = 0x80 | len; }
    else if (len < 65536) { header = Buffer.alloc(4); header[1] = 0x80 | 126; header.writeUInt16BE(len, 2); }
    else { header = Buffer.alloc(10); header[1] = 0x80 | 127; header.writeUInt32BE(Math.floor(len / 4294967296), 2); header.writeUInt32BE(len >>> 0, 6); }
    header[0] = 0x80 | opcode;
    const masked = Buffer.allocUnsafe(payload.length);
    for (let i = 0; i < payload.length; i++) masked[i] = payload[i] ^ mask[i & 3];
    this.sock.write(Buffer.concat([header, mask, masked]));
  }
  _shutdown() { if (this._closeEmitted) return; this._closeEmitted = true; if (this.onClose) { try { this.onClose(); } catch (_) {} } }
}

class CdpClient {
  constructor(ws, onEvent) {
    this.ws = ws; this.nextId = 1; this.pending = new Map(); this.onEvent = onEvent || null;
    ws.onMessage = (text) => {
      let m; try { m = JSON.parse(text); } catch (_) { return; }
      if (m && m.id && this.pending.has(m.id)) {
        const { resolve, reject } = this.pending.get(m.id); this.pending.delete(m.id);
        m.error ? reject(new Error(m.error.message || 'CDP error')) : resolve(m.result);
      } else if (m && m.method && this.onEvent) this.onEvent(m);
    };
    ws.onClose = () => { for (const { reject } of this.pending.values()) reject(new Error('CDP: соединение закрыто')); this.pending.clear(); };
  }
  call(method, params = {}, timeoutMs = 20000) {
    return new Promise((resolve, reject) => {
      if (!this.ws.isOpen) { reject(new Error('CDP: WS не открыт')); return; }
      const id = ++this.nextId;
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error('CDP таймаут: ' + method)); }, timeoutMs);
      this.pending.set(id, { resolve: (v) => { clearTimeout(timer); resolve(v); }, reject: (e) => { clearTimeout(timer); reject(e); } });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
}

function wsPathOf(wsUrl) { const m = /ws:\/\/[^/]+(\/.*)$/.exec(String(wsUrl || '')); return m ? m[1] : '/devtools/browser'; }

function getJsonPort(port, p) {
  return new Promise((resolve, reject) => {
    http.get({ host: '127.0.0.1', port, path: p, timeout: 3000 }, (res) => {
      let b = ''; res.on('data', (c) => b += c);
      res.on('end', () => { try { resolve(JSON.parse(b)); } catch (e) { reject(new Error('bad json: ' + b.slice(0, 100))); } });
    }).on('error', reject).on('timeout', function () { this.destroy(new Error('timeout')); });
  });
}

/* ═══════════════ HTTPS-хелперы ═══════════════ */
function httpsReq(method, url, { token, body, form } = {}) {
  return new Promise((resolve, reject) => {
    const u = new URL(url);
    let data = null;
    const headers = {};
    if (body !== undefined) { data = JSON.stringify(body); headers['Content-Type'] = 'application/json'; }
    if (form) { data = form; headers['Content-Type'] = 'application/x-www-form-urlencoded'; }
    if (token) headers['Authorization'] = 'Bearer ' + token;
    if (data) headers['Content-Length'] = Buffer.byteLength(data);
    const req = https.request({ hostname: u.hostname, path: u.pathname + u.search, method, headers, timeout: 20000 }, (res) => {
      let b = ''; res.on('data', (c) => b += c);
      res.on('end', () => { let j = null; try { j = JSON.parse(b); } catch (_) {} resolve({ status: res.statusCode, json: j, text: b }); });
    });
    req.on('error', reject); req.on('timeout', () => { req.destroy(new Error('https timeout')); });
    req.end(data || undefined);
  });
}
const api = (method, url, token, body) => httpsReq(method, url, { token, body });

/* ═══════════════ Фаза: token ═══════════════ */
async function loadTokens() {
  try {
    const t = JSON.parse(fs.readFileSync(TOKENS_FILE, 'utf8'));
    if (t.access_token && t.email) return t;
  } catch (_) {}
  return null;
}

async function saveTokens(t) {
  fs.mkdirSync(CFG, { recursive: true });
  fs.writeFileSync(TOKENS_FILE, JSON.stringify(t, null, 2), { mode: 0o600 });
  fs.chmodSync(TOKENS_FILE, 0o600);
}

async function tokenValid(t) {
  try {
    const r = await httpsReq('GET', 'https://cloudresourcemanager.googleapis.com/v1/projects/' + PROJECT, { token: t.access_token });
    return r.status === 200;
  } catch (_) { return false; }
}

async function refreshToken(t) {
  const form = new URLSearchParams({ client_id: GC_ID, client_secret: GC_SEC, refresh_token: t.refresh_token, grant_type: 'refresh_token' }).toString();
  const r = await httpsReq('POST', 'https://oauth2.googleapis.com/token', { form });
  if (r.status !== 200) throw new Error('refresh: ' + r.status + ' ' + (r.text || '').slice(0, 120));
  t.access_token = r.json.access_token;
  await saveTokens(t);
  log('access_token обновлён по refresh_token (' + t.access_token.length + ' симв.)');
  return t;
}

function writeLive(fields) {
  try {
    let cur = {};
    try { cur = JSON.parse(fs.readFileSync(LIVE_FILE, 'utf8')) || {}; } catch (_) {}
    fs.writeFileSync(LIVE_FILE, JSON.stringify({ ...cur, ...fields, ts: new Date().toISOString() }, null, 2), { mode: 0o600 });
  } catch (_) {}
}
function clearLive() { try { fs.unlinkSync(LIVE_FILE); } catch (_) {} }

async function launchChromium() {
  const cdpPort = 45000 + Math.floor(Math.random() * 15000);
  // headless=new: стабильнее (headed на Xvfb падал на consent-редиректе);
  // экран всё равно виден в превью — релей делает CDP-скринкаст/скриншоты
  const child = spawn(CHROME, [
    `--user-data-dir=${PROFILE}`, `--remote-debugging-port=${cdpPort}`,
    '--remote-debugging-address=127.0.0.1', '--no-first-run', '--no-default-browser-check',
    '--no-sandbox', '--headless=new', '--disable-gpu', '--hide-crash-restore-bubble',
    '--window-size=1280,900', 'about:blank',
  ], { stdio: 'ignore' });
  for (let i = 0; i < 50; i++) {
    await sleep(400);
    try { await getJsonPort(cdpPort, '/json/version'); return { child, cdpPort }; } catch (_) {}
  }
  throw new Error('Chromium не поднял CDP за 20с');
}

async function evalPage(cdp, expression, timeoutMs) {
  const r = await cdp.call('Runtime.evaluate', { expression, returnByValue: true }, timeoutMs || 20000);
  return (r.result && r.result.value) || '';
}

async function harvestToken() {
  // ОДНА попытка. Никаких перезапусков и «фарминга» новых цифр:
  // владелец подтверждает на телефоне, когда найдёт время (до 61 минуты).
  return harvestTokenOnce();
}

async function harvestTokenOnce() {
  log('запуск Chromium (Xvfb :99) с изолированным профилем…');
  const { child, cdpPort } = await launchChromium();
  log('Chromium CDP :' + cdpPort);
  writeLive({ cdp_port: cdpPort, pid: child.pid, phase: 'token', detail: 'GCP: окно Google открывается…' });
  try {
    // подключение к страничному таргету с авто-переподключением: при
    // кросс-процессной навигации старый WS может «замирать» (eval висит)
    async function connectPage() {
      const list = await getJsonPort(cdpPort, '/json/list');
      const page = (list || []).find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
      if (!page) throw new Error('нет page-target');
      const ws = await MiniWs.connect(cdpPort, wsPathOf(page.webSocketDebuggerUrl));
      const c = new CdpClient(ws);
      await c.call('Page.enable');
      await c.call('Runtime.enable');
      return c;
    }
    let cdp = await connectPage();
    let reconnected = 0;
    async function evalRetry(expr) {
      for (let k = 0; k < 3; k++) {
        try { return await evalPage(cdp, expr, 8000); }
        catch (e) {
          try { cdp.ws.close(); } catch (_) {}
          try {
            cdp = await connectPage();
            if (++reconnected <= 3) log('CDP переподключён (навигация сменила контекст)');
          } catch (_) { await sleep(1500); }
        }
      }
      return 'ERR|eval не удался после переподключений';
    }

    // навигация через location.assign внутри страницы; после кросс-процессного
    // перехода старый target-WS замирает — закрываем и подключаемся заново
    async function navTo(url) {
      try { await cdp.call('Page.navigate', { url }, 8000); } catch (_) {}
      try { cdp.ws.close(); } catch (_) {}
    }
    async function reconn(times) {
      for (let k = 0; k < (times || 3); k++) {
        try { cdp = await connectPage(); return true; } catch (_) { await sleep(1500); }
      }
      return false;
    }

    // жива ли сессия?
    await navTo('https://accounts.google.com/');
    await sleep(6000);
    await reconn(5);
    const info = await evalRetry('document.title + "|||" + location.href');
    log('accounts.google.com: ' + info.slice(0, 100));
    if (/^ERR\|/.test(info)) throw new Error('CDP: страница не отвечает после навигации');
    if (/signin|servicelogin/i.test(info)) throw new Error('сессия профиля не жива — нужен повторный вход через превью');

    // loopback-ловушка
    const listener = net.createServer();
    await new Promise((r) => listener.listen(0, r)); // dual-stack: :: и 127.0.0.1
    const port = listener.address().port;
    const redirectUri = 'http://localhost:' + port;
    const state = crypto.randomBytes(10).toString('hex');
    const codePromise = new Promise((resolve, reject) => {
      const to = setTimeout(() => reject(new Error('consent: таймаут 3660с')), 3660000);
      listener.on('connection', (sock) => {
        let done = false;
        sock.on('data', (d) => {
          if (done) return;
          done = true;
          const m = /code=([^&\s]+)/.exec(d.toString());
          try { sock.write('HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body>OK</body></html>'); sock.end(); } catch (_) {}
          if (m) { clearTimeout(to); resolve(decodeURIComponent(m[1])); }
        });
      });
    });

    const authUrl = 'https://accounts.google.com/o/oauth2/v2/auth?' + new URLSearchParams({
      client_id: GC_ID, redirect_uri: redirectUri, response_type: 'code',
      scope: process.env.SCOPES || 'https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email openid',
      state, access_type: 'offline', prompt: 'consent',
    }).toString();
    log('открываю consent (redirect → ' + redirectUri + ')');
    await navTo(authUrl);

    let clicks = 0;
    let lastNum = '';
    let challengeSeen = false;
    log('⏳ если Google спросит подтверждение — ЦИФРА будет напечатана здесь и в превью. Ждём до 61 минуты, БЕЗ перезапусков');
    writeLive({ detail: 'GCP: подтверждение Google…' });
    for (let i = 0; i < 1830; i++) {
      await sleep(2000);
      let v = '';
      try {
        v = await evalRetry(`(() => {
          const h = location.href;
          if (h.includes('localhost:') || h.includes('127.0.0.1:')) return 'REDIRECTED';
          const body = document.body ? document.body.innerText : '';
          // 0) челлендж подтверждения личности: человек подтверждает на телефоне —
          //    вытаскиваем ЦИФРУ с экрана, чтобы владелец сверил её с телефоном
          if (/подтвердите свою личность|verify it.?s you|проверьте оповещения|проверьте телефон|check your phone|подтвердите, что это вы/i.test(body)) {
            const nums = body.split('\\n').map(s => s.trim()).filter(l => /^\\d{2}$/.test(l));
            return 'CHALLENGE|' + (nums.join(',') || 'NONE') + '|' + body.slice(0, 180).replace(/\\s+/g, ' ');
          }
          // 1) выбор аккаунта: кликаем самый глубокий элемент с email
          if (/выберите аккаунт|choose an account/i.test(body)) {
            const hits = [...document.querySelectorAll('body *')].filter(e => /vitalijkotok18@gmail\.com/.test(e.innerText||''));
            if (hits.length) {
              const deepest = hits.reduce((a,b) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_CONTAINED_BY) ? b : a);
              deepest.click();
              return 'ACCOUNT_CLICKED';
            }
          }
          const sel = 'button, div[role=button], input[type=submit]';
          const btns = [...document.querySelectorAll(sel)];
          const exact = btns.find(b => /^(разрешить|allow|продолжить|continue|далее|next)$/i.test((b.innerText||b.value||'').trim()));
          if (exact) { exact.click(); return 'CLICK:' + (exact.innerText||exact.value||'').trim().slice(0,25); }
          const loose = btns.find(b => /разрешить|allow|продолжить как/i.test((b.innerText||'') + (b.value||'')));
          if (loose) { loose.click(); return 'CLICK2:' + (loose.innerText||loose.value||'').trim().slice(0,35); }
          return 'WAIT|' + document.title.slice(0,40) + '|' + (document.body?document.body.innerText.slice(0,90).replace(/\\s+/g,' '):'');
        })()`);
      } catch (e) { v = 'ERR|' + e.message.slice(0, 40); }
      if (v === 'REDIRECTED') { log('редирект на loopback — код на подходе'); break; }
      if (v === 'ACCOUNT_CLICKED' && clicks < 8) { log('аккаунт выбран'); clicks++; }
      else if (v.startsWith('CLICK') && clicks < 8) { log('consent: ' + v); clicks++; }
      else if (v.startsWith('CHALLENGE')) {
        const parts = v.split('|');
        const nums = (parts[1] || '').split(',').filter(Boolean);
        const n = nums.length ? nums[nums.length - 1] : '';
        if (n && n !== lastNum) {
          lastNum = n;
          log('🔢 ЦИФРА НА ЭКРАНЕ — ПОДТВЕРДИ ЕЁ НА ТЕЛЕФОНЕ: ' + n);
          writeLive({ challenge: n, detail: 'GCP: подтверди на телефоне ЦИФРУ ' + n });
        }
        if (!challengeSeen) { challengeSeen = true; log('экран подтверждения: ' + (parts[2] || '').slice(0, 160)); }
        if (i % 15 === 0) log('⏳ ждём подтверждение на телефоне (прошло ' + (i*2) + 'с), цифра: ' + (lastNum || '—'));
      }
      else if (i % 6 === 0) log('… ' + v.slice(0, 110));
    }
    const code = await codePromise;
    log('authorization code пойман (' + code.length + ' симв.)');

    const form = new URLSearchParams({
      code, client_id: GC_ID, client_secret: GC_SEC,
      redirect_uri: redirectUri, grant_type: 'authorization_code',
    }).toString();
    const ex = await httpsReq('POST', 'https://oauth2.googleapis.com/token', { form });
    if (ex.status !== 200) throw new Error('token exchange: ' + ex.status + ' ' + (ex.text || '').slice(0, 160));
    const idc = b64json((ex.json.id_token || '').split('.')[1]) || {};
    const tokens = {
      access_token: ex.json.access_token,
      refresh_token: ex.json.refresh_token,
      email: idc.email || '', name: idc.name || '',
      client_id: GC_ID, client_secret: GC_SEC,
      scope: ex.json.scope, ts: new Date().toISOString(),
    };
    await saveTokens(tokens);
    log('✓ токены получены и сохранены (0600). Аккаунт: ' + tokens.email);
    return tokens;
  } finally {
    clearLive();
    try { child.kill('SIGTERM'); } catch (_) {}
    setTimeout(() => { try { child.kill('SIGKILL'); } catch (_) {} }, 3000);
  }
}

async function ensureToken() {
  let t = await loadTokens();
  if (t) {
    if (await tokenValid(t)) { log('токен из кэша валиден (' + t.email + ')'); return t; }
    if (t.refresh_token) { try { return await refreshToken(t); } catch (e) { log('refresh не вышел: ' + e.message + ' — собираю заново'); } }
  }
  return harvestToken();
}

/* ═══════════════ Фаза: probe ═══════════════ */
async function phaseProbe(tok) {
  const proj = await api('GET', `https://cloudresourcemanager.googleapis.com/v1/projects/${PROJECT}`, tok.access_token);
  log(`проект ${PROJECT}: HTTP ${proj.status} — ` + (proj.json && proj.json.name ? `${proj.json.name} (${proj.json.lifecycleState}, parent=${(proj.json.parent && proj.json.parent.id) || '—'})` : (proj.text || '').slice(0, 160)));
  if (proj.status !== 200) throw new Error('нет доступа к проекту');

  for (const svc of ['drive.googleapis.com', 'gmail.googleapis.com', 'iap.googleapis.com']) {
    const en = await api('POST', `https://serviceusage.googleapis.com/v1/projects/${PROJECT}/services/${svc}:enable`, tok.access_token, {});
    const done = en.json && (en.json.done === true || en.json.done === false);
    log(`enable ${svc}: HTTP ${en.status} ${done ? 'operation done=' + en.json.done : (en.json && en.json.error ? en.json.error.message : (en.text || '').slice(0, 100))}`);
  }

  // подождать включения
  await sleep(4000);
  for (const svc of ['drive.googleapis.com', 'gmail.googleapis.com']) {
    const st = await api('GET', `https://serviceusage.googleapis.com/v1/projects/${PROJECT}/services/${svc}`, tok.access_token);
    log(`статус ${svc}: ` + (st.json && st.json.state ? st.json.state : st.status));
  }

  const brands = await api('GET', `https://clientauthconfig.googleapis.com/v1/brands?project=${PROJECT}&pageSize=10`, tok.access_token);
  log('brands: HTTP ' + brands.status + ' → ' + (brands.text || '').slice(0, 300));
  return brands;
}

/* ═══════════════ Фаза: brand (consent screen) ═══════════════ */
async function phaseBrand(tok) {
  const brands = await api('GET', `https://clientauthconfig.googleapis.com/v1/brands?project=${PROJECT}&pageSize=10`, tok.access_token);
  let brand = (brands.json && brands.json.brands && brands.json.brands[0]) || null;
  if (!brand) {
    log('brand не найден — создаю External consent screen «POLER Engine»');
    const created = await api('POST', 'https://clientauthconfig.googleapis.com/v1/brands', tok.access_token, {
      applicationTitle: 'POLER Engine',
      supportEmail: tok.email,
      project: 'projects/' + PROJECT,
    });
    log('create brand: HTTP ' + created.status + ' → ' + (created.text || '').slice(0, 300));
    if (created.json && created.json.brandName) brand = created.json;
    else {
      const again = await api('GET', `https://clientauthconfig.googleapis.com/v1/brands?project=${PROJECT}&pageSize=10`, tok.access_token);
      brand = (again.json && again.json.brands && again.json.brands[0]) || null;
    }
  }
  if (brand) log('✓ brand: ' + JSON.stringify({ brandName: brand.brandName, applicationTitle: brand.applicationTitle, supportEmail: brand.supportEmail, orgDisplayName: brand.orgDisplayName }));
  else log('⚠ brand не получен — детали выше');
  return brand;
}

/* ═══════════════ Фаза: client (OAuth Desktop) ═══════════════ */
async function phaseClient(tok, brand) {
  if (!brand || !brand.brandName) { log('нет brand — клиент не создать'); return null; }
  const existing = await api('GET', `https://clientauthconfig.googleapis.com/v1/${brand.brandName}/clients?pageSize=20`, tok.access_token);
  log('clients: HTTP ' + existing.status + ' → ' + (existing.text || '').slice(0, 400));

  let client = null;
  for (const c of (existing.json && existing.json.clients) || []) {
    log('  есть клиент: ' + JSON.stringify({ displayName: c.displayName, applicationType: c.applicationType, clientType: c.clientType, id: (c.clientId || '').slice(0, 12) + '…' }));
    if (!client && (c.applicationType === 'OTHERS' || /desktop/i.test(c.displayName || ''))) client = c;
  }

  if (!client) {
    log('Desktop-клиента нет — создаю poler-engine-desktop…');
    const variants = [
      { displayName: 'poler-engine-desktop', applicationType: 'OTHERS', project: 'projects/' + PROJECT },
      { displayName: 'poler-engine-desktop', project: 'projects/' + PROJECT },
    ];
    for (const v of variants) {
      const created = await api('POST', `https://clientauthconfig.googleapis.com/v1/${brand.brandName}/clients`, tok.access_token, v);
      log('create client: HTTP ' + created.status + ' → ' + (created.text || '').slice(0, 400));
      if (created.status === 200 && created.json && created.json.clientId) { client = created.json; break; }
    }
  }

  if (!client) { log('⚠ клиент не создан через API — нужен fallback через консоль'); return null; }

  // клиентский секрет: у clientauthconfig он может прийти полем или требовать отдельного запроса
  let clientId = client.clientId || client.id;
  let clientSecret = client.clientSecret || client.secret || '';
  if (!clientSecret) {
    const detail = await api('GET', `https://clientauthconfig.googleapis.com/v1/${brand.brandName}/clients/${clientId}`, tok.access_token);
    clientSecret = (detail.json && (detail.json.clientSecret || detail.json.secret)) || '';
    log('client detail: HTTP ' + detail.status + (clientSecret ? ' — секрет получен' : ' — секрета нет: ' + (detail.text || '').slice(0, 150)));
  }

  if (clientId && clientSecret) {
    const secretJson = {
      installed: {
        client_id: clientId, client_secret: clientSecret,
        auth_uri: 'https://accounts.google.com/o/oauth2/v2/auth',
        token_uri: 'https://oauth2.googleapis.com/token',
        auth_provider_x509_cert_url: 'https://www.googleapis.com/oauth2/v1/certs',
        redirect_uris: ['http://localhost', 'http://127.0.0.1'],
      },
    };
    fs.writeFileSync(SECRET_FILE, JSON.stringify(secretJson, null, 2), { mode: 0o600 });
    fs.chmodSync(SECRET_FILE, 0o600);
    log('✓ client_secret.json записан (0600): client_id=' + clientId.slice(0, 18) + '… secret=' + clientSecret.length + ' симв.');
    return secretJson;
  }
  log('⚠ не удалось достать client_secret — clientId=' + String(clientId).slice(0, 18) + '…');
  return null;
}

/* ═══════════════ Фаза: user (test users) ═══════════════ */
async function phaseUser(tok, brand) {
  if (!brand || !brand.brandName) return;
  // brands.update: добавить email в testUsers (пробуем несколько форм)
  const tries = [
    ['PATCH', `https://clientauthconfig.googleapis.com/v1/${brand.brandName}?updateMask=testUsers`, { testUsers: [tok.email] }],
    ['POST', `https://clientauthconfig.googleapis.com/v1/${brand.brandName}:update`, { testUsers: [tok.email] }],
  ];
  for (const [m, u, b] of tries) {
    const r = await httpsReq(m, u, { token: tok.access_token, body: b });
    log(`test user: ${m} → HTTP ${r.status} ` + ((r.json && r.json.error && r.json.error.message) || '').slice(0, 140));
    if (r.status === 200) {
      const has = r.json && r.json.testUsers && r.json.testUsers.includes(tok.email);
      if (has) { log('✓ test user добавлен: ' + tok.email); return; }
    }
  }
  log('⚠ test user через API не добавился — можно добавить в консоли (Test users)');
}

/* ═══════════════ Фаза: debug (dump consent error) ═══════════════ */
async function phaseDebug() {
  log('запуск headless Chromium…');
  const { child, cdpPort } = await launchChromium();
  try {
    const list = await getJsonPort(cdpPort, '/json/list');
    const page = (list || []).find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
    const ws = await MiniWs.connect(cdpPort, wsPathOf(page.webSocketDebuggerUrl));
    const cdp = new CdpClient(ws);
    await cdp.call('Page.enable');
    await cdp.call('Runtime.enable');

    const redirectUri = 'http://localhost:1';
    const authUrl = 'https://accounts.google.com/o/oauth2/v2/auth?' + new URLSearchParams({
      client_id: GC_ID, redirect_uri: redirectUri, response_type: 'code',
      scope: process.env.SCOPES || 'https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email openid',
      access_type: 'offline', prompt: 'consent',
    }).toString();
    await cdp.call('Page.navigate', { url: authUrl });
    await sleep(9000);
    const info = await evalPage(cdp, `JSON.stringify({
      url: location.href,
      title: document.title,
      text: document.body ? document.body.innerText.slice(0, 1200) : '',
    })`);
    log('ERROR PAGE: ' + info);
    const shot = await cdp.call('Page.captureScreenshot', { format: 'png' });
    fs.writeFileSync('/home/z/my-project/logs/consent-error.png', Buffer.from(shot.data, 'base64'));
    log('скриншот → logs/consent-error.png');
  } finally {
    try { child.kill('SIGTERM'); } catch (_) {}
    setTimeout(() => { try { child.kill('SIGKILL'); } catch (_) {} }, 3000);
  }
}

/* ═══════════════ main ═══════════════ */
async function main() {
  log('════ GCP SETUP · проект ' + PROJECT + ' · фаза ' + PHASE + ' ════');
  if (PHASE === 'debug') { await phaseDebug(); return; }
  const tok = await ensureToken();
  log('аккаунт владельца: ' + tok.email + (tok.name ? ' (' + tok.name + ')' : ''));

  if (PHASE === 'token') { log('фаза token завершена'); return; }

  if (PHASE === 'all' || PHASE === 'probe') await phaseProbe(tok);
  if (PHASE === 'all' || PHASE === 'brand') await phaseBrand(tok);
  if (PHASE === 'all' || PHASE === 'client' || PHASE === 'probe') {
    const brands = await api('GET', `https://clientauthconfig.googleapis.com/v1/brands?project=${PROJECT}&pageSize=10`, tok.access_token);
    const brand = (brands.json && brands.json.brands && brands.json.brands[0]) || null;
    if (PHASE !== 'probe') await phaseClient(tok, brand);
  }
  if (PHASE === 'all' || PHASE === 'user') {
    const brands = await api('GET', `https://clientauthconfig.googleapis.com/v1/brands?project=${PROJECT}&pageSize=10`, tok.access_token);
    const brand = (brands.json && brands.json.brands && brands.json.brands[0]) || null;
    await phaseUser(tok, brand);
  }
  log('════ ГОТОВО ════');
}

main().then(() => process.exit(0)).catch((e) => { log('ОШИБКА: ' + (e.stack || e.message).split('\n').slice(0, 3).join(' | ')); process.exit(1); });
