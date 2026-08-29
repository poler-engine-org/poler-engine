#!/usr/bin/env node
'use strict';
/*!
 * auth-dev-orchestrator.js — «dev-сервер» проекта poler-auth-ui (v0.17.6).
 * ==========================================================================
 * Стандартная точка входа (package.json → scripts.dev), которую поднимает
 * платформа (.zscripts/dev.sh → bun run dev). Роль: один процесс-супервизор
 * для всего стенда авторизации Google:
 *
 *   Xvfb :99 ── auth-companion (:8765, окно Google, exit-контракт)
 *            └─ auth-preview relay (:3100, CDP screencast + ввод)
 *   next dev (:3000) ← превью платформы (Caddy :81 → :3000)
 *
 * Контракты платформы:
 *   • dev-сервер слушает :3000 (единственный порт превью);
 *   • .zscripts/dev.pid ← PID процесса dev-сервера;
 *   • dev.log ← вывод dev-сервера (Next.js пишет туда через нас).
 *
 * Идемпотентность: уже живые компоненты (после рестарта контейнера или
 * повторного запуска) не дублируются — проверка портов/процессов.
 * companion: exit-код остаётся контрактом движка (0 authorized / 2 closed /
 * 3 timeout / 4 preflight / 130 interrupted). Если вход НЕ завершился
 * (exit 2/3/4) — окно Google поднимается заново автоматически, чтобы
 * превью не «слетал» между сессиями владельца. exit 0 (сессия захвачена)
 * и 130 (остановлен вручную) — не перезапускаем.
 */

const { spawn, execSync } = require('child_process');
const net = require('net');
const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = '/home/z/my-project';
const REPO = path.join(ROOT, 'poler-engine-gh');
const LOGDIR = path.join(ROOT, 'logs');
const DEVLOG = path.join(ROOT, 'dev.log');
const DEVPID = path.join(ROOT, '.zscripts', 'dev.pid');
const CHROME = '/home/z/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome';
const SESSION_FILE = path.join(os.homedir(), '.config', 'poler-engine', 'google_session.json');
const STATE_FILE = path.join(os.homedir(), '.config', 'poler-engine', 'auth-companion.state.json');

/** Сессия уже захвачена? Тогда окно Google больше не поднимаем НИКОГДА:
 *  цель достигнута, повторный подъём = шторм окон + лишние CDP-порты. */
function sessionCaptured() {
  try {
    if (fs.statSync(SESSION_FILE).size > 0) return true;
  } catch (_) { /* файла нет */ }
  try {
    const raw = JSON.parse(fs.readFileSync(STATE_FILE, 'utf8'));
    if (raw && raw.state === 'authorized') return true;
  } catch (_) { /* файла нет */ }
  return false;
}

function log(m) { process.stdout.write(`[orchestrator] ${m}\n`); }
function appendDevLog(m) {
  try { fs.appendFileSync(DEVLOG, m + '\n'); } catch (_) { /* best-effort */ }
}

function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

function portOpen(port) {
  return new Promise((resolve) => {
    const s = net.connect({ host: '127.0.0.1', port, timeout: 700 });
    s.once('connect', () => { s.destroy(); resolve(true); });
    s.once('error', () => resolve(false));
    s.once('timeout', () => { s.destroy(); resolve(false); });
  });
}

async function waitFor(port, name, timeoutMs) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (await portOpen(port)) { log(`${name} готов :${port}`); return true; }
    await sleep(700);
  }
  log(`ТАЙМАУТ: ${name} не поднялся на :${port} за ${Math.round(timeoutMs / 1000)}с`);
  return false;
}

function xvfbAlive() {
  try {
    const r = execSync('pgrep -x Xvfb', { stdio: ['ignore', 'pipe', 'ignore'] });
    return r.toString().trim().length > 0;
  } catch (_) { return false; }
}

/* ---- дети и перезапуски ---- */

const children = new Set();
let stopping = false;

function up(cmd, args, opts = {}) {
  const c = spawn(cmd, args, {
    stdio: ['ignore', 'pipe', 'pipe'],
    cwd: opts.cwd || ROOT,
    env: opts.env || process.env,
  });
  const tag = opts.tag || cmd;
  const fwd = (d) => {
    const s = d.toString();
    process.stdout.write(`[${tag}] ${s}`);
    appendDevLog(`[${tag}] ${s.trimEnd()}`);
  };
  c.stdout.on('data', fwd);
  c.stderr.on('data', fwd);
  c.once('exit', (code, sig) => {
    children.delete(c);
    if (!stopping) log(`${tag} завершился: exit=${code} sig=${sig}`);
    if (opts.onExit && !stopping) opts.onExit(code);
  });
  children.add(c);
  return c;
}

const RELAY_ENV = {
  ...process.env,
  PREVIEW_LISTEN_PORT: '3100',
  COMPANION_PORT: '8765',
};

const COMPANION_ENV = {
  ...process.env,
  DISPLAY: ':99',
  POLER_CHROME_BIN: CHROME,
  POLER_CHROME_NO_SANDBOX: '1',
  POLER_AUTH_COMPANION_PORT: '8765',
  POLER_AUTH_TIMEOUT_SECS: '1800',
};

function startXvfb() {
  log('Xvfb не найден — поднимаю :99');
  up('Xvfb', [':99', '-screen', '0', '1280x900x24', '-nolisten', 'tcp'], { tag: 'xvfb' });
}

function startRelay() {
  log('релей :3100 не отвечает — поднимаю');
  up('node', [path.join(ROOT, 'scripts', 'auth-preview.js')],
    { tag: 'relay', env: RELAY_ENV });
}

let companionChild = null;
let lastCompanionLaunch = 0;
const companionRestarts = [];

function onCompanionExit(code) {
  companionChild = null;
  if (stopping) return;
  if (code === 0) { log('companion: exit=0 — сессия захвачена, окно НЕ поднимаю'); return; }
  if (sessionCaptured()) { log(`companion: exit=${code}, но google_session.json уже есть — окно НЕ поднимаю`); return; }
  if (code === 130) { log('companion: exit=130 — остановлен вручную, окно НЕ поднимаю'); return; }
  const now = Date.now();
  while (companionRestarts.length && now - companionRestarts[0] > 10 * 60e3) companionRestarts.shift();
  if (companionRestarts.length >= 6) {
    log('companion: серия падений подряд — авто-подъём приостановлен на 10 мин');
    return;
  }
  companionRestarts.push(now);
  const delay = code === 4 ? 4000 : 2000;
  log(`companion exit=${code} — поднимаю новое окно Google через ${delay / 1000}с`);
  setTimeout(async () => {
    if (stopping || companionChild) return;
    if (await portOpen(8765)) return; // уже поднят кем-то ещё
    startCompanion();
  }, delay);
}

function startCompanion() {
  log('companion :8765 не отвечает — открываю окно Google');
  lastCompanionLaunch = Date.now();
  companionChild = up('node', [path.join(REPO, 'scripts', 'auth-companion.js')],
    { tag: 'companion', cwd: REPO, env: COMPANION_ENV, onExit: onCompanionExit });
}

function startNext() {
  log('поднимаю next dev → :3000');
  const c = up('bun', ['run', 'next'], {
    tag: 'next',
    onExit: () => { setTimeout(() => { if (!stopping) startNext(); }, 2000); },
  });
  try {
    fs.mkdirSync(path.dirname(DEVPID), { recursive: true });
    fs.writeFileSync(DEVPID, `${c.pid}\n`);
  } catch (_) { /* best-effort */ }
  appendDevLog(`--- orchestrator: next dev pid=${c.pid} ---`);
  log(`next dev pid=${c.pid} (dev.pid обновлён)`);
}

/* ---- сигналы ---- */

function graceful(sig) {
  if (stopping) return;
  stopping = true;
  log(`${sig} — останавливаю стек (${children.size} детей)`);
  for (const c of children) { try { c.kill('SIGTERM'); } catch (_) {} }
  setTimeout(() => {
    for (const c of children) { try { c.kill('SIGKILL'); } catch (_) {} }
    process.exit(0);
  }, 3500);
}
process.on('SIGTERM', () => graceful('SIGTERM'));
process.on('SIGINT', () => graceful('SIGINT'));

/* ---- главный поток ---- */

async function main() {
  fs.mkdirSync(LOGDIR, { recursive: true });
  log('полер-auth-ui dev-оркестратор: старт');
  log(`node ${process.version}, cwd=${process.cwd()}`);

  // 1) Xvfb (дисплей для окна Chromium)
  if (!xvfbAlive()) {
    startXvfb();
    await sleep(1200);
  } else {
    log('Xvfb уже жив — переиспользуем');
  }

  // 2) релей :3100 (CDP screencast; пережидает отсутствие companion)
  if (!(await portOpen(3100))) {
    startRelay();
    await waitFor(3100, 'релей', 20000);
  } else {
    log('релей :3100 уже жив — переиспользуем');
  }

  // 3) companion :8765 (окно логина Google; exit — контракт, НЕ рестартим).
  //    Если сессия уже захвачена — вообще не поднимаем окно.
  if (sessionCaptured()) {
    log('google_session.json найден — сессия уже захвачена, окно Google не открываю');
  } else if (!(await portOpen(8765))) {
    startCompanion();
    await waitFor(8765, 'companion', 45000);
  } else {
    log('companion :8765 уже жив — переиспользуем');
  }

  // 4) next dev :3000 — тот самый «dev-сервер» для превью платформы
  startNext();
  await waitFor(3000, 'next dev', 120000);

  log('стек готов: превью платформы (:81) → next(:3000) → relay(:3100) → CDP');

  // надзор (раз в 3с): Xvfb и релей оживляем; next оживляет свой onExit;
  // companion — через onExit (плюс страховка для «усыновлённого» окна)
  for (;;) {
    await sleep(3000);
    if (stopping) return;
    if (!xvfbAlive()) { startXvfb(); await sleep(1200); }
    if (!(await portOpen(3100))) startRelay();
    if (!(await portOpen(8765)) && !companionChild &&
        Date.now() - lastCompanionLaunch > 8000) {
      if (sessionCaptured()) {
        // сессия уже на диске: окно не поднимаем, CDP не открываем
        continue;
      }
      log('надзор: окно Google не отвечает — поднимаю заново');
      startCompanion();
    }
  }
}

main().catch((e) => {
  console.error('[orchestrator] фатально:', e);
  process.exit(1);
});
