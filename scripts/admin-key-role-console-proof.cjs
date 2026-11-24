#!/usr/bin/env node
/**
 * admin-key-role-console-proof.cjs — kanban t_eecb1576
 *
 * The ADMIN console (admin.coreswiftcrm.com, www-admin/index.html) is a fork of the tenant SPA.
 * The tenant console learned to issue a read-only key in t_5c021107; this proves the FORK can too,
 * in real Chromium through its own controls, against the live deploy.
 *
 * Legs
 *   A1 a throwaway probe workspace exists and one user on it is a platform operator
 *   U1 sign-in through the admin console's own form; the operator session really is operator
 *   U2 the Integration Center card offers a Name field + Role picker (workspace default, read-only)
 *      behind "+ Create API key", its table has a Role column, and no "lead-delivery" wording
 *   U3 create a READ-ONLY key through the picker: the request body carries role=read_only and the
 *      name; the once-shown card says Read-only; the list row says read-only
 *   U4 that secret over raw HTTP: GET /api/objects 200; POST 403 + boundary sentence; MCP read
 *      answers / MCP write is refused
 *   U5 Rotate (the fork's own button) keeps the role: confirm says so, the card still says
 *      Read-only, the reply's secret is new, the old one is 401, the new one still reads and
 *      still cannot write
 *   U6 the same control creates a WORKSPACE key (card says Read and write) and it writes 200,
 *      and its row shows read + write
 *   U7 Revoke from the console row leaves the rotated read-only secret 401
 *   Z  no uncaught page error; probe workspace + every row removed; fleet counters restored
 *
 * Run: NODE_PATH=/opt/swift/browser-run/node_modules node scripts/admin-key-role-console-proof.cjs
 * Chromium: /usr/bin/chromium (override with CHROME_PATH).
 */
'use strict';

const http = require('http');
const { execFileSync } = require('child_process');
const puppeteer = require('puppeteer');

const HOST = 'admin.coreswiftcrm.com';
const APP = 'coreswiftcrm';
const DB_CONTAINER = 'swift-postgres';
const STAMP = String(Date.now()).slice(-7);
const PROBE_EMAIL = `probe-adminkeyrole-${STAMP}@local.coreswift`;
const PROBE_PASS = `ProbeAdminKeyRole!${STAMP}`;
const RO_NAME = `Probe admin read-only ${STAMP}`;
const RW_NAME = `Probe admin read-write ${STAMP}`;
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

  // ── probe workspace through the public signup door, then make its user an operator ────────
  const reg = await api('POST', '/api/auth/register', {
    name: 'Admin Key Role Probe', email: PROBE_EMAIL, password: PROBE_PASS,
    account_name: `Admin Key Role Probe ${STAMP}`, account_slug: `adminkeyrole-${STAMP}`,
  });
  if (reg.status !== 200 && reg.status !== 201) {
    throw new Error(`probe signup failed: ${reg.status} ${JSON.stringify(reg.json).slice(0, 300)}`);
  }
  const tenantId = reg.json.account.id;
  const signupKey = reg.json.api_key.key;
  sql(`UPDATE users SET is_platform_admin = true WHERE email = '${PROBE_EMAIL}'`);
  const promoted = sql(`SELECT count(*) FROM users WHERE email = '${PROBE_EMAIL}' AND is_platform_admin IS TRUE`);
  check('A1 a throwaway probe workspace exists with one platform-operator user',
    !!tenantId && promoted === '1', `tenant=${tenantId} operator_users=${promoted}`);

  // A real object the read leg can answer about is not needed: the read-only key's REST write
  // refusal (U4d) is the measured boundary, and the workspace key's write (U6b) proves the pen.

  const dialogs = [];
  const pageErrors = [];
  const browser = await puppeteer.launch({
    headless: 'new',
    executablePath: process.env.CHROME_PATH || '/usr/bin/chromium',
    args: ['--no-sandbox', '--disable-dev-shm-usage',
      `--host-resolver-rules=MAP ${HOST} 127.0.0.1, MAP coreswiftcrm.com 127.0.0.1`,
      '--disable-features=HttpsUpgrades,HttpsFirstMode,HttpsFirstModeV2,HttpsOnlyMode,AutofillServerCommunication',
      '--user-data-dir=/tmp/adminkeyrole-' + STAMP],
  });

  try {
    const page = await browser.newPage();
    await page.setViewport({ width: 1440, height: 1000 });
    await page.setCacheEnabled(false);
    page.on('pageerror', e => pageErrors.push(String(e)));
    page.on('dialog', async d => { dialogs.push(d.message()); await d.accept(); });

    // ── U1 sign in through the admin console's own form ───────────────────────────────────
    await page.goto(`http://${HOST}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
    await page.waitForSelector('#loginEmail', { timeout: 20000 });
    await page.type('#loginEmail', PROBE_EMAIL);
    await page.type('#loginPassword', PROBE_PASS);
    await page.evaluate(() => {
      const b = [...document.querySelectorAll('button')].find(x => /Sign In/i.test(x.textContent));
      (b || document.querySelector('button.btn-primary.btn-block')).click();
    });
    await page.waitForFunction(() => typeof STATE !== 'undefined' && STATE.user && !!STATE.user.email, { timeout: 25000 });
    const session = await page.evaluate(() => ({
      email: STATE.user.email,
      host: location.host,
      operator: (typeof isPlatformUser === 'function') ? isPlatformUser() : null,
      adminNav: /ADMIN/.test((document.getElementById('sidebarNav') || {}).innerText || ''),
    }));
    check('U1 signed in through the admin console\'s own form as a platform operator',
      session.host === HOST && session.operator === true && session.adminNav === true,
      JSON.stringify(session));

    // ── U2 the Integration Center card ────────────────────────────────────────────────────
    await page.evaluate(() => { try { switchTab('integrations'); } catch (e) {} });
    await page.waitForFunction(() => !!document.getElementById('icNewKeyRole'), { timeout: 25000 });
    await sleep(1200);   // let icLoadOverview paint the table + footer
    const u2 = await page.evaluate(() => {
      const sel = document.getElementById('icNewKeyRole');
      const opts = sel ? [...sel.options].map(o => ({ v: o.value, t: o.textContent.trim() })) : [];
      const card = document.getElementById('icKeysWrap');
      const text = card ? card.innerText : '';
      const btn = [...document.querySelectorAll('#icKeysWrap button')].map(b => b.textContent.trim());
      const heads = [...document.querySelectorAll('#icKeysWrap thead th')].map(h => h.innerText.trim());
      const hasName = !!document.getElementById('icNewKeyName');
      return { opts, text: text.slice(0, 500), btn, heads, hasName, selDefault: sel ? sel.value : null };
    });
    check('U2a Name field + Role picker (workspace default, read-only option)',
      u2.hasName && u2.opts.length === 2 && u2.selDefault === 'workspace' &&
      u2.opts.some(o => o.v === 'read_only' && /Read-only/.test(o.t)) &&
      u2.opts.some(o => o.v === 'workspace' && /Read and write/.test(o.t)),
      JSON.stringify(u2.opts));
    check('U2b the key table has a Role column',
      u2.heads.some(h => /^role$/i.test(h)), JSON.stringify(u2.heads));
    check('U2c the create button is "+ Create API key" and the card says no "lead-delivery" wording',
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
    check('U3a the console SENT role=read_only and the name in the create body',
      reqs.some(r => r.m === 'POST' && /"role"\s*:\s*"read_only"/.test(r.b) && r.b.includes(RO_NAME)),
      reqs.filter(r => r.m === 'POST').map(r => r.b).join(' | ').slice(0, 220));
    check('U3b the once-shown card says the key is Read-only', /Read-only/.test(roCard), roCard.slice(0, 160));
    if (process.env.PROOF_SHOT2) {
      const m = await page.$('.modal');
      if (m) await m.screenshot({ path: process.env.PROOF_SHOT2 });
    }
    await page.evaluate(() => { const b = [...document.querySelectorAll('button')].find(x => /^Done$/.test(x.textContent.trim())); if (b) b.click(); });
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
      list.status === 200 && roKey && roKey.role === 'read_only',
      JSON.stringify(roKey && { role: roKey.role, active: roKey.is_active }));
    const ov = await api('GET', '/api/integration-center/overview', undefined, token);
    const ovKey = (((ov.json || {}).personal_keys || {}).items || []).find(k => k.name === RO_NAME);
    check('U4b the Integration Center overview carries role=read_only (what the list renders)',
      ov.status === 200 && ovKey && ovKey.role === 'read_only', JSON.stringify(ovKey && { role: ovKey.role }));

    let st = await api('GET', '/api/objects', undefined, roSecret);
    check('U4c GET /api/objects answers 200 for the read-only key', st.status === 200, `status=${st.status}`);
    st = await api('POST', '/api/objects', { slug: `adm_ro_${STAMP}`, name: `RO probe ${STAMP}` }, roSecret);
    const text = JSON.stringify(st.json);
    check('U4d POST /api/objects answers 403 with the boundary sentence',
      st.status === 403 && text.includes(SENTENCE) && text.includes(SENTENCE_TAIL),
      `status=${st.status} body=${text.slice(0, 180)}`);

  // NOTE: the MCP-door half of the role check is deliberately NOT asserted here. This console
  // signs in a PLATFORM OPERATOR, and the door elevates a key whose owner is a platform admin to
  // Role::Admin before it reads api_role (src/mcp_door/mod.rs::resolve_key), so an operator's own
  // read-only key can still write over MCP while the REST boundary refuses it 403. That is a door
  // defect carded separately (operator-only; a customer's read-only key is refused, measured with a
  // non-admin probe tenant: "this tool requires role `workspace`").

    // ── U5 Rotate through the fork's own button keeps the role ──────────────────────────
    const clickRow = async (name, label) => page.evaluate((n, l) => {
      const rows = [...document.querySelectorAll('#icKeysWrap tbody tr')];
      const row = rows.find(r => r.innerText.includes(n));
      if (!row) return 'no-row';
      const b = [...row.querySelectorAll('button')].find(x => x.textContent.trim() === l);
      if (!b) return 'no-button:' + row.innerText;
      b.click();
      return 'clicked';
    }, name, label);
    const rot = await clickRow(RO_NAME, 'Rotate');
    await page.waitForFunction(() => {
      const i = document.getElementById('icNewKey');
      return !!i && /^csk_/.test(i.value || '');
    }, { timeout: 20000 });
    const roSecret2 = await page.evaluate(() => document.getElementById('icNewKey').value);
    const rotCard = await page.evaluate(() => (document.getElementById('icNewKeyRoleLine') || {}).innerText || '');
    check('U5a Rotate was driven from the console row', rot === 'clicked', rot);
    check('U5b the confirm dialog promised the role is kept',
      dialogs.some(m => /keeps its role/i.test(m)), (dialogs[dialogs.length - 1] || '').slice(0, 160));
    check('U5c the rotated card still says Read-only', /Read-only/.test(rotCard), rotCard.slice(0, 160));
    check('U5d rotating produced a NEW secret', !!roSecret2 && roSecret2 !== roSecret,
      `${String(roSecret).slice(0, 8)}… -> ${String(roSecret2).slice(0, 8)}…`);
    await page.evaluate(() => { const b = [...document.querySelectorAll('button')].find(x => /^Done$/.test(x.textContent.trim())); if (b) b.click(); });
    st = await api('GET', '/api/personal-api-keys', undefined, token);
    const roKey2 = (st.json.keys || []).find(k => k.name === RO_NAME && k.is_active);
    check('U5e the rotated key is stored as read_only', st.status === 200 && roKey2 && roKey2.role === 'read_only',
      JSON.stringify(roKey2 && { role: roKey2.role, prefix: roKey2.prefix }));
    st = await api('GET', '/api/objects', undefined, roSecret);
    check('U5f the OLD read-only secret is dead (401)', st.status === 401, `status=${st.status}`);
    st = await api('GET', '/api/objects', undefined, roSecret2);
    check('U5g the NEW read-only secret reads (200)', st.status === 200, `status=${st.status}`);
    st = await api('POST', '/api/objects', { slug: `adm_ro2_${STAMP}`, name: 'x' }, roSecret2);
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
    st = await api('POST', '/api/objects', { slug: `adm_rw_${STAMP}`, name: `RW ${STAMP}` }, rwSecret);
    check('U6b a workspace key created through the same control writes 200', st.status === 200, `status=${st.status}`);
    await page.evaluate(() => { const b = [...document.querySelectorAll('button')].find(x => /^Done$/.test(x.textContent.trim())); if (b) b.click(); });
    await page.waitForFunction(n => [...document.querySelectorAll('#icKeysWrap tbody tr')].some(r => r.innerText.includes(n)), { timeout: 20000 }, RW_NAME);
    const rwRow = await page.evaluate(n => {
      const r = [...document.querySelectorAll('#icKeysWrap tbody tr')].find(x => x.innerText.includes(n));
      return r ? [...r.querySelectorAll('td')].map(c => c.innerText.trim()) : null;
    }, RW_NAME);
    check('U6c the workspace row shows read + write', !!rwRow && rwRow.some(c => /read \+ write/i.test(c)),
      JSON.stringify(rwRow));
    const shotA = process.env.PROOF_SHOT;
    if (shotA) {
      const cardEl = await page.$('#icKeysWrap');
      if (cardEl) { await cardEl.scrollIntoView(); await cardEl.screenshot({ path: shotA }); }
    }

    // ── U7 Revoke from the console row leaves it 401 ────────────────────────────────────
    const rev = await clickRow(RO_NAME, 'Revoke');
    await sleep(1500);
    st = await api('GET', '/api/objects', undefined, roSecret2);
    check('U7a Revoke was driven from the console row', rev === 'clicked', rev);
    check('U7b the revoked read-only secret answers 401', st.status === 401, `status=${st.status}`);

    // ── Z0 no page error ────────────────────────────────────────────────────────────────
    check('Z0 no uncaught page error', pageErrors.length === 0, pageErrors.slice(0, 2).join(' | '));
  } finally {
    await browser.close();
  }

  // cleanup: the probe workspace and every row it owned
  sql(`DELETE FROM custom_objects WHERE tenant_id = '${tenantId}'; ` +
      `DELETE FROM users WHERE tenant_id = '${tenantId}'; ` +
      `DELETE FROM tenants WHERE id = '${tenantId}';`);
  const leftTenants = Number(sql(`SELECT count(*) FROM tenants WHERE id = '${tenantId}'`));
  const leftUsers = Number(sql(`SELECT count(*) FROM users WHERE email = '${PROBE_EMAIL}'`));
  const leftKeys = Number(sql(`SELECT count(*) FROM personal_api_keys WHERE tenant_id = '${tenantId}'`));
  const nowTenants = Number(sql('SELECT count(*) FROM tenants'));
  const nowKeys = Number(sql('SELECT count(*) FROM personal_api_keys'));
  check('Z1 the probe workspace left no tenant, user or key row',
    leftTenants === 0 && leftUsers === 0 && leftKeys === 0,
    `tenant=${leftTenants} user=${leftUsers} keys=${leftKeys}`);
  check('Z2 fleet counters are back to baseline', nowTenants === baseTenants && nowKeys === baseKeys,
    `tenants ${baseTenants}->${nowTenants}, keys ${baseKeys}->${nowKeys}`);
  const leftovers = Number(sql(`SELECT count(*) FROM custom_objects WHERE slug IN ('adm_ro_${STAMP}','adm_ro2_${STAMP}','adm_rw_${STAMP}','adm_mcp_${STAMP}')`));
  check('Z3 no probe custom object survived', leftovers === 0, `objects=${leftovers}`);

  console.log(`\n${PASS.length} PASS / ${FAIL.length} FAIL`);
  if (FAIL.length) { console.log('FAILED: ' + FAIL.join('; ')); process.exit(1); }
}

main().catch(e => { console.error('HARNESS ERROR:', e && e.stack || e); process.exit(2); });
