#!/usr/bin/env node
'use strict';
/* test-relay-ws.js — проверка релея: подключиться к /ws, получить hello-кадр
 * и state. Доказывает, что страница-пульт получит скринкаст. */

const net = require('net');
const crypto = require('crypto');
const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

const PORT = parseInt(process.argv[2] || '3000', 10);

const key = crypto.randomBytes(16).toString('base64');
const sock = net.connect({ host: '127.0.0.1', port: PORT });
let buf = Buffer.alloc(0);
let handshaked = false;
let got = { frame: false, state: false, frames: 0, bytes: 0 };
const t0 = Date.now();

sock.once('error', (e) => { console.error('ОШИБКА:', e.message); process.exit(1); });
sock.once('connect', () => {
  sock.write(`GET /ws HTTP/1.1\r\nHost: 127.0.0.1:${PORT}\r\nUpgrade: websocket\r\n` +
    `Connection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`);
});
sock.on('data', (d) => {
  buf = Buffer.concat([buf, d]);
  if (!handshaked) {
    const i = buf.indexOf('\r\n\r\n');
    if (i === -1) return;
    const head = buf.slice(0, i).toString('latin1');
    if (!/^HTTP\/1\.1 101/.test(head)) { console.error('handshake fail:', head.split('\r\n')[0]); process.exit(1); }
    handshaked = true;
    buf = buf.slice(i + 4);
    // hello
    const payload = Buffer.from(JSON.stringify({ t: 'hello' }));
    const mask = crypto.randomBytes(4);
    const h = Buffer.alloc(2); h[0] = 0x81; h[1] = 0x80 | payload.length;
    const masked = Buffer.allocUnsafe(payload.length);
    for (let k = 0; k < payload.length; k++) masked[k] = payload[k] ^ mask[k & 3];
    sock.write(Buffer.concat([h, mask, masked]));
  }
  // разбор серверских фреймов (не маскированы)
  for (;;) {
    if (buf.length < 2) break;
    const opcode = buf[0] & 0x0f;
    const masked = (buf[1] & 0x80) !== 0;
    let len = buf[1] & 0x7f;
    let off = 2;
    if (len === 126) { if (buf.length < off + 2) break; len = buf.readUInt16BE(off); off += 2; }
    else if (len === 127) {
      if (buf.length < off + 8) break;
      len = buf.readUInt32BE(off) * 4294967296 + buf.readUInt32BE(off + 4); off += 8;
    }
    let mk = null;
    if (masked) { if (buf.length < off + 4) break; mk = buf.slice(off, off + 4); off += 4; }
    if (buf.length < off + len) break;
    let payload = buf.slice(off, off + len);
    buf = buf.slice(off + len);
    if (mk) {
      const out = Buffer.allocUnsafe(payload.length);
      for (let k = 0; k < payload.length; k++) out[k] = payload[k] ^ mk[k & 3];
      payload = out;
    }
    if (opcode === 0x1) {
      let m; try { m = JSON.parse(payload.toString('utf8')); } catch (_) { continue; }
      if (m.t === 'frame') {
        got.frame = true; got.frames++; got.bytes += (m.d || '').length;
        if (got.frames === 1) console.log(`кадр#1: ${m.d.length} b64-символов, viewport ${m.w}x${m.h}`);
      } else if (m.t === 'state') {
        got.state = true;
        console.log(`state: ${m.s && m.s.state} (cdp :${m.s && m.s.cdp_port}, cookies=${m.s && m.s.cookie_count})`);
      }
    }
  }
  if (got.frame && got.state && Date.now() - t0 > 4000) {
    console.log(`OK: frames=${got.frames}, ~${Math.round(got.bytes / 1024)}KB за ${((Date.now() - t0) / 1000).toFixed(1)}с`);
    process.exit(0);
  }
});
setTimeout(() => {
  if (got.frame && got.state) {
    console.log(`OK: frames=${got.frames}, ~${Math.round(got.bytes / 1024)}KB за 6с`);
    process.exit(0);
  }
  console.error('ПРОВАЛ: нет кадров или state', JSON.stringify(got));
  process.exit(1);
}, 6000);
