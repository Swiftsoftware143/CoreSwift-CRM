#!/usr/bin/env python3
"""Live acceptance — the key-View ruling (plan §6.2) on CoreSwift CRM.

docs/fleet-per-account-keys-and-mcp-2026-11-25.md §6.2 is David's own words (2026-11-25): the key
is private by default (masked on load) and **View is ONE CLICK** — a valid console session is
enough, no password re-prompt — while every reveal is still audited. §8 records the rule and the
security half: an API key presented as the bearer on that route is refused 403, so a read-only key
can never use the reveal door to obtain the account's read+write secret.

Checks (one PASS/FAIL line each, exit 1 on any FAIL):
  1. probe A's session mints ONE sealed key; the LIST is prefix-only (no plaintext)
  2. POST reveal with the SESSION and NO BODY AT ALL            -> 200 + the full key
  3. the legacy body {"password": "…"} is accepted and ignored  -> 200
  4. every reveal wrote a personal_api_key_reveals row (>= 2 by now)
  5. reveal with NO credential                                  -> 401
  6. reveal with an issued API KEY as bearer                    -> 403 (session-only)
  7. probe B's key id with probe A's session                    -> 404 (never another account)
  8. the revealed value is a working credential (GET /api/external/lists -> 200)
  9. revoke the key, then reveal it with the session            -> 404 (revoked is gone, not shown)
 10. teardown: both probe workspaces erased, residue 0

Never prints a full key — prefixes only.

Usage: python3 scripts/api-key-reveal-session-acceptance.py [--base URL] [--container NAME]
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from urllib.parse import unquote, urlparse

PASS: list[str] = []
FAIL: list[str] = []
HARNESS = "t_76d82f06"


def check(name: str, ok: bool, detail: str = "") -> None:
    (PASS if ok else FAIL).append(name)
    print(("PASS  " if ok else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))


def http(method: str, url: str, body=None, token: str | None = None, raw_body: bool = False):
    if body is None:
        data = b"" if raw_body else None
    else:
        data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", "coreswift-key-view-acceptance/1")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
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
    """psql through the postgres container — the probe never needs a host-side credential."""

    def __init__(self, app_container: str, db_container: str) -> None:
        self.url = subprocess.run(
            ["docker", "exec", app_container, "printenv", "DATABASE_URL"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        self.secret = subprocess.run(
            ["docker", "exec", app_container, "printenv", "JWT_SECRET"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        u = urlparse(self.url)
        self.user = unquote(u.username or "swift")
        self.password = unquote(u.password or "")
        self.name = unquote(u.path).lstrip("/")
        self.db_container = db_container

    def q(self, sql: str):
        out = subprocess.run(
            ["docker", "exec", "-i", "-e", f"PGPASSWORD={self.password}", self.db_container,
             "psql", "-U", self.user, "-d", self.name, "-At", "-F", "\x1f", "-v",
             "ON_ERROR_STOP=1", "-c", sql],
            capture_output=True, text=True, timeout=60,
        )
        if out.returncode != 0:
            raise SystemExit(f"SQL failed: {out.stderr.strip()[:400]}\n--- {sql[:200]}")
        return [line.split("\x1f") for line in out.stdout.splitlines() if line.strip()]

    def one(self, sql: str) -> str:
        rows = self.q(sql)
        return rows[0][0] if rows else ""


def b64(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def jwt(secret: str, sub: str, aid: str, role: str = "owner", ttl: int = 900) -> str:
    """A real console session for a REAL users row (the middleware checks the row exists)."""
    now = int(time.time())
    head = b64(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    body = b64(json.dumps({
        "sub": sub, "aid": aid, "role": role, "iat": now, "exp": now + ttl,
        "iss": "coreswift", "aud": "coreswift-api",
    }, separators=(",", ":")).encode())
    sig = b64(hmac.new(secret.encode(), f"{head}.{body}".encode(), hashlib.sha256).digest())
    return f"{head}.{body}.{sig}"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    ap.add_argument("--container", default="coreswiftcrm")
    ap.add_argument("--db-container", default="swift-postgres")
    args = ap.parse_args()
    base = args.base.rstrip("/")
    db = Db(args.container, args.db_container)
    stamp = str(int(time.time()))[-7:]
    tenants: list[str] = []

    print(f"== CoreSwift key View is one click — session-only acceptance @ {base} ({stamp})")
    st, _ = http("GET", f"{base}/api/ready")
    if st != 200:
        print(f"FAIL  app is not ready: /api/ready answered {st}")
        return 1

    def new_probe(tag: str) -> dict:
        tid = db.one(
            "INSERT INTO tenants (name, slug, probe_harness) "
            f"VALUES ('probe-keyview-{tag}-{stamp}', 'probe-keyview-{tag}-{stamp}', "
            f"'{HARNESS}') RETURNING id"
        )
        tenants.append(tid)
        uid = db.one(
            "INSERT INTO users (tenant_id, email, password_hash, name) "
            f"VALUES ('{tid}', 'probe-keyview-{tag}-{stamp}@example.invalid', 'x', "
            f"'KeyView Probe {tag}') RETURNING id"
        )
        return {"tenant": tid, "user": uid, "token": jwt(db.secret, uid, tid)}

    try:
        a = new_probe("a")
        b = new_probe("b")
        print(f"probe A {a['tenant']}  probe B {b['tenant']}")

        # ── 1. mint through the real console door (so the sealed copy exists) ────────────────
        st, body = http("POST", f"{base}/api/personal-api-keys",
                        {"name": f"probe-keyview-{stamp}", "role": "workspace"}, a["token"])
        key_id = (body or {}).get("id") if isinstance(body, dict) else None
        minted = (body or {}).get("key") if isinstance(body, dict) else None
        check("1 the session mints ONE sealed key", st == 200 and bool(key_id)
              and isinstance(minted, str) and minted.startswith("csk_"),
              f"HTTP {st} id={key_id}")

        st, body = http("GET", f"{base}/api/personal-api-keys", None, a["token"])
        keys = ((body or {}).get("keys") or []) if (st == 200 and isinstance(body, dict)) else []
        listed = next((k for k in keys if isinstance(k, dict) and k.get("id") == key_id), {})
        check("1b the LIST is prefix-only (no plaintext, recoverable=true)",
              st == 200 and bool(listed) and "key" not in listed and "token" not in listed
              and listed.get("recoverable") is True and bool(listed.get("prefix")),
              f"HTTP {st} keys={len(keys)}")

        # ── 2. one click: session + NO BODY ──────────────────────────────────────────────────
        st, body = http("POST", f"{base}/api/personal-api-keys/{key_id}/reveal",
                        token=a["token"], raw_body=True)
        full = body.get("key") if isinstance(body, dict) else None
        check("2 reveal with the session and NO body -> 200 + the full key",
              st == 200 and isinstance(full, str) and full.startswith("csk_")
              and full == minted and (body or {}).get("role") == "workspace",
              f"HTTP {st} prefix={(body or {}).get('prefix') if isinstance(body, dict) else None}")

        # ── 3. an older client that still sends a password keeps working ─────────────────────
        st, body = http("POST", f"{base}/api/personal-api-keys/{key_id}/reveal",
                        {"password": "ignored-legacy-field"}, a["token"])
        check("3 the legacy {\"password\":…} body is accepted and ignored -> 200",
              st == 200 and isinstance(body, dict) and bool(body.get("key")), f"HTTP {st}")

        # ── 4. the audit row ─────────────────────────────────────────────────────────────────
        n = int(db.one(
            "SELECT COUNT(*) FROM personal_api_key_reveals "
            f"WHERE key_id = '{key_id}' AND tenant_id = '{a['tenant']}'") or 0)
        check("4 every reveal wrote a personal_api_key_reveals row", n >= 2, f"{n} row(s)")

        # ── 5. anonymous ─────────────────────────────────────────────────────────────────────
        st, _ = http("POST", f"{base}/api/personal-api-keys/{key_id}/reveal", raw_body=True)
        check("5 reveal with no credential -> 401", st == 401, f"HTTP {st}")

        # ── 6. an API KEY cannot reach a session door ────────────────────────────────────────
        st, body = http("POST", f"{base}/api/personal-api-keys/{key_id}/reveal",
                        token=full, raw_body=True)
        refused = st == 403 and "console session" in json.dumps(body)
        check("6 reveal with the account's API key as bearer -> 403 (session-only)",
              refused, f"HTTP {st} {json.dumps(body)[:140]}")

        # ── 7. another account's key id is not found ─────────────────────────────────────────
        _, bbody = http("POST", f"{base}/api/personal-api-keys",
                        {"name": f"probe-keyview-b-{stamp}", "role": "workspace"}, b["token"])
        foreign = (bbody or {}).get("id") if isinstance(bbody, dict) else None
        st, body = http("POST", f"{base}/api/personal-api-keys/{foreign}/reveal",
                        token=a["token"], raw_body=True)
        check("7 probe B's key id with probe A's session -> 404", st == 404, f"HTTP {st}")

        # ── 8. the value revealed is a working credential ────────────────────────────────────
        st, _ = http("GET", f"{base}/api/external/lists", token=full)
        check("8 the revealed key authenticates (GET /api/external/lists -> 200)",
              st == 200, f"HTTP {st}")

        # ── 9. a revoked key is gone, not shown ──────────────────────────────────────────────
        st, _ = http("DELETE", f"{base}/api/personal-api-keys/{key_id}", None, a["token"])
        st2, _ = http("POST", f"{base}/api/personal-api-keys/{key_id}/reveal",
                      token=a["token"], raw_body=True)
        check("9 revoke the key, then reveal -> 404", st == 200 and st2 == 404,
              f"revoke {st} reveal {st2}")

    finally:
        ids = ",".join(f"'{t}'" for t in tenants) or "''"
        if tenants:
            # The audit table is deliberately NOT cascaded (a reveal is a security event that must
            # outlive the key), so a probe's rows are deleted here, by tenant id, not left behind.
            db.q(f"DELETE FROM personal_api_key_reveals WHERE tenant_id IN ({ids})")
            db.q(f"DELETE FROM tenants WHERE id IN ({ids})")
        left_tenants = db.one(
            f"SELECT COUNT(*) FROM tenants WHERE slug LIKE 'probe-keyview-%-{stamp}'")
        left_keys = db.one(
            f"SELECT COUNT(*) FROM personal_api_keys WHERE tenant_id IN ({ids})")
        left_reveals = db.one(
            f"SELECT COUNT(*) FROM personal_api_key_reveals WHERE tenant_id IN ({ids})")
        print(f"cleanup: probe workspaces removed (tenants left={left_tenants}, "
              f"keys={left_keys}, reveals={left_reveals})")
        check("10 both probe workspaces erased, residue 0",
              left_tenants == "0" and left_keys == "0" and left_reveals == "0",
              f"tenants={left_tenants} keys={left_keys} reveals={left_reveals}")

    print()
    if FAIL:
        print(f"{len(FAIL)} CHECK(S) FAILED: " + ", ".join(FAIL))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
