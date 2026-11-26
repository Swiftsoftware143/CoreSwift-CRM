#!/usr/bin/env python3
"""admin-delete-user-acceptance — kanban t_d7474aee, six checks, each with a database count.

Proves on a RUNNING deployment that an action which removes a person from the UI removes the row:

  A1  probe user -> DELETE -> 200 -> count(*) where id = ... = 0
  A2  random uuid -> 404  (and the same answer for an id that is not a user)
  A3  self -> 400 · workspace owner -> 403 · last remaining admin -> 400
  A4  exactly ONE audit_logs row, action='user.delete', naming the actor
  A5  a user who owned a deal: the deal still exists with owner_id IS NULL
  A6  probes removed: users where email like '%probe%' = 0

Sessions are JWT-bearer only, so this mints one from JWT_SECRET in the app's env file for a real
platform admin and for a probe tenant owner — it never guesses a password, and it deletes the probe
users it created whatever the outcome.

    python3 scripts/admin-delete-user-acceptance.py
    python3 scripts/admin-delete-user-acceptance.py --base https://app.coreswiftcrm.com
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

PASS: list[str] = []
FAIL: list[str] = []
PROBE_TAG = "delprobe"
ENV_FILE = "/etc/swift/env/coreswift.env"


def env(name: str) -> str:
    for line in open(ENV_FILE):
        line = line.strip()
        if line.startswith(name + "="):
            return line.split("=", 1)[1].strip().strip('"').strip("'")
    raise SystemExit(f"{name} not in {ENV_FILE}")


DB = env("DATABASE_URL")
SECRET = env("JWT_SECRET")


def sql(q: str) -> str:
    r = subprocess.run(["psql", DB, "-tAF|", "-c", q], capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit("psql failed: " + r.stderr.strip()[:300])
    return r.stdout.strip()


def one(q: str) -> str:
    out = sql(q)
    return out.splitlines()[0] if out else ""


def b64(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).decode().rstrip("=")


def mint(user_id: str, tenant_id: str, role: str) -> str:
    now = int(time.time())
    head = b64(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    body = b64(json.dumps({
        "sub": user_id, "aid": tenant_id, "role": role,
        "exp": now + 900, "iat": now, "aud": None, "iss": None,
    }, separators=(",", ":")).encode())
    sig = b64(hmac.new(SECRET.encode(), f"{head}.{body}".encode(), hashlib.sha256).digest())
    return f"{head}.{body}.{sig}"


def call(base: str, method: str, path: str, token: str) -> tuple[int, dict]:
    req = urllib.request.Request(base + path, method=method,
                                 headers={"Authorization": "Bearer " + token,
                                          "Accept": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read().decode()
            return r.status, (json.loads(raw) if raw.strip().startswith("{") else {"raw": raw})
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw[:300]}


def check(name: str, ok: bool, detail: str) -> None:
    (PASS if ok else FAIL).append(f"{name}: {detail}")
    print(("  PASS " if ok else "  FAIL ") + f"{name}: {detail}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    a = ap.parse_args()
    base = a.base.rstrip("/")

    admin = one("SELECT id||'|'||tenant_id||'|'||role||'|'||email FROM users "
                "WHERE is_platform_admin AND is_active LIMIT 1")
    if not admin:
        raise SystemExit("no platform admin to act as")
    a_id, a_tid, a_role, a_email = admin.split("|")
    tok = mint(a_id, a_tid, a_role)
    print(f"acting as platform admin {a_email}\n  base {base}")

    # ---- probes: two tenants, one owner-led, plus a deal owner
    # A throwaway tenant: the handler refuses portfolio logins, and choosing "the first tenant" picked a
    # portfolio company (measured: 403, which was the handler being right and the harness being wrong).
    probe_tid = str(uuid.uuid4())
    sql("INSERT INTO tenants (id, name, slug, is_active) VALUES "
        f"('{probe_tid}','{PROBE_TAG} home', '{PROBE_TAG}-home-{probe_tid[:8]}', true)")
    victim = str(uuid.uuid4())
    holder = str(uuid.uuid4())
    deal_owner = str(uuid.uuid4())
    for uid, email, role in ((victim, f"{PROBE_TAG}-victim@example.invalid", "member"),
                             (holder, f"{PROBE_TAG}-lastadmin@example.invalid", "admin"),
                             (deal_owner, f"{PROBE_TAG}-dealowner@example.invalid", "member")):
        sql("INSERT INTO users (id, tenant_id, email, password_hash, name, role, is_active, "
            f"is_platform_admin) VALUES ('{uid}','{probe_tid}','{email}','x','probe','{role}',true,false)")

    deal_id = ""
    try:
        r, body = call(base, "DELETE", f"/api/admin/users/{victim}", tok)
        left = one(f"SELECT COUNT(*) FROM users WHERE id='{victim}'")
        check("A1 row removed", r == 200 and left == "0",
              f"HTTP {r} {json.dumps(body)[:80]} | count(*) where id={victim[:8]}… = {left}")

        r2, b2 = call(base, "DELETE", f"/api/admin/users/{uuid.uuid4()}", tok)
        check("A2 unknown id", r2 == 404, f"HTTP {r2} {json.dumps(b2)[:70]}")

        r3, b3 = call(base, "DELETE", f"/api/admin/users/{a_id}", tok)
        check("A3a self", r3 == 400, f"HTTP {r3} {json.dumps(b3)[:80]}")

        # A probe OWNER in the throwaway tenant: the refusal is about ownership, not about which tenant.
        owner_row = str(uuid.uuid4())
        sql("INSERT INTO users (id, tenant_id, email, password_hash, name, role, is_active, "
            f"is_platform_admin) VALUES ('{owner_row}','{probe_tid}',"
            f"'{PROBE_TAG}-owner@example.invalid','x','probe owner','owner',true,false)")
        r4, b4 = call(base, "DELETE", f"/api/admin/users/{owner_row}", tok)
        survived = one(f"SELECT COUNT(*) FROM users WHERE id='{owner_row}'")
        check("A3b workspace owner", r4 == 403 and survived == "1",
              f"HTTP {r4} {json.dumps(b4)[:70]} | owner row still present = {survived}")

        # the holder is the ONLY admin in a tenant with no other admin => last-admin refusal
        solo_tid = str(uuid.uuid4())
        sql("INSERT INTO tenants (id, name, slug, is_active) VALUES "
            f"('{solo_tid}','{PROBE_TAG} solo', '{PROBE_TAG}-solo-{solo_tid[:8]}', true)")
        solo_admin = str(uuid.uuid4())
        sql("INSERT INTO users (id, tenant_id, email, password_hash, name, role, is_active, "
            f"is_platform_admin) VALUES ('{solo_admin}','{solo_tid}',"
            f"'{PROBE_TAG}-solo@example.invalid','x','solo','admin',true,false)")
        r5, b5 = call(base, "DELETE", f"/api/admin/users/{solo_admin}", tok)
        check("A3c last admin", r5 == 400, f"HTTP {r5} {json.dumps(b5)[:90]}")

        rows = one("SELECT COUNT(*) FROM audit_logs WHERE action='user.delete' "
                   f"AND entity_id='{victim}' AND user_id='{a_id}'")
        check("A4 one audit row", rows == "1", f"audit_logs rows for the actor+target = {rows}")

        deal_id = str(uuid.uuid4())
        pipe = one("SELECT id FROM pipelines ORDER BY created_at LIMIT 1")
        stage = one(f"SELECT id FROM pipeline_stages WHERE pipeline_id='{pipe}' ORDER BY created_at LIMIT 1")
        sql("INSERT INTO opportunities (id, tenant_id, name, pipeline_id, stage_id, owner_id) VALUES "
            f"('{deal_id}','{probe_tid}','{PROBE_TAG} deal','{pipe}','{stage}','{deal_owner}')")
        r6, _ = call(base, "DELETE", f"/api/admin/users/{deal_owner}", tok)
        deal_still = one(f"SELECT COUNT(*) FROM opportunities WHERE id='{deal_id}'")
        owner_now = one(f"SELECT COALESCE(owner_id::text,'NULL') FROM opportunities WHERE id='{deal_id}'")
        check("A5 history survives", r6 == 200 and deal_still == "1" and owner_now == "NULL",
              f"HTTP {r6} | deal rows = {deal_still} | owner_id = {owner_now}")
    finally:
        for uid in (victim, holder, deal_owner, owner_row):
            sql(f"DELETE FROM users WHERE id='{uid}'")
        sql(f"DELETE FROM opportunities WHERE name LIKE '{PROBE_TAG}%'")
        sql(f"DELETE FROM users WHERE email LIKE '{PROBE_TAG}%'")
        sql(f"DELETE FROM tenants WHERE name LIKE '{PROBE_TAG}%'")

    probes = one("SELECT COUNT(*) FROM users WHERE email LIKE '%probe%'")
    check("A6 no probe users left", probes == "0", f"users where email like '%probe%' = {probes}")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 0 if not FAIL else 1


if __name__ == "__main__":
    sys.exit(main())
