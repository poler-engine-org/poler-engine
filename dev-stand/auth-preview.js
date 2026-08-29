#!/usr/bin/env node
'use strict';
/*!
 * poler-engine auth-preview relay v1.0
 * =====================================
 * Мост «изолированный Chromium движка → браузер владельца» для
 * графического входа Google через превью платформы.
 *
 *   [браузер владельца]
 *        ⇅ wss/https (превью-прокси платформы: :81 → :3000)
 *   [этот релей — HTTP + WS, порт 3000]
 *        ├─ GET  /           страница-пульт (screencast + ввод)
 *        ├─ GET  /status     прокси статус-сервера companion (:8765)
 *        ├─ POST /shutdown   прокси graceful-завершения companion
 *        └─ WS   /ws         кадры JPEG + события мыши/клавиатуры
 *        ⇅ CDP (127.0.0.1:<случайный порт>)
 *   [auth-companion.js] — единственный, кто читает куки (не этот релей)
 *        └─ spawn → [Chromium @ Xvfb :99, изолированный профиль]
 *
 * Безопасность:
 *  • релей НЕ логирует и НЕ сохраняет ввод (пароль идёт транзитом
 *    в изолированный Chromium и дальше — только в Google);
 *  • релей НЕ читает куки/сессию — захват делает auth-companion.js;
 *  • CDP и статус companion остаются строго на 127.0.0.1.
 *
 * Zero-dependency (net, http, crypto, fs, path). Node >= 18.
 * Env: PREVIEW_LISTEN_HOST (127.0.0.1), PREVIEW_LISTEN_PORT (3000),
 *      COMPANION_PORT (8765).
 */

const http = require('http');
const net = require('net');
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const os = require('os');

const LISTEN_HOST = process.env.PREVIEW_LISTEN_HOST || '127.0.0.1';
const LISTEN_PORT = parseInt(process.env.PREVIEW_LISTEN_PORT || '3100', 10) || 3100;
const COMPANION_PORT = parseInt(process.env.COMPANION_PORT || '8765', 10) || 8765;
const STATE_PATH = process.env.POLER_STATE_FILE ||
  path.join(os.homedir(), '.config', 'poler-engine', 'auth-companion.state.json');
// Только ФАКТ существования снапшота (не содержимое — релей куки не читает)
const SESSION_PATH = process.env.POLER_SESSION_FILE ||
  path.join(os.homedir(), '.config', 'poler-engine', 'google_session.json');
const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';
const PAGE_HTML = path.join(__dirname, 'auth-preview.html');

function log(msg) { process.stdout.write(`[${new Date().toISOString()}] ${msg}\n`); }

// ---------------------------------------------------------------------------
// HTTP GET → JSON (локальные порты)
// ---------------------------------------------------------------------------

function httpGetJson(port, p, timeoutMs = 3000) {
  return new Promise((resolve, reject) => {
    const req = http.get({ host: '127.0.0.1', port, path: p, timeout: timeoutMs },
      (res) => {
        let b = '';
        res.on('data', (c) => { b += c; });
        res.on('end', () => {
          try { resolve(JSON.parse(b)); }
          catch (e) { reject(new Error(`GET :${port}${p}: не JSON`)); }
        });
      });
    req.once('timeout', () => req.destroy(new Error(`GET :${port}${p}: таймаут`)));
    req.once('error', reject);
  });
}

// ---------------------------------------------------------------------------
// MiniWs — минимальный RFC 6455 WS-КЛИЕНТ для CDP (как в auth-companion.js)
// ---------------------------------------------------------------------------

class MiniWs {
  constructor(sock) {
    this.sock = sock;
    this.buf = Buffer.alloc(0);
    this.fragments = [];
    this.fragOpcode = 0;
    this.onMessage = null; // (text) => void
    this.onClose = null;   // () => void
    this._closeEmitted = false;
  }

  static connect(port, wsPath, timeoutMs = 10000) {
    return new Promise((resolve, reject) => {
      const key = crypto.randomBytes(16).toString('base64');
      const sock = net.connect({ host: '127.0.0.1', port });
      let ws = null;
      let handshaked = false;
      let buf = Buffer.alloc(0);
      const timer = setTimeout(() => {
        sock.destroy();
        reject(new Error(`WS connect ${wsPath}: таймаут ${timeoutMs}мс`));
      }, timeoutMs);

      sock.once('error', (e) => { clearTimeout(timer); reject(e); });
      sock.once('connect', () => {
        sock.write(
          `GET ${wsPath} HTTP/1.1\r\n` +
          `Host: 127.0.0.1:${port}\r\n` +
          `Upgrade: websocket\r\n` +
          `Connection: Upgrade\r\n` +
          `Sec-WebSocket-Key: ${key}\r\n` +
          `Sec-WebSocket-Version: 13\r\n\r\n`);
      });
      sock.on('data', (d) => {
        if (!handshaked) {
          buf = Buffer.concat([buf, d]);
          const i = buf.indexOf('\r\n\r\n');
          if (i === -1) return;
          const head = buf.slice(0, i).toString('latin1');
          if (!/^HTTP\/1\.1 101/.test(head)) {
            clearTimeout(timer);
            sock.destroy();
            reject(new Error(`WS handshake отклонён: ${head.split('\r\n')[0]}`));
            return;
          }
          handshaked = true;
          clearTimeout(timer);
          ws = new MiniWs(sock);
          const rest = buf.slice(i + 4);
          resolve(ws);
          if (rest.length) ws._feed(rest);
          return;
        }
        if (ws) ws._feed(d);
      });
      sock.once('close', () => {
        if (ws) ws._shutdown();
        else if (!handshaked) {
          clearTimeout(timer);
          reject(new Error('WS: сокет закрыт до handshake'));
        }
      });
    });
  }

  get isOpen() {
    return !this._closeEmitted && this.sock && !this.sock.destroyed;
  }

  send(text) { this._sendFrame(0x1, Buffer.from(String(text), 'utf8')); }

  close() {
    try {
      if (this.isOpen) this._sendFrame(0x8, Buffer.alloc(0));
      this.sock.end();
    } catch (_) { /* уже закрыт */ }
    this._shutdown();
  }

  _feed(d) {
    this.buf = this.buf.length ? Buffer.concat([this.buf, d]) : d;
    for (;;) {
      const frame = this._parseFrame();
      if (!frame) break;
      this._handleFrame(frame);
      if (this._closeEmitted) break;
    }
  }

  _parseFrame() {
    const b = this.buf;
    if (b.length < 2) return null;
    const fin = (b[0] & 0x80) !== 0;
    const opcode = b[0] & 0x0f;
    const masked = (b[1] & 0x80) !== 0;
    let len = b[1] & 0x7f;
    let off = 2;
    if (len === 126) {
      if (b.length < off + 2) return null;
      len = b.readUInt16BE(off); off += 2;
    } else if (len === 127) {
      if (b.length < off + 8) return null;
      const hi = b.readUInt32BE(off);
      const lo = b.readUInt32BE(off + 4);
      len = hi * 4294967296 + lo;
      off += 8;
    }
    let maskKey = null;
    if (masked) {
      if (b.length < off + 4) return null;
      maskKey = b.slice(off, off + 4); off += 4;
    }
    if (b.length < off + len) return null;
    let payload = b.slice(off, off + len);
    this.buf = b.slice(off + len);
    if (maskKey) {
      const out = Buffer.allocUnsafe(payload.length);
      for (let i = 0; i < payload.length; i++) out[i] = payload[i] ^ maskKey[i & 3];
      payload = out;
    }
    return { fin, opcode, payload };
  }

  _handleFrame(f) {
    switch (f.opcode) {
      case 0x0:
        if (this.fragOpcode) {
          this.fragments.push(f.payload);
          if (f.fin) {
            const full = Buffer.concat(this.fragments);
            const op = this.fragOpcode;
            this.fragments = [];
            this.fragOpcode = 0;
            this._deliver(op, full);
          }
        }
        break;
      case 0x1:
      case 0x2:
        if (f.fin) this._deliver(f.opcode, f.payload);
        else { this.fragments = [f.payload]; this.fragOpcode = f.opcode; }
        break;
      case 0x8:
        try { if (this.isOpen) this._sendFrame(0x8, f.payload); } catch (_) {}
        this._shutdown();
        break;
      case 0x9: this._sendFrame(0xA, f.payload); break;
      case 0xA: break;
      default: break;
    }
  }

  _deliver(opcode, payload) {
    if (opcode === 0x1 && this.onMessage) {
      try { this.onMessage(payload.toString('utf8')); } catch (_) {}
    }
  }

  _sendFrame(opcode, payload) {
    if (!this.sock || this.sock.destroyed) return;
    const mask = crypto.randomBytes(4);
    const len = payload.length;
    let header;
    if (len < 126) {
      header = Buffer.alloc(2);
      header[1] = 0x80 | len;
    } else if (len < 65536) {
      header = Buffer.alloc(4);
      header[1] = 0x80 | 126;
      header.writeUInt16BE(len, 2);
    } else {
      header = Buffer.alloc(10);
      header[1] = 0x80 | 127;
      header.writeUInt32BE(Math.floor(len / 4294967296), 2);
      header.writeUInt32BE(len >>> 0, 6);
    }
    header[0] = 0x80 | opcode;
    const masked = Buffer.allocUnsafe(payload.length);
    for (let i = 0; i < payload.length; i++) masked[i] = payload[i] ^ mask[i & 3];
    this.sock.write(Buffer.concat([header, mask, masked]));
  }

  _shutdown() {
    if (this._closeEmitted) return;
    this._closeEmitted = true;
    if (this.onClose) {
      try { this.onClose(); } catch (_) {}
    }
  }
}

// ---------------------------------------------------------------------------
// CdpClient: запросы + события поверх MiniWs
// ---------------------------------------------------------------------------

class CdpClient {
  constructor(ws, onEvent) {
    this.ws = ws;
    this.nextId = 1;
    this.pending = new Map();
    this.onEvent = onEvent || null;
    ws.onMessage = (text) => {
      let m;
      try { m = JSON.parse(text); } catch (_) { return; }
      if (m && m.id && this.pending.has(m.id)) {
        const { resolve, reject } = this.pending.get(m.id);
        this.pending.delete(m.id);
        if (m.error) reject(new Error(m.error.message || 'CDP error'));
        else resolve(m.result);
      } else if (m && m.method && this.onEvent) {
        this.onEvent(m);
      }
    };
    ws.onClose = () => {
      for (const { reject } of this.pending.values()) {
        reject(new Error('CDP: соединение закрыто'));
      }
      this.pending.clear();
    };
  }

  call(method, params = {}, timeoutMs = 15000) {
    return new Promise((resolve, reject) => {
      if (!this.ws.isOpen) { reject(new Error('CDP: WS не открыт')); return; }
      const id = ++this.nextId;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP таймаут: ${method}`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (v) => { clearTimeout(timer); resolve(v); },
        reject: (e) => { clearTimeout(timer); reject(e); },
      });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
}

function wsPathOf(wsUrl) {
  const m = /ws:\/\/[^/]+(\/.*)$/.exec(String(wsUrl || ''));
  return m ? m[1] : '/devtools/browser';
}

// ---------------------------------------------------------------------------
// CDP-сессия страничного таргета: скринкаст + ввод
// ---------------------------------------------------------------------------

class CdpPageSession {
  constructor(cdpPort, target, hooks) {
    this.cdpPort = cdpPort;
    this.target = target;
    this.hooks = hooks; // { onFrame(dataB64, w, h), onDead(why) }
    this.cdp = null;
    this.alive = false;
    this.view = { w: 1280, h: 800 }; // viewport в CSS-пикселях (из метаданных кадров)
    this.lastFrameAt = 0;
    this.keepTimer = null;
  }

  async start() {
    const ws = await MiniWs.connect(this.cdpPort, wsPathOf(this.target.webSocketDebuggerUrl));
    this.cdp = new CdpClient(ws, (m) => this._handleEvent(m));
    ws.onClose = () => this._dead('CDP WS закрыт');
    await this.cdp.call('Page.enable');
    await this.cdp.call('Page.startScreencast', {
      format: 'jpeg', quality: 72, maxWidth: 1280, maxHeight: 940, everyNthFrame: 1,
    });
    this.alive = true;
    // Подстраховка: страница статична → кадров нет; снимок раз в 3с
    this.keepTimer = setInterval(() => this._keepAlive(), 3000);
  }

  async _keepAlive() {
    if (!this.alive) return;
    if (Date.now() - this.lastFrameAt < 3200) return;
    try {
      const r = await this.cdp.call('Page.captureScreenshot', { format: 'jpeg', quality: 70 });
      if (r && r.data) this.hooks.onFrame(r.data, this.view.w, this.view.h);
    } catch (_) { /* сессия могла умереть — onClose приберёт */ }
  }

  _handleEvent(m) {
    if (m.method === 'Page.screencastFrame' && m.params) {
      const p = m.params;
      const md = p.metadata || {};
      if (md.deviceWidth) this.view.w = md.deviceWidth;
      if (md.deviceHeight) this.view.h = md.deviceHeight;
      this.lastFrameAt = Date.now();
      this.hooks.onFrame(p.data, this.view.w, this.view.h);
      // ack обязателен — иначе Chrome перестанет слать кадры
      this.cdp.call('Page.screencastFrameAck', { sessionId: p.sessionId }).catch(() => {});
    }
  }

  // --- ввод: координаты в CSS-пикселях вьюпорта ---
  mouse(type, x, y, extra) {
    if (!this.alive) return;
    this.cdp.call('Input.dispatchMouseEvent',
      { type, x: Math.round(x), y: Math.round(y), ...extra }).catch(() => {});
  }

  key(type, params) {
    if (!this.alive) return;
    this.cdp.call('Input.dispatchKeyEvent', { type, ...params }).catch(() => {});
  }

  insertText(text) {
    if (!this.alive) return;
    this.cdp.call('Input.insertText', { text }).catch(() => {});
  }

  // --- навигация: кнопки «Назад»/«Вперёд»/«Обновить»/«Заново» ---
  navBack(fallbackUrl) {
    if (!this.alive) return;
    this.cdp.call('Page.getNavigationHistory').then((h) => {
      if (h && Array.isArray(h.entries) && h.currentIndex > 0) {
        const prev = h.entries[h.currentIndex - 1];
        return this.cdp.call('Page.navigateToHistoryEntry', { entryId: prev.id });
      }
      // истории нет (первая страница) — просто открываем страницу входа
      if (fallbackUrl) return this.cdp.call('Page.navigate', { url: fallbackUrl });
    }).catch(() => {});
  }

  navForward() {
    if (!this.alive) return;
    this.cdp.call('Page.getNavigationHistory').then((h) => {
      if (h && Array.isArray(h.entries) && h.currentIndex < h.entries.length - 1) {
        const next = h.entries[h.currentIndex + 1];
        return this.cdp.call('Page.navigateToHistoryEntry', { entryId: next.id });
      }
    }).catch(() => {});
  }

  reloadPage() {
    if (!this.alive) return;
    this.cdp.call('Page.reload', { ignoreCache: false }).catch(() => {});
  }

  gotoUrl(url) {
    if (!this.alive) return;
    let u;
    try { u = new URL(String(url)); } catch (_) { return; }
    // только http(s): никакого file:/chrome:// из ввода
    if (u.protocol !== 'https:' && u.protocol !== 'http:') return;
    this.cdp.call('Page.navigate', { url: u.href }).catch(() => {});
  }

  _dead(why) {
    if (!this.alive && !this.keepTimer) return;
    this.alive = false;
    if (this.keepTimer) { clearInterval(this.keepTimer); this.keepTimer = null; }
    try { if (this.cdp) this.cdp.ws.close(); } catch (_) {}
    log(`CDP-сессия закрыта: ${why}`);
    if (this.hooks.onDead) this.hooks.onDead(why);
  }

  close() { this._dead('закрыт релеем'); }
}

// ---------------------------------------------------------------------------
// WS-СЕРВЕР (браузер владельца): handshake + текстовые фреймы
// ---------------------------------------------------------------------------

const clients = new Set();

function wsAccept(key) {
  return crypto.createHash('sha1').update(key + WS_GUID).digest('base64');
}

/** Серверный текстовый фрейм (без маски; 64-битные длины поддерживаются). */
function frameText(text) {
  const payload = Buffer.from(String(text), 'utf8');
  const len = payload.length;
  let header;
  if (len < 126) {
    header = Buffer.alloc(2); header[1] = len;
  } else if (len < 65536) {
    header = Buffer.alloc(4); header[1] = 126; header.writeUInt16BE(len, 2);
  } else {
    header = Buffer.alloc(10); header[1] = 127;
    header.writeUInt32BE(Math.floor(len / 4294967296), 2);
    header.writeUInt32BE(len >>> 0, 6);
  }
  header[0] = 0x81; // FIN + text
  return Buffer.concat([header, payload]);
}

class BrowserConn {
  constructor(sock) {
    this.sock = sock;
    this.buf = Buffer.alloc(0);
    this.alive = true;
    sock.setNoDelay(true);
    sock.on('data', (d) => this._feed(d));
    sock.on('close', () => this._drop());
    sock.on('error', () => this._drop());
    clients.add(this);
    log(`WS-клиент подключился (всего ${clients.size})`);
  }

  send(obj) {
    if (!this.alive) return;
    try { this.sock.write(frameText(JSON.stringify(obj))); } catch (_) { this._drop(); }
  }

  ping() {
    if (!this.alive) return;
    try {
      const h = Buffer.alloc(2); h[0] = 0x89; h[1] = 0;
      this.sock.write(h);
    } catch (_) { this._drop(); }
  }

  _drop() {
    if (!this.alive) return;
    this.alive = false;
    clients.delete(this);
    try { this.sock.destroy(); } catch (_) {}
  }

  _feed(d) {
    this.buf = this.buf.length ? Buffer.concat([this.buf, d]) : d;
    for (;;) {
      const f = this._parse();
      if (!f) break;
      if (f.opcode === 0x8) { this._drop(); return; }
      if (f.opcode === 0x9) {
        try { const h = Buffer.alloc(2); h[0] = 0x8A; h[1] = 0; this.sock.write(h); } catch (_) {}
        continue;
      }
      if (f.opcode === 0x1) {
        try { onBrowserMessage(this, JSON.parse(f.payload.toString('utf8'))); } catch (_) {}
      }
    }
  }

  _parse() { // клиентские фреймы маскированы
    const b = this.buf;
    if (b.length < 2) return null;
    const opcode = b[0] & 0x0f;
    const masked = (b[1] & 0x80) !== 0;
    let len = b[1] & 0x7f;
    let off = 2;
    if (len === 126) { if (b.length < off + 2) return null; len = b.readUInt16BE(off); off += 2; }
    else if (len === 127) {
      if (b.length < off + 8) return null;
      len = b.readUInt32BE(off) * 4294967296 + b.readUInt32BE(off + 4); off += 8;
    }
    let maskKey = null;
    if (masked) { if (b.length < off + 4) return null; maskKey = b.slice(off, off + 4); off += 4; }
    if (b.length < off + len) return null;
    let payload = b.slice(off, off + len);
    this.buf = b.slice(off + len);
    if (maskKey) {
      const out = Buffer.allocUnsafe(payload.length);
      for (let i = 0; i < payload.length; i++) out[i] = payload[i] ^ maskKey[i & 3];
      payload = out;
    }
    return { opcode, payload };
  }
}

setInterval(() => { for (const c of clients) c.ping(); }, 30000);

// ---------------------------------------------------------------------------
// маршрутизация ввода: браузер владельца → CDP
// ---------------------------------------------------------------------------

const VK_MAP = {
  Enter: 13, Backspace: 8, Tab: 9, Shift: 16, Control: 17, Alt: 18, Meta: 91,
  Escape: 27, ' ': 32, PageUp: 33, PageDown: 34, End: 35, Home: 36,
  ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40, Delete: 46,
  CapsLock: 20,
};

/** Обработка одного события ввода (общая для WS и POST /input). */
function processInput(m) {
  if (!m || typeof m.t !== 'string') return;
  const s = session;
  if (!s || !s.alive) return;

  switch (m.t) {
    case 'md':
      s.mouse('mousePressed', m.x * s.view.w, m.y * s.view.h,
        { button: 'left', buttons: 1, clickCount: m.cc || 1 });
      break;
    case 'mu':
      s.mouse('mouseReleased', m.x * s.view.w, m.y * s.view.h,
        { button: 'left', buttons: 0, clickCount: m.cc || 1 });
      break;
    case 'mm':
      s.mouse('mouseMoved', m.x * s.view.w, m.y * s.view.h,
        { button: 'none', buttons: m.b || 0 });
      break;
    case 'wh':
      s.mouse('mouseWheel', m.x * s.view.w, m.y * s.view.h,
        { deltaX: Math.round(m.dx || 0), deltaY: Math.round(m.dy || 0) });
      break;
    case 'kd':
    case 'ku': {
      const isDown = m.t === 'kd';
      const printable = typeof m.key === 'string' && m.key.length === 1;
      const vk = m.vk || VK_MAP[m.key] ||
        (printable ? m.key.toUpperCase().charCodeAt(0) : 0);
      const params = {
        key: String(m.key || ''),
        code: String(m.code || ''),
        windowsVirtualKeyCode: vk,
        nativeVirtualKeyCode: vk,
        modifiers: m.mods || 0,
      };
      if (isDown && m.text) { params.text = m.text; params.unmodifiedText = m.text; }
      s.key(isDown ? (m.text ? 'keyDown' : 'rawKeyDown') : 'keyUp', params);
      break;
    }
    case 'ins':
      if (m.text) s.insertText(String(m.text).slice(0, 5000));
      break;
    case 'back':
      s.navBack('https://accounts.google.com/');
      break;
    case 'forward':
      s.navForward();
      break;
    case 'reload':
      s.reloadPage();
      break;
    case 'goto':
      if (m.url) s.gotoUrl(m.url);
      break;
    default: break;
  }
}

function onBrowserMessage(conn, m) {
  if (!m || typeof m.t !== 'string') return;
  if (m.t === 'hello') {
    conn.send({ t: 'state', s: companionState });
    if (lastFrame.d) conn.send({ t: 'frame', d: lastFrame.d, w: lastFrame.w, h: lastFrame.h });
    return;
  }
  processInput(m);
}

// ---------------------------------------------------------------------------
// жизненный цикл: опрос companion → CDP-сессия → вещание
// ---------------------------------------------------------------------------

let session = null;
let connecting = false;
let companionState = { state: 'starting', detail: '' };
let cdpPort = 0;
const lastFrame = { d: null, w: 1280, h: 800 };

const sseClients = new Set();

function broadcast(obj) {
  const line = 'data: ' + JSON.stringify(obj) + '\n\n';
  for (const c of clients) c.send(obj);
  for (const res of sseClients) {
    try { res.write(line); } catch (_) { sseClients.delete(res); }
  }
}

async function findPageTarget(port) {
  const list = await httpGetJson(port, '/json/list');
  const pages = (Array.isArray(list) ? list : []).filter(
    (t) => t && t.type === 'page' && t.webSocketDebuggerUrl &&
      !String(t.url || '').startsWith('devtools://'));
  return pages[0] || null;
}

async function ensureSession() {
  if (connecting || (session && session.alive)) return;
  if (!cdpPort) return;
  connecting = true;
  try {
    const target = await findPageTarget(cdpPort);
    if (!target) return;
    const s = new CdpPageSession(cdpPort, target, {
      onFrame: (d, w, h) => {
        lastFrame.d = d; lastFrame.w = w; lastFrame.h = h;
        broadcast({ t: 'frame', d, w, h });
      },
      onDead: () => { if (session === s) session = null; },
    });
    await s.start();
    session = s;
    broadcast({ t: 'log', m: `экран подключён: ${shortUrl(target.url)}` });
    log(`CDP-сессия открыта: ${target.url}`);
  } catch (e) {
    log(`CDP-сессия: ${e.message} (повтор через секунду)`);
  } finally {
    connecting = false;
  }
}

function shortUrl(u) {
  try { const x = new URL(u); return x.host + (x.pathname === '/' ? '' : x.pathname); }
  catch (_) { return String(u); }
}

async function pollCompanion() {
  let st = null;
  try { st = await httpGetJson(COMPANION_PORT, '/status', 1500); } catch (_) {}
  if (!st) {
    // companion мог корректно завершиться (authorized/closed/timeout) —
    // читаем финальный state-файл, чтобы владелец видел итог, а не «не отвечает»
    try {
      const raw = JSON.parse(fs.readFileSync(STATE_PATH, 'utf8'));
      const fresh = raw && raw.ts && (Date.now() - Date.parse(raw.ts)) < 6 * 3600e3;
      if (fresh && ['authorized', 'closed', 'timeout', 'error', 'interrupted'].includes(raw.state)) {
        st = { ...raw, final: true };
      }
    } catch (_) { /* нет файла — так нет */ }
  }
  const next = st || { state: 'companion-down', detail: 'companion не отвечает' };
  // снапшот сессии есть → показываем authorized, даже если state-файл остался
  // с промежуточным «waiting» от убитого окна (факт файла, без чтения куков)
  if (!st && next.state === 'companion-down') {
    try {
      if (fs.statSync(SESSION_PATH).size > 0) {
        next.state = 'authorized';
        next.detail = 'сессия захвачена (google_session.json)';
        next.final = true;
      }
    } catch (_) { /* файла нет — оставляем companion-down */ }
  }
  const changed = next.state !== companionState.state ||
    next.cookie_count !== companionState.cookie_count;
  companionState = next;
  if (changed) broadcast({ t: 'state', s: next });

  if (next.state === 'waiting' && next.cdp_port) {
    cdpPort = next.cdp_port;
    ensureSession();
  } else if (['authorized', 'closed', 'timeout', 'error', 'interrupted'].includes(next.state)) {
    if (session) { try { session.close(); } catch (_) {} session = null; }
  }
}

// ---------------------------------------------------------------------------
// HTTP-сервер релея
// ---------------------------------------------------------------------------

const server = http.createServer((req, res) => {
  const u = new URL(req.url, 'http://127.0.0.1');

  if (req.method === 'GET' && (u.pathname === '/' || u.pathname === '/index.html')) {
    try {
      res.writeHead(200, {
        'Content-Type': 'text/html; charset=utf-8',
        'Cache-Control': 'no-store',
      });
      res.end(fs.readFileSync(PAGE_HTML));
    } catch (e) {
      res.writeHead(500); res.end('page not found on disk');
    }
    return;
  }

  if (req.method === 'GET' && (u.pathname === '/status' || u.pathname === '/companion-status')) {
    const preq = http.get({ host: '127.0.0.1', port: COMPANION_PORT, path: '/status', timeout: 2000 },
      (pres) => {
        res.writeHead(pres.statusCode, {
          'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store',
        });
        pres.pipe(res);
      });
    preq.once('error', () => {
      res.writeHead(502, { 'Content-Type': 'application/json; charset=utf-8' });
      res.end(JSON.stringify({ state: 'companion-down' }));
    });
    return;
  }

  if (req.method === 'POST' && u.pathname === '/shutdown') {
    const preq = http.request({
      host: '127.0.0.1', port: COMPANION_PORT, path: '/shutdown',
      method: 'POST', timeout: 2000,
    }, (pres) => {
      res.writeHead(pres.statusCode, { 'Content-Type': 'application/json; charset=utf-8' });
      pres.pipe(res);
    });
    preq.once('error', () => {
      res.writeHead(502, { 'Content-Type': 'application/json; charset=utf-8' });
      res.end('{"error":"companion-down"}');
    });
    preq.end();
    return;
  }

  // --- API-плоскость для Next.js (проксируется как /api/*) ---

  if (req.method === 'GET' && u.pathname === '/frame') {
    if (lastFrame.d) {
      res.writeHead(200, {
        'Content-Type': 'application/json; charset=utf-8',
        'Cache-Control': 'no-store',
      });
      res.end(JSON.stringify(
        { t: 'frame', d: lastFrame.d, w: lastFrame.w, h: lastFrame.h }));
    } else {
      res.writeHead(204);
      res.end();
    }
    return;
  }

  if (req.method === 'GET' && u.pathname === '/state') {
    res.writeHead(200, {
      'Content-Type': 'application/json; charset=utf-8',
      'Cache-Control': 'no-store',
    });
    res.end(JSON.stringify({ t: 'state', s: companionState }));
    return;
  }

  if (req.method === 'POST' && u.pathname === '/input') {
    let body = '';
    req.on('data', (c) => {
      body += c;
      if (body.length > 1e6) { try { req.destroy(); } catch (_) {} }
    });
    req.on('end', () => {
      let j = null;
      try { j = JSON.parse(body); } catch (_) { /* некорректный JSON — игнор */ }
      const evs = Array.isArray(j && j.e) ? j.e : (j ? [j] : []);
      for (const ev of evs) {
        if (ev && ev.t === 'hello') {
          broadcast({ t: 'state', s: companionState });
          if (lastFrame.d) {
            broadcast({ t: 'frame', d: lastFrame.d, w: lastFrame.w, h: lastFrame.h });
          }
        } else {
          processInput(ev);
        }
      }
      // свежий кадр для poll-клиентов сразу после ввода
      if (session && session.alive) {
        setTimeout(() => { try { session._keepAlive(); } catch (_) {} }, 80);
      }
      res.writeHead(200, { 'Content-Type': 'application/json; charset=utf-8' });
      res.end('{"ok":true}');
    });
    return;
  }

  if (req.method === 'GET' && u.pathname === '/stream') {
    res.writeHead(200, {
      'Content-Type': 'text/event-stream; charset=utf-8',
      'Cache-Control': 'no-store',
      'Connection': 'keep-alive',
    });
    res.write('data: ' + JSON.stringify({ t: 'state', s: companionState }) + '\n\n');
    if (lastFrame.d) {
      res.write('data: ' + JSON.stringify(
        { t: 'frame', d: lastFrame.d, w: lastFrame.w, h: lastFrame.h }) + '\n\n');
    }
    sseClients.add(res);
    const hb = setInterval(() => {
      try { res.write(':hb\n\n'); } catch (_) { clearInterval(hb); }
    }, 15000);
    req.once('close', () => {
      clearInterval(hb);
      sseClients.delete(res);
    });
    return;
  }

  res.writeHead(404, { 'Content-Type': 'application/json; charset=utf-8' });
  res.end('{"error":"not found"}');
});

server.on('upgrade', (req, sock) => {
  let u;
  try { u = new URL(req.url, 'http://127.0.0.1'); } catch (_) { try { sock.destroy(); } catch (_) {} return; }
  if (u.pathname !== '/ws') { try { sock.destroy(); } catch (_) {} return; }
  const key = req.headers['sec-websocket-key'];
  if (!key) { try { sock.destroy(); } catch (_) {} return; }
  sock.write(
    'HTTP/1.1 101 Switching Protocols\r\n' +
    'Upgrade: websocket\r\n' +
    'Connection: Upgrade\r\n' +
    `Sec-WebSocket-Accept: ${wsAccept(key)}\r\n\r\n`);
  new BrowserConn(sock);
});

server.listen(LISTEN_PORT, LISTEN_HOST, () => {
  log(`auth-preview relay: http://${LISTEN_HOST}:${LISTEN_PORT} (Next.js /api/* проксирует сюда)`);
  log(`статус companion:   http://127.0.0.1:${COMPANION_PORT}/status`);
  log(`каналы: WS /ws, SSE /stream, polling /frame+/state, ввод POST /input`);
});

pollCompanion();
setInterval(pollCompanion, 1000);
