'use client';

import { useCallback, useEffect, useRef, useState } from 'react';

/* ── карта клавиш → virtual key codes ─────────────────────────────── */
const VK = {
  Enter: 13, Backspace: 8, Tab: 9, Shift: 16, Control: 17, Alt: 18, Meta: 91,
  Escape: 27, ' ': 32, PageUp: 33, PageDown: 34, End: 35, Home: 36,
  ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40, Delete: 46,
  CapsLock: 20,
};

const STATE_LABELS = {
  starting:         ['companion стартует…', 'waiting'],
  waiting:          ['окно открыто — ждём входа', 'waiting'],
  authorized:       ['сессия захвачена', 'authorized'],
  closed:           ['окно закрыто без входа', 'bad'],
  timeout:          ['таймаут входа', 'bad'],
  error:            ['ошибка companion', 'bad'],
  interrupted:      ['прервано', 'bad'],
  'companion-down': ['companion не отвечает', 'bad'],
};

export default function AuthPage() {
  const [frame, setFrame] = useState(null);          // {d,w,h}
  const [st, setSt] = useState(null);                // статус companion
  const [mode, setMode] = useState('connecting');    // sse | poll
  const [events, setEvents] = useState([]);
  const imgRef = useRef(null);
  const wrapRef = useRef(null);
  const modeRef = useRef('connecting');
  const t0 = useRef(Date.now());
  const [uptime, setUptime] = useState(0);

  const logLine = useCallback((m) => {
    const line = new Date().toLocaleTimeString('ru-RU') + ' · ' + m;
    setEvents((prev) => [line, ...prev].slice(0, 8));
  }, []);

  /* ── поток кадров: SSE с fallback на polling ──────────────────── */
  useEffect(() => {
    let es = null;
    let pollTimer = null;
    let failed = 0;
    let closed = false;

    const startPolling = () => {
      if (pollTimer || closed) return;
      modeRef.current = 'poll';
      setMode('poll');
      logLine('прямой поток недоступен — режим опроса');
      const tick = async () => {
        try {
          const r = await fetch('/api/frame', { cache: 'no-store' });
          if (r.ok) { const f = await r.json(); if (f && f.d) setFrame(f); }
        } catch (_) { /* сеть мигнула */ }
        try {
          const r2 = await fetch('/api/state', { cache: 'no-store' });
          if (r2.ok) { const j = await r2.json(); if (j && j.s) setSt(j.s); }
        } catch (_) { /* сеть мигнула */ }
      };
      tick();
      pollTimer = setInterval(tick, 500);
    };

    try {
      es = new EventSource('/api/stream');
      es.onopen = () => { modeRef.current = 'sse'; setMode('sse'); failed = 0; };
      es.onmessage = (ev) => {
        let m; try { m = JSON.parse(ev.data); } catch (_) { return; }
        if (m.t === 'frame') setFrame({ d: m.d, w: m.w, h: m.h });
        else if (m.t === 'state') setSt(m.s);
        else if (m.t === 'log') logLine(m.m);
      };
      es.onerror = () => {
        failed += 1;
        if (failed >= 2) { try { es.close(); } catch (_) {} es = null; startPolling(); }
      };
    } catch (_) { startPolling(); }

    // страховка: если за 4с ни одного кадра по SSE — уходим в опрос
    const guard = setTimeout(() => {
      if (modeRef.current !== 'sse' && !pollTimer) startPolling();
    }, 4000);

    return () => {
      closed = true;
      clearTimeout(guard);
      if (es) { try { es.close(); } catch (_) {} }
      if (pollTimer) clearInterval(pollTimer);
    };
  }, [logLine]);

  /* ── таймер сессии ────────────────────────────────────────────── */
  useEffect(() => {
    const id = setInterval(() => setUptime(Math.floor((Date.now() - t0.current) / 1000)), 1000);
    return () => clearInterval(id);
  }, []);

  /* ── отправка ввода в изолированный Chromium ──────────────────── */
  const send = useCallback(async (ev) => {
    try {
      await fetch('/api/input', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(ev),
        keepalive: true,
      });
      // событие ввода → сразу подтянуть свежий кадр (отзывчивость печати)
      const isNav = ['back', 'forward', 'reload', 'goto'].includes(ev.t);
      const refetch = (ms) => setTimeout(() => {
        fetch('/api/frame', { cache: 'no-store' })
          .then((r) => (r.ok ? r.json() : null))
          .then((f) => { if (f && f.d) setFrame(f); })
          .catch(() => {});
      }, ms);
      if (isNav) { refetch(800); refetch(2000); } // навигация — кадр с задержкой
      else if (modeRef.current === 'poll' || ev.t === 'md' || ev.t === 'wh') refetch(0);
    } catch (_) { /* сеть мигнула — не беда */ }
  }, []);

  /* ── мышь (координаты нормализованы 0..1) ─────────────────────── */
  const rel = (cx, cy) => {
    const el = imgRef.current;
    if (!el) return { x: 0, y: 0 };
    const r = el.getBoundingClientRect();
    return {
      x: Math.min(1, Math.max(0, (cx - r.left) / r.width)),
      y: Math.min(1, Math.max(0, (cy - r.top) / r.height)),
    };
  };

  const onMouseDown = (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const p = rel(e.clientX, e.clientY);
    send({ t: 'md', x: p.x, y: p.y, cc: e.detail || 1 });
  };
  const onMouseUp = (e) => {
    if (e.button !== 0) return;
    const p = rel(e.clientX, e.clientY);
    send({ t: 'mu', x: p.x, y: p.y, cc: e.detail || 1 });
  };
  const lastMove = useRef(0);
  const onMouseMove = (e) => {
    const now = performance.now();
    if (now - lastMove.current < 60) return;
    lastMove.current = now;
    const p = rel(e.clientX, e.clientY);
    send({ t: 'mm', x: p.x, y: p.y, b: e.buttons });
  };
  const onWheel = (e) => {
    e.preventDefault();
    const p = rel(e.clientX, e.clientY);
    send({ t: 'wh', x: p.x, y: p.y, dx: e.deltaX, dy: e.deltaY });
  };
  const onTouchStart = (e) => {
    const t = e.touches[0]; if (!t) return;
    const p = rel(t.clientX, t.clientY);
    send({ t: 'md', x: p.x, y: p.y, cc: 1 });
  };
  const onTouchEnd = (e) => {
    const t = e.changedTouches[0]; if (!t) return;
    const p = rel(t.clientX, t.clientY);
    send({ t: 'mu', x: p.x, y: p.y, cc: 1 });
  };

  /* ── клавиатура ───────────────────────────────────────────────── */
  const mods = (e) =>
    (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
  const keyPayload = (e) => {
    const printable = e.key && e.key.length === 1;
    let text = '';
    if (printable) text = e.key;
    else if (e.key === 'Enter') text = '\r';
    else if (e.key === 'Tab') text = '\t';
    return {
      key: e.key, code: e.code,
      vk: VK[e.key] || (printable ? e.key.toUpperCase().charCodeAt(0) : 0),
      text, mods: mods(e),
    };
  };
  const ownKey = (e) =>
    e.key === 'F5' ||
    ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 'r') ||
    ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'v');

  useEffect(() => {
    const kd = (e) => { if (ownKey(e)) return; e.preventDefault(); send({ t: 'kd', ...keyPayload(e) }); };
    const ku = (e) => { if (ownKey(e)) return; e.preventDefault(); send({ t: 'ku', ...keyPayload(e) }); };
    const paste = (e) => {
      const cd = e.clipboardData;
      const text = cd ? cd.getData('text') : '';
      if (text) { send({ t: 'ins', text: text.slice(0, 5000) }); e.preventDefault(); }
    };
    window.addEventListener('keydown', kd);
    window.addEventListener('keyup', ku);
    window.addEventListener('paste', paste);
    return () => {
      window.removeEventListener('keydown', kd);
      window.removeEventListener('keyup', ku);
      window.removeEventListener('paste', paste);
    };
  }, [send]);

  /* ── производный UI-стейт ─────────────────────────────────────── */
  const stateKey = (st && st.state) || 'unknown';
  const label = STATE_LABELS[stateKey] || [stateKey, ''];
  const authorized = stateKey === 'authorized';
  const detailParts = [];
  if (st && st.detail) detailParts.push(String(st.detail));
  if (st && typeof st.cookie_count === 'number' && st.cookie_count > 0)
    detailParts.push('google-кук: ' + st.cookie_count);
  if (st && st.cdp_port) detailParts.push('CDP :' + st.cdp_port);

  return (
    <main style={S.page}>
      <style>{CSS}</style>
      <header style={S.header}>
        <div style={S.brand}>
          <b>poler-engine</b>
          <span style={S.brandVer}>auth companion · v0.17.6</span>
        </div>
        <div className={'chip ' + (label[1] || '')} style={S.chip}>
          <span className="dot" />
          <span>{label[0]}</span>
        </div>
        <div className={'chip ' + (mode === 'sse' ? 'authorized' : mode === 'poll' ? 'waiting' : '')} style={S.chip}>
          <span className="dot" />
          <span>{mode === 'sse' ? 'поток' : mode === 'poll' ? 'опрос' : 'канал…'}</span>
        </div>
        <div style={S.spacer} />
        <div style={S.timer}>{uptime} c</div>
      </header>

      <div style={S.main}>
        <div style={S.stage}>
          <div
            ref={wrapRef}
            style={S.screenWrap}
            onMouseDown={onMouseDown}
            onMouseUp={onMouseUp}
            onMouseMove={onMouseMove}
            onWheel={onWheel}
            onTouchStart={onTouchStart}
            onTouchEnd={onTouchEnd}
            onContextMenu={(e) => e.preventDefault()}
          >
            {frame && frame.d ? (
              <img
                ref={imgRef}
                src={'data:image/jpeg;base64,' + frame.d}
                alt="Экран изолированного Chromium"
                draggable={false}
                style={S.screen}
              />
            ) : (
              <div style={S.placeholder}>
                <div className="spin" />
                <div>{authorized ? '' : 'Ждём окно Chromium…'}</div>
              </div>
            )}
            {authorized && (
              <div style={S.successOverlay}>
                <div style={S.successTitle}>Сессия Google захвачена ✓</div>
                <div>
                  Куки сохранены в google_session.json (0600),<br />окно Chromium закрыто.
                </div>
              </div>
            )}
          </div>
        </div>

        <aside style={S.aside}>
          <section>
            <h2 style={S.h2}>Как войти</h2>
            <ol className="steps" style={S.steps}>
              <li>Кликни в поле <b>Email</b> на экране слева</li>
              <li>Печатай — клавиши идут прямо в изолированный Chromium</li>
              <li>Пройди 2FA (SMS / Authenticator)</li>
              <li>Движок сам увидит куки сессии и закроет окно</li>
              <li>Зашёл не туда (напр. «Восстановление доступа»)? Жми <b>⬅ Назад</b></li>
            </ol>
          </section>
          <section>
            <h2 style={S.h2}>Статус</h2>
            <div className="note" style={S.note}>{detailParts.join(' · ') || '—'}</div>
          </section>
          <section>
            <h2 style={S.h2}>Управление</h2>
            <div style={{ ...S.btnrow, marginBottom: 8 }}>
              <button className="primary" onClick={() => send({ t: 'back' })} title="Вернуться на прошлую страницу (или на страницу входа)">⬅ Назад</button>
              <button onClick={() => send({ t: 'forward' })}>Вперёд ➡</button>
              <button onClick={() => send({ t: 'reload' })}>↻ Обновить</button>
            </div>
            <div style={S.btnrow}>
              <button onClick={() => {
                if (confirm('Открыть заново страницу входа Google? Текущий прогресс страницы будет сброшен.'))
                  send({ t: 'goto', url: 'https://accounts.google.com/' });
              }}>Начать заново</button>
              <button onClick={() => send({ t: 'hello' })}>Кадр</button>
              <button onClick={async () => {
                try {
                  const r = await fetch('/api/companion-status');
                  alert(JSON.stringify(await r.json(), null, 2));
                } catch (e) { alert('нет связи с companion: ' + e.message); }
              }}>Статус JSON</button>
              <button className="danger" onClick={() => {
                if (!confirm('Завершить auth-companion? Окно Chromium закроется.')) return;
                fetch('/api/shutdown', { method: 'POST' }).catch(() => {});
              }}>Завершить</button>
            </div>
          </section>
          <section>
            <h2 style={S.h2}>События</h2>
            <div style={S.eventlog}>
              {events.length ? events.map((e, i) => <div key={i}>{e}</div>) : <div>—</div>}
            </div>
          </section>
          <div className="note" style={S.note}>
            <b>Безопасность:</b> это изолированный Chromium движка (свежий профиль{' '}
            <span style={S.mono}>google-profile</span>). Релей пересылает клавиши без
            логирования; куки читает только auth-companion →{' '}
            <span style={S.mono}>google_session.json</span> (0600).
          </div>
        </aside>
      </div>

      <footer style={S.footer}>
        <span>CDP screencast · вход только между тобой и Google</span>
        <span>таймаут сессии: 30 мин</span>
      </footer>
    </main>
  );
}

/* ── стили ─────────────────────────────────────────────────────────── */
const CSS = `
  * { box-sizing: border-box; }
  .chip { display: inline-flex; align-items: center; gap: 8px; padding: 4px 12px;
    border-radius: 999px; font-size: 12.5px; border: 1px solid #1f2a3a;
    background: #0d1420; color: #8b96a8; }
  .chip .dot { width: 8px; height: 8px; border-radius: 50%; background: #8b96a8; }
  .chip.waiting { color: #fbbf24; border-color: #4a3b12; }
  .chip.waiting .dot { background: #fbbf24; animation: pulse 1.2s infinite; }
  .chip.authorized { color: #34d399; border-color: #14432f; }
  .chip.authorized .dot { background: #34d399; }
  .chip.bad { color: #f87171; border-color: #4a1f1f; }
  .chip.bad .dot { background: #f87171; }
  @keyframes pulse { 50% { opacity: .35; } }
  .spin { width: 28px; height: 28px; border-radius: 50%;
    border: 3px solid #1f2a3a; border-top-color: #5b9cff;
    animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .steps { margin: 0; padding-left: 18px; color: #c3ccdb; display: grid;
    gap: 8px; font-size: 13px; }
  .steps li::marker { color: #5b9cff; }
  .note { font-size: 12px; color: #8b96a8; border-left: 2px solid #1f2a3a; padding-left: 10px; }
  .note b { color: #c3ccdb; }
  button { background: #16233a; color: #e5eaf3; border: 1px solid #1f2a3a;
    border-radius: 8px; padding: 7px 12px; font-size: 12.5px; cursor: pointer; }
  button:hover { border-color: #5b9cff; }
  button.primary { border-color: #2d4a7a; background: #1a2a4a; color: #cfe0ff; }
  button.primary:hover { border-color: #5b9cff; }
  button.danger { color: #f87171; }
  @media (max-width: 860px) { aside { display: none !important; } }
`;

const S = {
  page: {
    margin: 0, minHeight: '100vh', background: '#0b0f17', color: '#e5eaf3',
    fontFamily: "system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif",
    fontSize: 14, lineHeight: 1.5, display: 'flex', flexDirection: 'column',
  },
  header: {
    display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap',
    padding: '12px 18px', borderBottom: '1px solid #1f2a3a',
    background: 'linear-gradient(180deg, #0e1522, #0b0f17)',
  },
  brand: { display: 'flex', alignItems: 'baseline', gap: 10 },
  brandVer: { color: '#8b96a8', fontFamily: 'ui-monospace, Consolas, monospace', fontSize: 12 },
  chip: {},
  spacer: { flex: 1 },
  timer: { fontFamily: 'ui-monospace, Consolas, monospace', color: '#8b96a8', fontSize: 12.5 },
  main: { flex: 1, display: 'flex', minHeight: 0 },
  stage: {
    flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center',
    padding: 16, minWidth: 0, position: 'relative',
  },
  screenWrap: {
    position: 'relative', maxWidth: '100%', maxHeight: '100%', display: 'flex',
    borderRadius: 10, overflow: 'hidden', cursor: 'crosshair',
    boxShadow: '0 12px 40px rgba(0,0,0,.5), 0 0 0 1px #1f2a3a',
  },
  screen: {
    display: 'block', maxWidth: '100%', maxHeight: 'calc(100vh - 150px)',
    userSelect: 'none', WebkitUserDrag: 'none',
  },
  placeholder: {
    width: 640, height: 420, display: 'flex', flexDirection: 'column',
    alignItems: 'center', justifyContent: 'center', gap: 12, color: '#8b96a8',
    fontSize: 13.5, background: '#0d1420',
  },
  successOverlay: {
    position: 'absolute', inset: 0, display: 'flex', flexDirection: 'column',
    alignItems: 'center', justifyContent: 'center', gap: 8, textAlign: 'center',
    background: 'rgba(8,11,18,.86)', color: '#8b96a8', padding: 20,
  },
  successTitle: { color: '#34d399', fontSize: 16, fontWeight: 600 },
  aside: {
    width: 300, borderLeft: '1px solid #1f2a3a', padding: 16, display: 'flex',
    flexDirection: 'column', gap: 16, overflowY: 'auto', background: '#0d1420',
  },
  h2: {
    fontSize: 11.5, textTransform: 'uppercase', letterSpacing: '.8px',
    color: '#8b96a8', margin: '0 0 8px', fontWeight: 600,
  },
  btnrow: { display: 'flex', gap: 8, flexWrap: 'wrap' },
  eventlog: {
    fontFamily: 'ui-monospace, Consolas, monospace', fontSize: 11,
    color: '#8b96a8', display: 'grid', gap: 4,
  },
  footer: {
    padding: '8px 18px', borderTop: '1px solid #1f2a3a', color: '#8b96a8',
    fontSize: 11.5, display: 'flex', gap: 16, flexWrap: 'wrap',
  },
  mono: { fontFamily: 'ui-monospace, Consolas, monospace' },
};
