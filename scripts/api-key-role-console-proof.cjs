#!/usr/bin/env node
/**
 * api-key-role-console-proof.cjs — kanban t_5c021107
 *
 * The guide promises a read-only key; this proves the CONSOLE can issue one, shows its role, and
 * that Regenerate keeps it — in real Chromium on the served shell, against the running app.
 *
 * Legs
 *   U1  login through the shell's own inline form (not an injected token)
 *   U2  Integration Center renders a Role picker and "+ Create API key" (no "lead-delivery" copy)
 *   U3  create a read-only key through the picker: the request body carries role=read_only, the
 *       once-shown card says Read-only, the list row says read-only
 *   U4  that key over raw HTTP: GET /api/objects 200; POST /api/objects 403 + boundary sentence;
 *       MCP read tool answers; MCP write tool refused
 *   U5  Regenerate (the console's own button) keeps the role; the modal says Read-only, the secret
 *       changes, the old secret is 401, the new one still reads 200 and refuses writes 403
 *   U6  the same control creates a WORKSPACE key (modal says Read and write) and it writes 200
 *   U7  Revoke (console button) leaves the rotated read-only secret 401
 *   U8  the served guide matches what the console offers
 *   Z   probe account + every row removed; baseline counts restored
 *
 * Run: NODE_PATH=/opt/swift/browser-run/node_modules node scripts/api-key-role-console-proof.cjs
 * Chromium: /usr/bin/chromium (override with CHROME_PATH).
 */
'use strict';

const http = require('http');
const { execFileSync } = require('child_process');
const puppeteer = require('puppeteer');

const HOST = 'app.coreswiftcrm.com';
const APP = 'coreswiftcrm';
const DB_CONTAINER = 'swift-postgres';
const STAMP = String(Date.now()).slice(-7);
const PROBE_EMAIL = `probe-keyrole-${STAMP}@local.coreswift`;
const PROBE_PASS = `ProbeKeyRole!${STAMP}`;
const RO_NAME = `Probe read-only ${STAMP}`;
const RW_NAME = `Probe read-write ${STAMP}`;
const SENTENCE = 'this key is read-only';
const SENTENCE_TAIL = 'use a workspace key to write';

const PASS = [];
const FAIL = [];
function check(name, ok, detail) {
  (ok ? PASS : FAIL).push(name);
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  [${detail}]` : ''}`);
}

// ── raw HTTP to the vhost, Host-routed exactly as nginx sees it ───────────────────────────────
function api(method, path, body, token) {
  return new Promise((resolve, reject) => {
    const data = body === undefined ? null : Buffer.from(JSON.stringify(body));
    const headers = { Host: HOST, Accept: 'application/json' };
    if (data) { headers['Content-Type'] = 'application/json'; headers['Content-Length'] = data.length; }
    if (token) headers.Authorization = 'Bearer ' + token;
    const req = http.request({ host: '127.0.0.1', port: 80, path, method, headers }, res => {
      const chunks = [];
      res.on('data', c => chunks.push(c));
      res.on('end', () => {
        const raw = Buffer.concat(chunks).toString('utf8');
        let json = null;
        try { json = raw ? JSON.parse(raw) : null; } catch (e) { json = { raw: raw.slice(0, 400) }; }
        resolve({ status: res.statusCode, json, raw });
      });
    });
    req.on('error', reject);
    if (data) req.write(data);
    req.end();
  });
}

// ── psql through the postgres container (no host credential) ─────────────────────────────────
let DB = null;
function dbInit() {
  const url = execFileSync('docker', ['exec', APP, 'printenv', 'DATABASE_URL'], { encoding: 'utf8' }).trim();
  const m = /^postgres:\/\/([^:]+):([^@]*)@[^/]+\/(\w+)/.exec(url);
  if (!m) throw new Error('cannot parse DATABASE_URL: ' + url.replace(/:[^:@]*@/, ':***@'));
  DB = { user: m[1], pass: m[2], name: m[3] };
}
function sql(q) {
  return execFileSync('docker', ['exec', '-e', `PGPASSWORD=${DB.pass}`, DB_CONTAINER,
    'psql', '-U', DB.user, '-d', DB.name, '-At', '-F', '|', '-v', 'ON_ERROR_STOP=1', '-c', q],
    { encoding: 'utf8' }).trim();
}

const sleep = ms => new Promise(r => setTimeout(r, ms));

async function main() {
  dbInit();
  const baseTenants = Number(sql('SELECT count(*) FROM tenants'));
  const baseKeys = Number(sql('SELECT count(*) FROM personal_api_keys'));

  // ── probe account through the public signup door ──────────────────────────────────────────
  const reg = await api('POST', '/api/auth/register', {
    name: 'Key Role Probe', email: PROBE_EMAIL, password: PROBE_PASS,
    account_name: `Key Role Probe ${STAMP}`, account_slug: `keyrole-${STAMP}`,
  });
  if (reg.status !== 200 && reg.status !== 201) {
    throw new Error(`probe signup failed: ${reg.status} ${JSON.stringify(reg.json).slice(0, 300)}`);
  }
  const tenantId = reg.json.account.id;
  const signupKey = reg.json.api_key.key;
  check('S1 a throwaway probe account exists', !!tenantId, `tenant=${tenantId}`);

  // A real object the MCP read leg can answer about (the read-only key cannot create one, by
  // design, so the fixture is written with the account's own workspace key).
  const fixtureSlug = `mcp_fixture_${STAMP}`;
  const fx = await api('POST', '/api/objects', { slug: fixtureSlug, name: `MCP fixture ${STAMP}` }, signupKey);
  check('S2 a workspace fixture object exists for the MCP read leg', fx.status === 200,
    `status=${fx.status} body=${JSON.stringify(fx.json).slice(0, 120)}`);

  const dialogs = [];
  const pageErrors = [];
  const browser = await puppeteer.launch({
    headless: 'new',
    executablePath: process.env.CHROME_PATH || '/usr/bin/chromium',
    args: ['--no-sandbox', '--disable-dev-shm-usage',
      `--host-resolver-rules=MAP ${HOST} 127.0.0.1, MAP coreswiftcrm.com 127.0.0.1`,
      '--disable-features=HttpsUpgrades,HttpsFirstMode,HttpsFirstModeV2,HttpsOnlyMode,AutofillServerCommunication',
      '--user-data-dir=/tmp/keyrole-' + STAMP],
  });

  try {
    const page = await browser.newPage();
    await page.setCacheEnabled(false);
    page.on('pageerror', e => pageErrors.push(String(e)));
    page.on('dialog', async d => { dialogs.push(d.message()); await d.accept(); });

    // ── U1 login through the shell's own form ────────────────────────────────────────────
    await page.goto(`http://${HOST}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
    await page.waitForSelector('#loginEmail', { timeout: 20000 });
    await page.type('#loginEmail', PROBE_EMAIL);
    await page.type('#loginPassword', PROBE_PASS);
    await page.evaluate(() => {
      const b = [...document.querySelectorAll('button')].find(x => /Sign In/i.test(x.textContent));
      (b || document.querySelector('button.btn-primary.btn-block')).click();
    });
    await page.waitForFunction(() => typeof STATE !== 'undefined' && STATE.user && !!STATE.user.email, { timeout: 25000 });
    check('U1 signed in through the console\'s own form', true,
      await page.evaluate(() => STATE.user.email));

    // ── U2 the Integration Center card ───────────────────────────────────────────────────
    await page.evaluate(() => { try { switchTab('integrations'); } catch (e) {} });
    await page.waitForFunction(() => !!document.getElementById('icNewKeyRole'), { timeout: 25000 });
    const u2 = await page.evaluate(() => {
      const sel = document.getElementById('icNewKeyRole');
      const opts = sel ? [...sel.options].map(o => ({ v: o.value, t: o.textContent.trim() })) : [];
      const card = document.getElementById('icKeysWrap');
      const text = card ? card.innerText : '';
      const btn = [...document.querySelectorAll('#icKeysWrap button')].map(b => b.textContent.trim());
      return { opts, text: text.slice(0, 400), btn, selDefault: sel ? sel.value : null };
    });
    check('U2a Role picker offers workspace (default) and read-only',
      u2.opts.length === 2 && u2.selDefault === 'workspace' &&
      u2.opts.some(o => o.v === 'read_only' && /Read-only/.test(o.t)) &&
      u2.opts.some(o => o.v === 'workspace' && /Read and write/.test(o.t)),
      JSON.stringify(u2.opts));
    check('U2b the create button is "+ Create API key" (no "lead-delivery" wording)',
      u2.btn.some(b => /\+ Create API key/.test(b)) && !/lead-delivery/i.test(u2.text),
      JSON.stringify(u2.btn));

    // ── U3 create a READ-ONLY key through the picker ─────────────────────────────────────
    const reqs = [];
    page.on('request', r => { if (r.url().includes('/api/personal-api-keys')) reqs.push({ m: r.method(), u: r.url(), b: r.postData() || '' }); });
    await page.evaluate(name => {
      const sel = document.getElementById('icNewKeyRole');
      sel.value = 'read_only';
      sel.dispatchEvent(new Event('change', { bubbles: true }));
      document.getElementById('icNewKeyName').value = name;
    }, RO_NAME);
    await page.evaluate(() => {
      const b = [...document.querySelectorAll('#icKeysWrap button')].find(x => /\+ Create API key/.test(x.textContent));
      b.click();
    });
    await page.waitForFunction(() => {
      const i = document.getElementById('icNewKey');
      return !!i && /^csk_/.test(i.value || '');
    }, { timeout: 20000 });
    const roSecret = await page.evaluate(() => document.getElementById('icNewKey').value);
    const roCard = await page.evaluate(() => (document.getElementById('icNewKeyRoleLine') || {}).innerText || '');
    check('U3a the console SENT role=read_only in the create body',
      reqs.some(r => r.m === 'POST' && /"role"\s*:\s*"read_only"/.test(r.b) && r.b.includes(RO_NAME)),
      reqs.filter(r => r.m === 'POST').map(r => r.b).join(' | ').slice(0, 220));
    check('U3b the once-shown card says the key is Read-only', /Read-only/.test(roCard), roCard.slice(0, 160));
    await page.evaluate(() => { const b = [...document.querySelectorAll('.modal button, button')].find(x => /^Close$/.test(x.textContent.trim())); if (b) b.click(); });
    await page.waitForFunction(n => [...document.querySelectorAll('#icKeysWrap tbody tr')].some(r => r.innerText.includes(n)), { timeout: 20000 }, RO_NAME);
    const roRow = await page.evaluate(n => {
      const r = [...document.querySelectorAll('#icKeysWrap tbody tr')].find(x => x.innerText.includes(n));
      return r ? [...r.querySelectorAll('td')].map(c => c.innerText.trim()) : null;
    }, RO_NAME);
    check('U3c the list row shows the read-only role', !!roRow && roRow.some(c => /^read-only$/i.test(c)),
      JSON.stringify(roRow));

    // ── U4 the read-only secret, over raw HTTP and MCP ───────────────────────────────────
    const token = await page.evaluate(() => localStorage.getItem('crm_token'));
    const list = await api('GET', '/api/personal-api-keys', undefined, token);
    const roKey = (list.json.keys || []).find(k => k.name === RO_NAME);
    check('U4a GET /api/personal-api-keys returns role=read_only for the new key',
      list.status === 200 && roKey && roKey.role === 'read_only', JSON.stringify(roKey && { role: roKey.role, active: roKey.is_active }));
    const ov = await api('GET', '/api/integration-center/overview', undefined, token);
    const ovKey = (((ov.json || {}).personal_keys || {}).items || []).find(k => k.name === RO_NAME);
    check('U4b the Integration Center overview carries role=read_only (what the list renders)',
      ov.status === 200 && ovKey && ovKey.role === 'read_only', JSON.stringify(ovKey && { role: ovKey.role }));

    let st = await api('GET', '/api/objects', undefined, roSecret);
    check('U4c GET /api/objects answers 200 for the read-only key', st.status === 200, `status=${st.status}`);
    st = await api('POST', '/api/objects', { slug: `ro_probe_${STAMP}`, name: `RO probe ${STAMP}` }, roSecret);
    const text = JSON.stringify(st.json);
    check('U4d POST /api/objects answers 403 with the boundary sentence',
      st.status === 403 && text.includes(SENTENCE) && text.includes(SENTENCE_TAIL),
      `status=${st.status} body=${text.slice(0, 180)}`);

    async function rpc(secret, name, args) {
      const r = await api('POST', '/api/mcp', {
        jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args },
      }, secret);
      const result = (r.json || {}).result || {};
      let payload = result.structuredContent;
      if (!payload) {
        const block = (result.content || []).find(b => b.type === 'text') || {};
        try { payload = JSON.parse(block.text || '{}'); } catch (e) { payload = { text: block.text }; }
      }
      return { http: r.status, isError: !!result.isError, payload };
    }
    let mcp = await rpc(roSecret, 'query_records', { object: fixtureSlug, limit: 5 });
    check('U4e the MCP read tool answers for the read-only key', mcp.http === 200 && !mcp.isError,
      JSON.stringify(mcp.payload).slice(0, 160));
    mcp = await rpc(roSecret, 'create_object', { slug: `mcp_hacked_${STAMP}`, name: 'mcp hacked' });
    check('U4f the MCP write tool is refused for the read-only key',
      mcp.isError && mcp.payload && mcp.payload.error === 'forbidden',
      JSON.stringify(mcp.payload).slice(0, 160));

    // ── U5 Regenerate through the console's own button keeps the role ───────────────────
    const clickRow = async (name, label) => page.evaluate((n, l) => {
      const rows = [...document.querySelectorAll('#icKeysWrap tbody tr')];
      const row = rows.find(r => r.innerText.includes(n));
      if (!row) return 'no-row';
      const b = [...row.querySelectorAll('button')].find(x => x.textContent.trim() === l);
      if (!b) return 'no-button:' + row.innerText;
      b.click();
      return 'clicked';
    }, name, label);
    const rot = await clickRow(RO_NAME, 'Regenerate');
    await page.waitForFunction(() => {
      const i = document.getElementById('icNewKey');
      return !!i && /^csk_/.test(i.value || '');
    }, { timeout: 20000 });
    const roSecret2 = await page.evaluate(() => document.getElementById('icNewKey').value);
    const rotCard = await page.evaluate(() => (document.getElementById('icNewKeyRoleLine') || {}).innerText || '');
    check('U5a Regenerate was driven from the console row', rot === 'clicked', rot);
    check('U5b the confirm dialog promised the role is kept',
      dialogs.some(m => /keeps its role/i.test(m)), (dialogs[dialogs.length - 1] || '').slice(0, 160));
    check('U5c the regenerated card still says Read-only', /Read-only/.test(rotCard), rotCard.slice(0, 160));
    check('U5d regenerating produced a NEW secret', !!roSecret2 && roSecret2 !== roSecret,
      `${String(roSecret).slice(0, 8)}… -> ${String(roSecret2).slice(0, 8)}…`);
    await page.evaluate(() => { const b = [...document.querySelectorAll('button')].find(x => /^Close$/.test(x.textContent.trim())); if (b) b.click(); });
    st = await api('GET', '/api/personal-api-keys', undefined, token);
    const roKey2 = (st.json.keys || []).find(k => k.name === RO_NAME && k.is_active);
    check('U5e the rotated key is stored as read_only', st.status === 200 && roKey2 && roKey2.role === 'read_only',
      JSON.stringify(roKey2 && { role: roKey2.role, prefix: roKey2.prefix }));
    st = await api('GET', '/api/objects', undefined, roSecret);
    check('U5f the OLD read-only secret is dead (401)', st.status === 401, `status=${st.status}`);
    st = await api('GET', '/api/objects', undefined, roSecret2);
    check('U5g the NEW read-only secret reads (200)', st.status === 200, `status=${st.status}`);
    st = await api('POST', '/api/objects', { slug: `ro_probe2_${STAMP}`, name: 'x' }, roSecret2);
    check('U5h the NEW read-only secret still cannot write (403 + sentence)',
      st.status === 403 && JSON.stringify(st.json).includes(SENTENCE), `status=${st.status}`);

    // ── U6 the same control creates a workspace key that writes ─────────────────────────
    await page.waitForFunction(n => [...document.querySelectorAll('#icKeysWrap tbody tr')].some(r => r.innerText.includes(n)), { timeout: 20000 }, RO_NAME);
    await page.evaluate(name => {
      const sel = document.getElementById('icNewKeyRole');
      sel.value = 'workspace';
      sel.dispatchEvent(new Event('change', { bubbles: true }));
      document.getElementById('icNewKeyName').value = name;
      const b = [...document.querySelectorAll('#icKeysWrap button')].find(x => /\+ Create API key/.test(x.textContent));
      b.click();
    }, RW_NAME);
    await page.waitForFunction(() => {
      const i = document.getElementById('icNewKey');
      return !!i && /^csk_/.test(i.value || '');
    }, { timeout: 20000 });
    const rwSecret = await page.evaluate(() => document.getElementById('icNewKey').value);
    const rwCard = await page.evaluate(() => (document.getElementById('icNewKeyRoleLine') || {}).innerText || '');
    check('U6a the workspace card says Read and write', /Read and write/.test(rwCard), rwCard.slice(0, 160));
    st = await api('POST', '/api/objects', { slug: `rw_ok_${STAMP}`, name: `RW ${STAMP}` }, rwSecret);
    check('U6b a workspace key created through the same control writes 200', st.status === 200, `status=${st.status}`);
    await page.evaluate(() => { const b = [...document.querySelectorAll('button')].find(x => /^Close$/.test(x.textContent.trim())); if (b) b.click(); });
    await page.waitForFunction(n => [...document.querySelectorAll('#icKeysWrap tbody tr')].some(r => r.innerText.includes(n)), { timeout: 20000 }, RW_NAME);
    const rwRow = await page.evaluate(n => {
      const r = [...document.querySelectorAll('#icKeysWrap tbody tr')].find(x => x.innerText.includes(n));
      return r ? [...r.querySelectorAll('td')].map(c => c.innerText.trim()) : null;
    }, RW_NAME);
    check('U6c the workspace row shows read + write', !!rwRow && rwRow.some(c => /read \+ write/i.test(c)),
      JSON.stringify(rwRow));
    const shotA = process.env.PROOF_SHOT;
    if (shotA) {
      await page.evaluate(() => {
        const el = document.getElementById('icKeysWrap');
        if (el && el.scrollIntoView) el.scrollIntoView({ block: 'center' });
      });
      await sleep(400);
      await page.screenshot({ path: shotA, fullPage: false });
    }

    // ── U6d the MCP page's own key table shows the role too ────────────────────────────
    await page.evaluate(() => { try { switchTab('mcp'); } catch (e) {} });
    await page.waitForFunction(() => {
      const w = document.getElementById('mcpKeyWrap');
      return !!w && !!w.querySelector('table tbody tr');
    }, { timeout: 25000 });
    const mcpRows = await page.evaluate(() => [...document.querySelectorAll('#mcpKeyWrap tbody tr')]
      .map(r => [...r.querySelectorAll('td')].map(c => c.innerText.trim())));
    const mcpRo = mcpRows.find(r => r[0] && r[0].includes(RO_NAME));
    const mcpRw = mcpRows.find(r => r[0] && r[0].includes(RW_NAME));
    check('U6d the MCP page lists each key with its role',
      !!mcpRo && /read-only/i.test(mcpRo[2]) && !!mcpRw && /read \+ write/i.test(mcpRw[2]),
      JSON.stringify(mcpRows));
    await page.evaluate(() => { try { switchTab('integrations'); } catch (e) {} });
    await page.waitForFunction(() => !!document.getElementById('icNewKeyRole'), { timeout: 25000 });

    // ── U7 Revoke from the console row leaves it 401 ────────────────────────────────────
    const rev = await clickRow(RO_NAME, 'Revoke');
    await sleep(1500);
    st = await api('GET', '/api/objects', undefined, roSecret2);
    check('U7a Revoke was driven from the console row', rev === 'clicked', rev);
    check('U7b the revoked read-only secret answers 401', st.status === 401, `status=${st.status}`);

    // ── U8 the served guide matches what the console offers ─────────────────────────────
    const guide = await api('GET', '/guide.html');
    const gtext = guide.raw || '';
    check('U8a the served guide names the Role choice on + Create API key',
      guide.status === 200 && /Choosing a key&#39;s role|Choosing a key's role/.test(gtext) &&
      /\+ Create API key/.test(gtext), `status=${guide.status}`);
    check('U8b the guide no longer says every console key is a workspace key',
      !/Every key the console creates is/.test(gtext));
    check('U8c the guide says Regenerate keeps the role', /Regenerate<\/strong> keeps the role/.test(gtext));

    // ── Z no residue ────────────────────────────────────────────────────────────────────
    check('Z0 no uncaught page error', pageErrors.length === 0, pageErrors.slice(0, 2).join(' | '));
  } finally {
    await browser.close();
  }

  // cleanup: the probe account and every row it owned
  sql(`DELETE FROM users WHERE tenant_id = '${tenantId}'; DELETE FROM tenants WHERE id = '${tenantId}';`);
  const leftTenants = Number(sql(`SELECT count(*) FROM tenants WHERE id = '${tenantId}'`));
  const leftUsers = Number(sql(`SELECT count(*) FROM users WHERE email = '${PROBE_EMAIL}'`));
  const leftKeys = Number(sql(`SELECT count(*) FROM personal_api_keys WHERE tenant_id = '${tenantId}'`));
  const nowTenants = Number(sql('SELECT count(*) FROM tenants'));
  const nowKeys = Number(sql('SELECT count(*) FROM personal_api_keys'));
  check('Z1 the probe account left no tenant, user or key row',
    leftTenants === 0 && leftUsers === 0 && leftKeys === 0,
    `tenant=${leftTenants} user=${leftUsers} keys=${leftKeys}`);
  check('Z2 fleet counters are back to baseline', nowTenants === baseTenants && nowKeys === baseKeys,
    `tenants ${baseTenants}->${nowTenants}, keys ${baseKeys}->${nowKeys}`);
  const leftovers = Number(sql(`SELECT count(*) FROM custom_objects WHERE slug IN ('mcp_fixture_${STAMP}','ro_probe_${STAMP}','ro_probe2_${STAMP}','rw_ok_${STAMP}','mcp_hacked_${STAMP}')`));
  check('Z3 no probe custom object survived', leftovers === 0, `objects=${leftovers}`);

  console.log(`\n${PASS.length} PASS / ${FAIL.length} FAIL`);
  if (FAIL.length) { console.log('FAILED: ' + FAIL.join('; ')); process.exit(1); }
}

main().catch(e => { console.error('HARNESS ERROR:', e && e.stack || e); process.exit(2); });
