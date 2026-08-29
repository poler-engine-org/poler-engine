/* gcp-cdp-machinery.js — проверенная CDP-машина из gcp-setup.js, как переиспользуемый модуль */
'use strict';
const { spawn } = require('child_process');
const https = require('https');
const http = require('http');
const net = require('net');
const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');

const CHROME = '/home/z/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome';
const PROFILE = '/home/z/.cache/poler-engine/google-profile';

const log = (m) => process.stdout.write(`[${new Date().toISOString()}] ${m}\n`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const b64json = (s) => { try { return JSON.parse(Buffer.from(s, 'base64').toString('utf8')); } catch (_) { return null; } };
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
  const r = await cdp.call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }, timeoutMs || 20000);
  if (r && r.exceptionDetails) {
    const d = r.exceptionDetails;
    return 'EXC|' + ((d.exception && d.exception.description) || d.text || 'unknown').slice(0, 200);
  }
  return (r.result && r.result.value) || '';
}

async function connectPageAny(cdpPort) {
  const list = await getJsonPort(cdpPort, '/json/list');
  const pages = (list || []).filter((t) => t.type === 'page' && t.webSocketDebuggerUrl);
  const page = pages[pages.length - 1];
  if (!page) throw new Error('нет page-target');
  const ws = await MiniWs.connect(cdpPort, wsPathOf(page.webSocketDebuggerUrl));
  const cdp = new CdpClient(ws);
  await cdp.call('Page.enable', {}, 8000);
  await cdp.call('Runtime.enable', {}, 8000);
  return cdp;
}

async function connectPageRetry(cdpPort, times) {
  for (let k = 0; k < (times || 5); k++) {
    try { return await connectPageAny(cdpPort); } catch (_) { await (new Promise((r) => setTimeout(r, 2000))); }
  }
  throw new Error('connectPage не удался');
}

async function evalRetry(ref, cdpPort, expr, timeoutMs) {
  for (let k = 0; k < 3; k++) {
    try { return await evalPage(ref.cdp, expr, timeoutMs || 15000); }
    catch (_) { ref.cdp = await connectPageRetry(cdpPort, 3); }
  }
  return 'ERR|eval не удался';
}

module.exports = { MiniWs, CdpClient, getJsonPort, wsPathOf, launchChromium, evalPage, connectPageAny, connectPageRetry, evalRetry, log, sleep, CHROME, PROFILE };
