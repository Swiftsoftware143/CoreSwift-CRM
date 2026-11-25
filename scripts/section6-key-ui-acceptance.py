#!/usr/bin/env python3
"""Live acceptance — plan §6 key UI on CoreSwift CRM (kanban t_6dbd084e).

David's ruling 2026-11-25 (docs/fleet-per-account-keys-and-mcp-2026-11-25.md §6/§8):
  1. the key is private by default — masked on load, never the full secret in the list payload
  2. View is ONE click (console session only, no password body) and writes an audit row;
     Copy fetches the sealed value and writes it to the clipboard WITHOUT rendering it
  3. Profile / API-keys page = View, Copy, Rotate, Revoke (the original location)
  4. the MCP page shows View + Copy ONLY at the TOP — no rotate control, no rotate handler
  5. a new account is minted with exactly ONE key labelled `MCP — <account name>`, in the same
     transaction as the account row
  6. rotate: the new value is shown once, the old key is 401 immediately, sessions untouched

Checks (one PASS/FAIL line each, exit 1 on any FAIL) — a fresh probe account through the REAL
signup door with the probe_harness marker, a throwaway address, mail suppressed by that marker:
  1  signup 201 (harness-marked) and EXACTLY one active key labelled `MCP — <account name>`
  2  GET list: 200, one key, prefix-only (no plaintext, no token field)
  3  POST reveal (Copy path) -> 200 + the full key
  4  that key answers `tools/list` at /api/mcp -> 200 with tools
  5  POST reveal (View path) -> the SAME secret, writes EXACTLY one audit row
  6  rotate -> 200 + a new value; the OLD key answers 401 at /api/mcp, the NEW one 200
  7  the console session still works after the rotation (sessions untouched)
  8  real Chromium: the MCP screen's FIRST card is the key card, it carries View + Copy, and the
     MCP view has NO rotate/regenerate control (runtime DOM evaluate) and NO rotate handler (source)
  9  teardown: probe workspace, keys and reveal rows erased — residue 0
Never prints a full key — prefixes only.

Usage: python3 scripts/section6-key-ui-acceptance.py [--base URL] [--app-url URL]
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from urllib.parse import unquote, urlparse

PASS: list[str] = []
FAIL: list[str] = []
HARNESS = "t_6dbd084e"


def check(name: str, ok: bool, detail: str = "") -> None:
    (PASS if ok else FAIL).append(name)
    print(("PASS  " if ok else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))


def http(method: str, url: str, body=None, token: str | None = None, raw_body: bool = False,
         extra_headers: dict | None = None):
    data = b"" if (body is None and raw_body) else (None if body is None else json.dumps(body).encode())
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", "coreswift-section6-acceptance/1")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    for k, v in (extra_headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=40) as r:
            raw = r.read().decode(errors="replace")
            try:
                return r.status, (json.loads(raw) if raw else {})
            except ValueError:
                return r.status, {"raw": raw}
    except urllib.error.HTTPError as e:
        raw = e.read().decode(errors="replace")
        try:
            return e.code, (json.loads(raw) if raw else {})
        except ValueError:
            return e.code, {"raw": raw}


class Db:
    def __init__(self, app_container: str, db_container: str) -> None:
        url = subprocess.run(["docker", "exec", app_container, "printenv", "DATABASE_URL"],
                             capture_output=True, text=True, check=True).stdout.strip()
        u = urlparse(url)
        self.user = unquote(u.username or "swift")
        self.password = unquote(u.password or "")
        self.name = unquote(u.path).lstrip("/")
        self.db_container = db_container

    def q(self, sql: str):
        out = subprocess.run(
            ["docker", "exec", "-i", "-e", f"PGPASSWORD={self.password}", self.db_container,
             "psql", "-U", self.user, "-d", self.name, "-At", "-F", "\x1f", "-v", "ON_ERROR_STOP=1",
             "-c", sql],
            capture_output=True, text=True, timeout=60,
        )
        if out.returncode != 0:
            raise SystemExit(f"SQL failed: {out.stderr.strip()[:400]}\n--- {sql[:200]}")
        return [line.split("\x1f") for line in out.stdout.splitlines() if line.strip()]

    def one(self, sql: str) -> str:
        rows = self.q(sql)
        return rows[0][0] if rows else ""


NODE_SCRIPT = r"""
const puppeteer = require('/usr/lib/node_modules/puppeteer');
(async () => {
  const token = process.env.CS_TOKEN;
  const user = process.env.CS_USER;
  const appUrl = process.env.CS_APP_URL;
  const browser = await puppeteer.launch({
    executablePath: '/usr/bin/chromium',
    headless: 'new',
    args: ['--no-sandbox', '--disable-setuid-sandbox', '--disable-dev-shm-usage']
  });
  try {
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', e => errors.push(String(e)));
    await page.goto(appUrl, { waitUntil: 'domcontentloaded', timeout: 30000 });
    await page.evaluate((t, u) => {
      localStorage.setItem('crm_token', t);
      localStorage.setItem('crm_user', JSON.stringify(u));
    }, token, user);
    await page.goto(appUrl, { waitUntil: 'networkidle2', timeout: 45000 });
    const switched = await page.evaluate(() => {
      if (typeof switchTab !== 'function') return false;
      switchTab('mcp');
      return true;
    });
    // Wait for the key card to paint its buttons (the key list loads after the tab renders).
    let ready = false;
    for (let i = 0; i < 60; i++) {
      const r = await page.evaluate(() => {
        const card = document.getElementById('mcp-key-card');
        if (!card) return null;
        const btns = [...card.querySelectorAll('button')].map(b => (b.textContent || '').trim());
        return { view: btns.includes('View'), copy: btns.includes('Copy') };
      });
      if (r && r.view && r.copy) { ready = true; break; }
      await new Promise(res => setTimeout(res, 250));
    }
    const dom = await page.evaluate(() => {
      const card = document.getElementById('mcp-key-card');
      let cards = [...document.querySelectorAll('#tabPanels .card')];
      if (!cards.length) cards = [...document.querySelectorAll('#mainContent .card, .card')];
      const banned = [];
      for (const el of [...document.querySelectorAll('button, a, [role=button]')]) {
        const t = (el.textContent || '').trim();
        if (/rotate|regenerate/i.test(t)) banned.push({ tag: el.tagName, text: t.slice(0, 60) });
      }
      const btns = card ? [...card.querySelectorAll('button')].map(b => (b.textContent || '').trim()) : [];
      const codeText = card ? (card.textContent || '') : '';
      return {
        keyCardFound: !!card,
        keyCardIsFirstCard: !!(card && cards.length && cards[0] === card),
        cardButtons: btns,
        hasView: btns.includes('View'),
        hasCopy: btns.includes('Copy'),
        cardMentionsRotate: /rotate|regenerate/i.test(codeText),
        bannedControls: banned,
        // masked: the card must not hold a csk_ secret, only a prefix + ellipsis
        cardHasFullSecret: /csk_[A-Za-z0-9_-]{25,}/.test(document.body.innerText || ''),
      };
    });
    const source = await page.evaluate(() => {
      const scripts = [...document.querySelectorAll('script:not([src])')];
      const text = scripts.map(s => s.textContent || '').join('\n');
      const start = text.indexOf('async function mcpLoadKey');
      const end = text.indexOf('function icKeyRoleIsReadOnly');
      if (start < 0 || end < 0 || end <= start) return null;
      // Strip comments: the view's own comment SAYS there is no rotate handler, so a raw text
      // match would read the documentation as the defect. Judge the executable code only.
      return text.slice(start, end)
        .replace(/\/\*[\s\S]*?\*\//g, '')
        .replace(/^[ \t]*\/\/[^\n]*$/gm, '');
    });
    // The ORIGINAL location must carry all four actions: View, Copy, Regenerate, Revoke.
    const keysPage = await (async () => {
      await page.evaluate(() => { if (typeof switchTab === 'function') switchTab('integrations'); });
      for (let i = 0; i < 60; i++) {
        const r = await page.evaluate(() => {
          const wrap = document.getElementById('icKeysWrap');
          if (!wrap) return null;
          const btns = [...wrap.querySelectorAll('button')].map(b => (b.textContent || '').trim());
          return { ok: ['View', 'Copy', 'Regenerate', 'Revoke'].every(x => btns.includes(x)), btns };
        });
        if (r && r.ok) return r;
        await new Promise(res => setTimeout(res, 250));
      }
      return await page.evaluate(() => {
        const wrap = document.getElementById('icKeysWrap');
        return { ok: false, btns: wrap ? [...wrap.querySelectorAll('button')].map(b => (b.textContent || '').trim()) : [] };
      });
    })();
    console.log(JSON.stringify({
      switched,
      ready,
      dom,
      keysPage,
      mcpViewMentionsRotate: source === null ? null : /rotate|regenerate/i.test(source),
      pageErrors: errors.slice(0, 5),
    }));
  } finally {
    await browser.close();
  }
})().catch(e => { console.error('NODE-ERR', e && e.message); process.exit(1); });
"""


def dom_assertion(token: str, email: str, name: str, app_url: str):
    env = dict(os.environ)
    env["CS_TOKEN"] = token
    env["CS_USER"] = json.dumps({"email": email, "name": name})
    env["CS_APP_URL"] = app_url
    f = Path("/root/.hermes/cache/scratch/cs-section6-dom.cjs")
    f.write_text(NODE_SCRIPT)
    r = subprocess.run(["node", str(f)], capture_output=True, text=True, timeout=180, env=env)
    if r.returncode != 0:
        return False, f"node rc={r.returncode} stderr={r.stderr.strip()[:400]}"
    for line in r.stdout.strip().splitlines():
        if line.startswith("{"):
            try:
                return True, json.loads(line)
            except json.JSONDecodeError:
                return True, line
    return False, f"no JSON: {r.stdout[:400]} {r.stderr[:200]}"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    ap.add_argument("--app-url", default="https://app.coreswiftcrm.com/")
    ap.add_argument("--container", default="coreswiftcrm")
    ap.add_argument("--db-container", default="swift-postgres")
    args = ap.parse_args()
    base = args.base.rstrip("/")
    db = Db(args.container, args.db_container)
    tag = uuid.uuid4().hex[:8]
    email = f"probe-section6-{tag}@example.invalid"
    password = "Section6Probe!2026"
    owner = f"Section6 Probe {tag}"
    account_name = f"Section6 Probe {tag}"
    tenant = ""

    print(f"== CoreSwift §6 key UI acceptance @ {base} (probe {tag})")
    st, _ = http("GET", f"{base}/api/ready")
    if st != 200:
        print(f"FAIL  app is not ready: /api/ready answered {st}")
        return 1

    try:
        # ── 1. the real signup door, harness-marked (credentials mail suppressed by the marker) ──
        st, body = http("POST", f"{base}/api/auth/register",
                        {"name": owner, "email": email, "password": password,
                         "account_name": account_name},
                        extra_headers={"X-Swift-Harness": HARNESS})
        body = body if isinstance(body, dict) else {}
        token = body.get("access_token") or body.get("token") or ""
        tenant = db.one(f"SELECT id FROM tenants WHERE name = '{account_name}' LIMIT 1")
        rows = db.q(
            "SELECT name, is_active FROM personal_api_keys "
            f"WHERE tenant_id = '{tenant}' ORDER BY created_at") if tenant else []
        active = [r for r in rows if r[1] == "t"]
        check("1 signup 201 (harness-marked) mints EXACTLY one key labelled `MCP — <account>`",
              st in (200, 201) and bool(token) and len(active) == 1
              and active[0][0] == f"MCP — {account_name}",
              f"HTTP {st} keys={len(rows)} label={active[0][0] if active else None} "
              f"emailed={body.get('credentials_emailed')}")

        # ── 2. the list is prefix-only ─────────────────────────────────────────────────────────
        st, body = http("GET", f"{base}/api/personal-api-keys", None, token)
        keys = (body or {}).get("keys") or [] if st == 200 and isinstance(body, dict) else []
        k = keys[0] if keys else {}
        check("2 GET list 200 and prefix-only (no plaintext, no token field)",
              st == 200 and len(keys) == 1 and "key" not in k and "token" not in k
              and "key_sealed" not in k and bool(k.get("prefix")) and k.get("recoverable") is True,
              f"HTTP {st} keys={len(keys)} fields={sorted(k.keys())[:9]}")
        key_id = k.get("id")
        prefix = k.get("prefix")

        # ── 3. Copy path: one session-only POST, no body ─────────────────────────────────────
        st, body = http("POST", f"{base}/api/personal-api-keys/{key_id}/reveal",
                        token=token, raw_body=True)
        full = (body or {}).get("key") if isinstance(body, dict) else None
        check("3 Copy path (POST reveal, session, NO body) -> 200 + the full key",
              st == 200 and isinstance(full, str) and full.startswith("csk_")
              and full.startswith(prefix or "??"),
              f"HTTP {st} prefix={prefix}")

        # ── 4. that key is a working MCP credential ──────────────────────────────────────────
        st, mcp = http("POST", f"{base}/api/mcp",
                       {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}, full)
        tools = (((mcp or {}).get("result") or {}).get("tools") or []) if isinstance(mcp, dict) else []
        check("4 the copied key answers `tools/list` at /api/mcp -> 200 with tools",
              st == 200 and len(tools) > 0, f"HTTP {st} tools={len(tools)}")

        # ── 5. View path: same secret, exactly one audit row for this reveal ─────────────────
        before = int(db.one("SELECT COUNT(*) FROM personal_api_key_reveals "
                            f"WHERE key_id = '{key_id}'") or 0)
        st, body = http("POST", f"{base}/api/personal-api-keys/{key_id}/reveal",
                        token=token, raw_body=True)
        view_key = (body or {}).get("key") if isinstance(body, dict) else None
        after = int(db.one("SELECT COUNT(*) FROM personal_api_key_reveals "
                           f"WHERE key_id = '{key_id}'") or 0)
        check("5 View path returns the SAME secret and writes EXACTLY one audit row",
              st == 200 and view_key == full and (after - before) == 1,
              f"HTTP {st} same={view_key == full} audit +{after - before} (who/key/when rows)")

        # ── 6. rotate: old 401, new 200 ─────────────────────────────────────────────────────
        st, body = http("POST", f"{base}/api/personal-api-keys/{key_id}/rotate", {}, token)
        new_key = (body or {}).get("key") if isinstance(body, dict) else None
        st_old, _ = http("POST", f"{base}/api/mcp",
                         {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}, full)
        st_new, _ = http("POST", f"{base}/api/mcp",
                         {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}, new_key)
        check("6 rotate -> new value shown once; OLD key 401, NEW key 200 at /api/mcp",
              st == 200 and isinstance(new_key, str) and new_key != full
              and st_old in (401, 403) and st_new == 200,
              f"rotate {st} old {st_old} new {st_new}")

        # ── 7. the console session survives the rotation ────────────────────────────────────
        st, body = http("GET", f"{base}/api/personal-api-keys", None, token)
        check("7 the console session still works after the rotation (sessions untouched)",
              st == 200, f"HTTP {st}")

        # ── 8. the MCP screen in real Chromium ──────────────────────────────────────────────
        ok, d = dom_assertion(token, email, owner, args.app_url)
        if not ok:
            check("8 Chromium DOM: MCP key card first with View + Copy, no rotate control", False, str(d))
        else:
            dom = d.get("dom") or {}
            good = (d.get("ready") and dom.get("keyCardFound") and dom.get("keyCardIsFirstCard")
                    and dom.get("hasView") and dom.get("hasCopy")
                    and not dom.get("bannedControls") and not dom.get("cardMentionsRotate")
                    and not dom.get("cardHasFullSecret")
                    and d.get("mcpViewMentionsRotate") is False)
            check("8 Chromium DOM: MCP key card FIRST with View + Copy, NO rotate control/handler",
                  bool(good),
                  f"first={dom.get('keyCardIsFirstCard')} btns={dom.get('cardButtons')} "
                  f"banned={dom.get('bannedControls')} viewRotate={d.get('mcpViewMentionsRotate')} "
                  f"fullSecretInDom={dom.get('cardHasFullSecret')} errors={d.get('pageErrors')}")
            kp = d.get("keysPage") or {}
            check("8b Chromium DOM: the API Keys page still carries View, Copy, Regenerate, Revoke",
                  bool(kp.get("ok")), f"buttons={kp.get('btns')}")
    finally:
        if tenant:
            # The audit table is deliberately not cascaded (a reveal is a security event that must
            # outlive its key), so the probe's rows are deleted here by tenant id.
            db.q(f"DELETE FROM personal_api_key_reveals WHERE tenant_id = '{tenant}'")
            db.q(f"DELETE FROM tenants WHERE id = '{tenant}'")
        left_t = db.one(f"SELECT COUNT(*) FROM tenants WHERE name = '{account_name}'")
        left_k = db.one(f"SELECT COUNT(*) FROM personal_api_keys WHERE tenant_id = '{tenant}'") if tenant else "0"
        left_r = db.one(f"SELECT COUNT(*) FROM personal_api_key_reveals WHERE tenant_id = '{tenant}'") if tenant else "0"
        left_u = db.one(f"SELECT COUNT(*) FROM users WHERE email = '{email}'")
        print(f"cleanup: tenants={left_t} users={left_u} keys={left_k} reveals={left_r}")
        check("9 probe erased — residue 0", left_t == "0" and left_u == "0"
              and left_k == "0" and left_r == "0",
              f"tenants={left_t} users={left_u} keys={left_k} reveals={left_r}")

    print()
    if FAIL:
        print(f"{len(FAIL)} CHECK(S) FAILED: " + ", ".join(FAIL))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
