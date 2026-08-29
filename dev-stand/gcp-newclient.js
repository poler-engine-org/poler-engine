#!/usr/bin/env node
'use strict';
/*!
 * gcp-newclient.js — создать OAuth-клиент «poler-engine-desktop» (Desktop app)
 * в Google Auth Platform и поймать client_secret в момент создания.
 * Диалог создания показывается ОДИН раз — ловим через deep-DOM поллинг.
 */
const fs = require('fs');
const path = require('path');
const os = require('os');

const CFG = path.join(os.homedir(), '.config', 'poler-engine');
const SECRET_FILE = path.join(CFG, 'client_secret.json');
const URL_CREATE = 'https://console.cloud.google.com/auth/clients/create?project=verification-506705';
const CLIENT_NAME = process.env.CLIENT_NAME || 'poler-engine-desktop';

const M = require('/home/z/my-project/scripts/gcp-cdp-machinery.js');
const log = M.log;
const sleep = M.sleep;

const DEEPQ = `function deepAll(root, sel) {
  let out = [...root.querySelectorAll(sel)];
  for (const el of [...root.querySelectorAll('*')]) { if (el.shadowRoot) out = out.concat(deepAll(el.shadowRoot, sel)); }
  return out;
}`;

(async () => {
  log('запуск Chromium (профиль движка)…');
  const { child, cdpPort } = await M.launchChromium();
  log('Chromium CDP :' + cdpPort);
  const ref = { cdp: null };
  try {
    ref.cdp = await M.connectPageRetry(cdpPort, 4);
    log('навигация: форма создания клиента');
    try { await ref.cdp.call('Page.navigate', { url: URL_CREATE }, 10000); } catch (_) {}
    try { ref.cdp.ws.close(); } catch (_) {}
    await sleep(6000);
    ref.cdp = await M.connectPageRetry(cdpPort, 4);

    // ждём форму (Application type)
    let formReady = false;
    for (let i = 0; i < 15 && !formReady; i++) {
      const v = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ}; const s = deepAll(document, 'cfc-select'); return JSON.stringify({n: s.length, txt: s.map(x => (x.innerText||'').trim().slice(0,30)), failed: /Failed to load/.test(document.body.innerText)}); })()`, 15000);
      try {
        const j = JSON.parse(v);
        if (j.n > 0) { formReady = true; log('✓ форма готова: cfc-select x' + j.n + ' ' + JSON.stringify(j.txt)); break; }
        if (j.failed && i < 10) { log('⚠ Failed to load — reload'); try { await ref.cdp.call('Page.reload', {}, 8000); } catch (_) {} await sleep(9000); continue; }
      } catch (_) {}
      await sleep(3000);
    }
    if (!formReady) throw new Error('форма создания не загрузилась');

    // 1) открыть dropdown типа
    log('кликаю select «Application type»…');
    await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ}; const s = deepAll(document, 'cfc-select')[0]; if (!s) return 'NOSEL'; const trig = s.shadowRoot ? [...s.shadowRoot.querySelectorAll('*')].filter(e => /div|button/i.test(e.tagName)).sort((a,b)=>b.getBoundingClientRect().width-a.getBoundingClientRect().width)[0] : s; (trig || s).click(); return 'OPENED'; })()`, 15000);
    await sleep(2500);

    // 2) выбрать «Desktop app»
    const picked = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ};
      // опции могут быть в cdk-overlay, в shadow DOM select'а или в body
      const cands = deepAll(document, 'mat-option, cfc-option, li[role=option], [role=option], cfc-listbox-option');
      const txts = cands.map(o => (o.innerText || '').trim());
      const opt = cands.find(o => /desktop app|приложение для десктопа|настольн/i.test((o.innerText || '') + ' ' + JSON.stringify(o.getAttributeNames && o.textContent || '')));
      if (opt) { opt.click(); return 'PICKED:' + (opt.innerText || '').trim().slice(0, 40); }
      return 'NOOPT|' + txts.join('; ').slice(0, 300);
    })()`, 15000);
    log('выбор типа: ' + picked.slice(0, 350));
    if (!picked.startsWith('PICKED')) {
      // фоллбэк: открыть список по-другому — клик по тексту опции в shadowRoot select'а
      const fb = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ};
        const s = deepAll(document, 'cfc-select')[0];
        if (s && s.shadowRoot) {
          const items = [...s.shadowRoot.querySelectorAll('*')].filter(e => e.childElementCount === 0 && /desktop/i.test(e.textContent || ''));
          if (items.length) { items[0].click(); return 'PICKED-SHADOW'; }
        }
        return 'NOFB';
      })()`, 15000);
      log('фоллбэк выбора: ' + fb);
    }
    await sleep(3000);

    // 3) заполнить имя
    const named = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ};
      const inputs = deepAll(document, 'input, textarea').filter(i => !/search/i.test(i.placeholder || '') && i.type !== 'hidden');
      const inp = inputs.find(i => !i.value) || inputs[0];
      if (!inp) return 'NOINPUT|' + inputs.length;
      const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
      setter.call(inp, '${CLIENT_NAME}');
      inp.dispatchEvent(new Event('input', { bubbles: true }));
      inp.dispatchEvent(new Event('change', { bubbles: true }));
      return 'NAMED:' + inputs.length + ':' + (inp.value || '').slice(0, 30);
    })()`, 15000);
    log('имя: ' + named.slice(0, 200));
    await sleep(2000);

    // 4) кнопка Create
    const created = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ};
      const btns = deepAll(document, 'button, [role=button], cfc-button');
      const b = btns.find(x => /^(create|создать)$/i.test((x.innerText || x.textContent || '').trim()));
      if (!b) return 'NOBTN|' + btns.map(x => (x.innerText || '').trim().slice(0, 18)).filter(Boolean).join(';').slice(0, 300);
      b.click(); return 'CREATE-CLICKED';
    })()`, 15000);
    log('create: ' + created.slice(0, 350));

    // 5) ловим диалог с client_id и secret
    log('жду диалог с секретом (deep-DOM поллинг)…');
    let clientId = '';
    let secret = '';
    for (let i = 0; i < 40 && !secret; i++) {
      await sleep(1500);
      const v = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ};
        let t = '';
        function walk(node) { if (!node) return; const tag = (node.tagName || '').toUpperCase(); if (tag === 'STYLE' || tag === 'SCRIPT' || tag === 'NOSCRIPT' || tag === 'LINK') return; if (node.shadowRoot) walk(node.shadowRoot); for (const k of (node.childNodes || [])) { if (k.nodeType === 3) { const v2 = (k.textContent || '').trim(); if (v2) t += v2 + ' '; } else if (k.nodeType === 1) walk(k); } }
        walk(document.body);
        const sec = (t.match(/GOCSPX-[A-Za-z0-9_-]{20,}/) || [''])[0];
        const cid = (t.match(/\\d+-[a-z0-9]+\\.apps\\.googleusercontent\\.com/) || [''])[0];
        return JSON.stringify({sec, cid});
      })()`, 20000);
      try {
        const j = JSON.parse(v);
        if (j.sec) { secret = j.sec; clientId = j.cid || clientId; break; }
        if (j.cid && !clientId) { clientId = j.cid; log('… client_id виден, ждём секрет'); }
      } catch (_) {}
      if (i % 8 === 7) log('… поллинг ' + ((i + 1) * 1.5 | 0) + 'с');
    }

    if (secret) {
      log('🎉 СЕКРЕТ ПОЙМАН: GOCSPX-…' + secret.slice(-4) + ' | client_id: ' + clientId);
      const secretJson = {
        installed: {
          client_id: clientId, client_secret: secret,
          auth_uri: 'https://accounts.google.com/o/oauth2/v2/auth',
          token_uri: 'https://oauth2.googleapis.com/token',
          auth_provider_x509_cert_url: 'https://www.googleapis.com/oauth2/v1/certs',
          redirect_uris: ['http://localhost', 'http://127.0.0.1'],
        },
      };
      fs.writeFileSync(SECRET_FILE, JSON.stringify(secretJson, null, 2), { mode: 0o600 });
      fs.chmodSync(SECRET_FILE, 0o600);
      log('✓ client_secret.json записан (0600): ' + SECRET_FILE);
    } else {
      log('⚠ секрет не пойман — дамп:');
      const dump = await M.evalRetry(ref, cdpPort, `(() => { ${DEEPQ}; let t=''; function walk(node){if(!node)return;const tag=(node.tagName||'').toUpperCase();if(tag==='STYLE'||tag==='SCRIPT'||tag==='NOSCRIPT'||tag==='LINK')return;if(node.shadowRoot)walk(node.shadowRoot);for(const k of(node.childNodes||[])){if(k.nodeType===3){const v2=(k.textContent||'').trim();if(v2)t+=v2+' | ';}else if(k.nodeType===1)walk(k);}} walk(document.body); return t.slice(0, 2000); })()`, 20000);
      console.log('\n===== DEEP DUMP =====\n' + dump + '\n===== /DUMP =====\n');
    }
  } finally {
    try { child.kill('SIGTERM'); } catch (_) {}
    setTimeout(() => { try { child.kill('SIGKILL'); } catch (_) {} }, 3000);
  }
})().catch((e) => { log('ОШИБКА: ' + e.message); process.exit(1); });
