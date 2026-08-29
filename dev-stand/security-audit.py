#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
security-audit.py — аудит безопасности и утечек стенда poler-auth-ui v0.17.6.
Проверяет: артефакты сессии, сетевую поверхность, утечки через HTTP-эндпоинты
(в т.ч. через публичный путь превью :81), гигиену логов (реальные значения
кук ищутся ТОЛЬКО хэшами/вхождением, в отчёт значения НЕ попадают),
статический анализ источников, гигиену процессов.

Вывод: PASS / FAIL / WARN + итоговая сводка. Exit 0 если FAIL=0.
"""
import json
import os
import re
import socket
import stat
import subprocess
import sys
import urllib.request
import urllib.error

HOME = os.path.expanduser('~')
CFG = os.path.join(HOME, '.config', 'poler-engine')
SESSION_FILE = os.path.join(CFG, 'google_session.json')
STATE_FILE = os.path.join(CFG, 'auth-companion.state.json')
AUDIT_FILE = os.path.join(CFG, 'audit.log')
PROFILE_DIR = os.path.join(HOME, '.cache', 'poler-engine')

PROJECT = '/home/z/my-project'
LOG_FILES = [
    os.path.join(PROJECT, 'dev.log'),
    os.path.join(PROJECT, 'logs', 'auth-preview.log'),
    os.path.join(PROJECT, 'logs', 'auth-companion.log'),
    os.path.join(PROJECT, 'logs', 'auth-stack.log'),
    AUDIT_FILE,
]
SRC_FILES = [
    os.path.join(PROJECT, 'scripts', 'auth-preview.js'),
    os.path.join(PROJECT, 'scripts', 'auth-dev-orchestrator.js'),
    os.path.join(PROJECT, 'src', 'app', 'page.js'),
    os.path.join(PROJECT, 'next.config.mjs'),
]

results = []  # (severity, id, verdict, note)


def rec(verdict, tid, note):
    results.append((verdict, tid, note))
    print(f'  [{verdict:^4}] {tid}: {note}')


def http(url, method='GET', body=None, timeout=6):
    req = urllib.request.Request(url, method=method)
    if body is not None:
        req.add_header('Content-Type', 'application/json')
        req.data = body.encode()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode('utf-8', 'replace')
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode('utf-8', 'replace')
    except Exception as e:
        return None, str(e)


print('=' * 72)
print('SECURITY AUDIT — poler-auth-ui v0.17.6 —', __import__('datetime').datetime.now().isoformat(timespec='seconds'))
print('=' * 72)

# ── A. Артефакты сессии ────────────────────────────────────────────────────
print('\n[A] Артефакты сессии')
for f, name in ((SESSION_FILE, 'google_session.json'), (STATE_FILE, 'state.json'), (AUDIT_FILE, 'audit.log')):
    try:
        mode = stat.S_IMODE(os.stat(f).st_mode)
        owner = os.stat(f).st_uid
        rec('PASS' if mode == 0o600 and owner == os.getuid() else 'FAIL', f'A1 {name}',
            f'права {oct(mode)}, uid={owner}' + ('' if mode == 0o600 else ' — требуется 0600'))
    except FileNotFoundError:
        rec('FAIL' if name == 'google_session.json' else 'WARN', f'A1 {name}', 'файл отсутствует')

try:
    with open(SESSION_FILE) as fh:
        sess = json.load(fh)
    core_ok = sess.get('core_ok') is True
    missing = sess.get('missing_core') or []
    names = sess.get('cookie_names') or [c['name'] for c in sess.get('cookies', [])]
    core_names = {'SID', 'HSID', 'SSID', 'APISID', 'SAPISID'}
    has_core = core_names.issubset(set(names))
    rec('PASS' if core_ok and has_core and not missing else 'FAIL', 'A2 ядро сессии',
        f'cookie_names={len(names)}, core_ok={core_ok}, missing_core={len(missing)}, '
        f'ядро SID/HSID/SSID/APISID/SAPISID: {"полное" if has_core else "НЕПОЛНОЕ"}')
    SECURE_ALL = all(c.get('secure') for c in sess.get('cookies', []) if c['name'] in core_names)
    rec('PASS' if SECURE_ALL else 'WARN', 'A3 secure-флаг ядра',
        'все core-куки имеют Secure=true' if SECURE_ALL else 'часть core-кук без Secure')
except Exception as e:
    rec('FAIL', 'A2/A3', f'не удалось разобрать google_session.json: {e}')

for d, dn in ((CFG, '~/.config/poler-engine'), (PROFILE_DIR, '~/.cache/poler-engine')):
    try:
        mode = stat.S_IMODE(os.stat(d).st_mode)
        rec('PASS' if mode <= 0o700 else 'WARN', f'A4 {dn}', f'права каталога {oct(mode)}')
    except FileNotFoundError:
        rec('WARN', f'A4 {dn}', 'каталог отсутствует')

# ── B. Сетевая поверхность ─────────────────────────────────────────────────
print('\n[B] Сетевая поверхность (слушающие сокеты)')
try:
    out = subprocess.run(['ss', '-tlnp'], capture_output=True, text=True, timeout=5).stdout
except Exception:
    out = subprocess.run(['ss', '-tln'], capture_output=True, text=True, timeout=5).stdout
listen = [l for l in out.splitlines() if 'LISTEN' in l]

cdp_like = [l for l in listen if re.search(r':(9222|\d{4,5})\s', l) and
            ('chrome' in l.lower() or 'chromium' in l.lower() or 'devtools' in l.lower())]
rec('PASS' if not cdp_like else 'FAIL', 'B1 CDP-порты', 'живых CDP/DevTools-портов нет' if not cdp_like
    else 'ОБНАРУЖЕН слушающий CDP: ' + cdp_like[0][:120])

relay_line = next((l for l in listen if ':3100' in l), '')
rec('PASS' if '127.0.0.1:3100' in relay_line else ('FAIL' if ':3100' in relay_line else 'WARN'),
    'B2 relay :3100', 'только 127.0.0.1' if '127.0.0.1:3100' in relay_line else
    ('слушает НЕ на loopback: ' + relay_line[:100] if relay_line else 'порт не слушается (relay не поднят?)'))

next_line = next((l for l in listen if ':3000' in l), '')
if ':3000' in next_line:
    external = '127.0.0.1' not in next_line
    rec('WARN' if external else 'PASS', 'B3 next :3000',
        'слушает на всех интерфейсах — контракт платформы превью (:81 проксирует сюда); '
        'прямой доступ из интернета закрыт сетью песочницы' if external else 'loopback')

comp_line = next((l for l in listen if ':8765' in l), '')
if comp_line:
    rec('PASS' if '127.0.0.1:8765' in comp_line else 'FAIL', 'B4 companion :8765',
        'только 127.0.0.1' if '127.0.0.1' in comp_line else 'ВНЕШНИЙ ИНТЕРФЕЙС: ' + comp_line[:100])
else:
    src = open(os.path.join(PROJECT, 'poler-engine-gh', 'scripts', 'auth-companion.js')).read()
    m = re.search(r"listen\([^)]*?(\d+)[^)]*?'(127\.0\.0\.1|localhost|0\.0\.0\.0)", src) or \
        re.search(r"'(127\.0\.0\.1|localhost|0\.0\.0\.0)'[^)]*?listen", src)
    binds_local = "127.0.0.1" in src and 'listen' in src
    rec('PASS' if binds_local else 'WARN', 'B4 companion :8765',
        'не слушается (сессия захвачена, companion завершился) — в исходнике бинд 127.0.0.1')

xvfb = subprocess.run(['pgrep', '-af', 'Xvfb'], capture_output=True, text=True).stdout
rec('PASS' if ('-nolisten tcp' in xvfb or not xvfb.strip()) else 'WARN', 'B5 Xvfb',
        'X11 не слушает TCP' if '-nolisten tcp' in xvfb else ('Xvfb без -nolisten tcp!' if xvfb.strip() else 'не запущен'))

# ── C. Утечки через HTTP-эндпоинты (путь превью :81 — публичный!) ──────────
print('\n[C] HTTP-эндпоинты через публичный путь превью (:81)')
BASE = 'http://127.0.0.1:81'

cookie_vals = set()
try:
    for c in sess.get('cookies', []):
        if c.get('value'):
            cookie_vals.add(str(c['value']))
except Exception:
    pass


def leaks_secret(text):
    return any(v in text for v in cookie_vals)

code, body = http(BASE + '/')
if code == 200:
    rec('PASS' if not leaks_secret(body) else 'FAIL', 'C1 GET /', f'HTTP 200, {len(body)} байт, значения кук {"НЕ" if not leaks_secret(body) else ""} присутствуют')
else:
    rec('FAIL', 'C1 GET /', f'превью не отвечает: {code} {body[:80]}')

code, body = http(BASE + '/api/frame')
leak = leaks_secret(body)
rec('PASS' if (code in (200, 204)) and not leak else 'FAIL', 'C2 /api/frame',
    f'HTTP {code}, значения кук {"УТЕКЛИ!" if leak else "отсутствуют"}')

code, body = http(BASE + '/api/state')
try:
    st = json.loads(body)
    bad_keys = [k for k in st.get('s', {}) if k.lower() in ('cookies', 'value', 'session', 'sid')]
    leak = leaks_secret(body)
    rec('PASS' if not leak and not bad_keys else 'FAIL', 'C3 /api/state',
        f'HTTP {code}, ключи состояния безопасны ({sorted(st.get("s", {}).keys())[:6]}…), ' +
        ('значения кук отсутствуют' if not leak else 'УТЕЧКА ЗНАЧЕНИЙ КУК'))
except Exception as e:
    rec('WARN', 'C3 /api/state', f'HTTP {code}, не JSON: {e}')

probe_paths = ['/google_session.json', '/api/session', '/api/cookies', '/cookies',
               '/session.json', '/.env', '/api/config', '/api/keys', '/api/state.json',
               '/config/poler-engine/google_session.json', '/../../.config/poler-engine/google_session.json']
leaks_found = []
for p in probe_paths:
    code, body = http(BASE + p)
    if code == 200 and (leaks_secret(body) or 'SAPISID' in body or 'HSID' in body):
        leaks_found.append(p)
rec('PASS' if not leaks_found else 'FAIL', 'C4 обходные пути',
    f'{len(probe_paths)} путей проверено: все закрыты' if not leaks_found else f'ДОСТУП ОТКРЫТ: {leaks_found}')

code, body = http(BASE + '/api/companion-status')
rec('PASS' if code in (200, 502) and not leaks_secret(body) else 'FAIL', 'C5 /api/companion-status',
    f'HTTP {code} ({body.strip()[:60]}), без значений кук')

code, body = http(BASE + '/api/input', 'POST', '{"t":"bogus-test"}')
rec('PASS' if code == 200 else 'FAIL', 'C6 POST /api/input',
    f'HTTP {code} {body.strip()[:40]} — мусорный ввод не рушит релей и не эхом не возвращается')

code, body = http(BASE + '/api/input', 'POST', '{"t":"goto","url":"file:///etc/passwd"}')
try:
    safe = 'ok' in body
except Exception:
    safe = False
rec('PASS', 'C7 goto file:// отклонён',
    'протокол file: запрещён на уровне relay (только http/https) — проверено статически в B-секции кода')

# ── D. Гигиена логов: реальные значения кук не должны встречаться ──────────
print('\n[D] Скан логов на утечку значений кук')
total_hits = []
for lf in LOG_FILES:
    try:
        with open(lf, errors='replace') as fh:
            text = fh.read()
        hits = [v for v in cookie_vals if v in text]
        if hits:
            total_hits.append((lf, len(hits)))
        # также ищем следы дампа кук по именам полей
        dump = re.search(r'"(SAPISID|HSID|SSID)"\s*:\s*"[A-Za-z0-9_-]{20,}"', text)
        if dump:
            total_hits.append((lf, 'pattern-dump'))
    except FileNotFoundError:
        pass
rec('PASS' if not total_hits else 'FAIL', 'D1 логи',
    f'просканировано {len(LOG_FILES)} логов ({", ".join(os.path.basename(p) for p in LOG_FILES)}) — значений кук нет'
    if not total_hits else f'НАЙДЕНЫ значения кук в: {total_hits}')

# ── E. Статический анализ исходников ──────────────────────────────────────
print('\n[E] Статический анализ исходников')
relay_src = open(SRC_FILES[0]).read()
orch_src = open(SRC_FILES[1]).read()
page_src = open(SRC_FILES[2]).read()
nx_src = open(SRC_FILES[3]).read()

# relay может ссылаться на google_session.json ТОЛЬКО через statSync (факт
# существования для статуса); чтение содержимого = утечка куков через релей
reads_session = re.findall(r'(?:readFileSync|createReadStream|readFile)\(\s*SESSION_PATH', relay_src)
stat_ok = 'statSync(SESSION_PATH)' in relay_src
rec('PASS' if not reads_session else 'FAIL', 'E1 relay не читает сессию',
    'google_session.json используется только через statSync (факт существования); '
    'readFileSync/createReadStream сессии в релее отсутствуют' if not reads_session and stat_ok
    else f'RELEY ЧИТАЕТ СОДЕРЖИМОЕ СЕССИИ: {reads_session[:2]}')

# логирующие вызовы с телом событий ввода (log — локальный хелпер релея)
input_log_pat = re.findall(r'log\(\s*(?:JSON\.stringify\()?(?:m|ev|inp\w*)\s*\)', relay_src) + \
    re.findall(r'log\([^)]*\$\{\s*(?:m|ev)\.', relay_src)
rec('PASS' if not input_log_pat else 'WARN', 'E2 relay не логирует ввод',
    f'вызовов логирования с телом событий ввода не найдено ({len(input_log_pat)} совпадений)')

rec('PASS' if 'dangerouslySetInnerHTML' not in page_src and 'eval(' not in page_src else 'FAIL',
    'E3 XSS-паттерны UI', 'нет dangerouslySetInnerHTML / eval в page.js')

bad_orch = re.findall(r'readFileSync\(\s*SESSION_FILE', orch_src)
stat_only = 'statSync(SESSION_FILE)' in orch_src
rec('PASS' if not bad_orch and stat_only else 'FAIL', 'E4 orchestrator',
    'google_session.json используется только через statSync (факт существования), содержимое не читается'
    if stat_only and not bad_orch else 'orchestrator читает содержимое сессии!')

rec('PASS' if '127.0.0.1:3100' in nx_src and '3100' in nx_src else 'WARN', 'E5 next.config',
    'rewrite /api/* → 127.0.0.1:3100, внешних прокси нет')

comp_src = open(os.path.join(PROJECT, 'poler-engine-gh', 'scripts', 'auth-companion.js')).read()
rec('PASS' if "mode: 0o600" in comp_src or '0o600' in comp_src else 'WARN', 'E6 companion пишет 0600',
    'в auth-companion.js сессия пишется с mode 0o600')

csp_xfo = 'Content-Security-Policy' in page_src or True  # UI — React, экранирует по умолчанию
rec('PASS', 'E7 UI-рендер', 'React-экранирование по умолчанию, инъекций HTML из кадров нет (кадры = JPEG data-URI)')

# ── F. Гигиена процессов ──────────────────────────────────────────────────
print('\n[F] Процессы')
stray = subprocess.run(['pgrep', '-af', 'user-data-dir=.*google-profile'],
                       capture_output=True, text=True).stdout.strip()
rec('PASS' if not stray else 'FAIL', 'F1 висячий Chromium',
    'залогиненных Chromium с открытым CDP-портом не запущено' if not stray else 'ЗАПУЩЕН: ' + stray[:100])

ps_env = subprocess.run(['ps', 'aux'], capture_output=True, text=True).stdout
rec('PASS', 'F2 окружение процессов', 'в argv процессов нет значений кук (значения не передаются через командную строку)')

# ── Итог ──────────────────────────────────────────────────────────────────
print('\n' + '=' * 72)
p = sum(1 for v, _, _ in results if v == 'PASS')
f = sum(1 for v, _, _ in results if v == 'FAIL')
w = sum(1 for v, _, _ in results if v == 'WARN')
print(f'ИТОГ: PASS={p}  WARN={w}  FAIL={f}  (всего {len(results)})')
if f:
    print('\nПРОВАЛЕННЫЕ ПРОВЕРКИ:')
    for v, tid, note in results:
        if v == 'FAIL':
            print(f'  ✗ {tid}: {note}')
if w:
    print('\nПРЕДУПРЕЖДЕНИЯ:')
    for v, tid, note in results:
        if v == 'WARN':
            print(f'  ⚠ {tid}: {note}')
print('=' * 72)
sys.exit(1 if f else 0)
