#!/usr/bin/env python3
"""mcp-key-role-elevation-proof — a platform admin's `read_only` key is read-only on the MCP door too.

Kanban t_0c16825c. The door's resolver tested `is_platform_admin` BEFORE the key's own `api_role`, so
an operator-owned `read_only` key resolved to Admin and `create_object` answered 200 over `/api/mcp`
while `/api/objects` refused the same key 403 — the two doors disagreed about one credential. The fix
(`src/mcp_door/mod.rs::key_role`) makes the key's role decisive: `read_only` caps even an operator's
key, and admin only upgrades a key that can already write.

Proves, on a RUNNING deployment over real HTTP, against a throwaway probe workspace:

  O1  operator-owned `read_only` key: MCP `create_object` refused (forbidden, «requires role …»)
  O2  the same key: REST `POST /api/objects` 403 with the boundary sentence — the doors now agree
  O3  the same key still READS over MCP (`list_objects`)
  O4  operator-owned `workspace` key: MCP `create_object` 200 and the object really exists
  O5  operator-owned `workspace` key: REST `POST /api/objects` 200 (unchanged)
  C1  customer (non-admin) `read_only` key: MCP write refused — unchanged
  C2  customer `read_only` key: MCP read answers, REST read answers — unchanged
  Z   the probe workspace removed; tenant / key / object counters back where they started

    python3 scripts/mcp-key-role-elevation-proof.py                  # 127.0.0.1:8084
    python3 scripts/mcp-key-role-elevation-proof.py --base https://app.coreswiftcrm.com
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

PASS: list[str] = []
FAIL: list[str] = []

# The exact sentences the two doors answer for a read-only credential.
REST_SENTENCE = "this key is read-only"
REST_SENTENCE_TAIL = "use a workspace key to write"
MCP_REFUSAL_TAIL = "requires role `workspace`"


def check(name: str, ok: bool, detail: str = "") -> None:
    (PASS if ok else FAIL).append(name)
    print(("PASS  " if ok else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))


class Db:
    """psql through the postgres container — the probe never needs a host-side credential."""

    def __init__(self, container: str, db_container: str) -> None:
        url = subprocess.run(
            ["docker", "exec", container, "printenv", "DATABASE_URL"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        m = re.match(r"postgres://([^:/@]+):([^@]*)@[^/]+/(\w+)", url)
        if not m:
            raise SystemExit("cannot parse DATABASE_URL from the app container")
        self.user, self.password, self.name = m.groups()
        self.db_container = db_container

    def q(self, sql: str) -> list[list[str]]:
        out = subprocess.run(
            ["docker", "exec", "-e", f"PGPASSWORD={self.password}", self.db_container,
             "psql", "-U", self.user, "-d", self.name, "-At", "-F", "\x1f", "-v",
             "ON_ERROR_STOP=1", "-c", sql],
            capture_output=True, text=True,
        )
        if out.returncode != 0:
            raise SystemExit(f"SQL failed: {out.stderr.strip()}\n{sql[:200]}")
        return [line.split("\x1f") for line in out.stdout.splitlines() if line.strip()]

    def one(self, sql: str) -> str:
        rows = self.q(sql)
        return rows[0][0] if rows else ""


def http(method: str, url: str, body=None, token: str | None = None) -> tuple[int, dict]:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", "coreswift-mcp-key-role-elevation-proof/1")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read().decode()
            return r.status, json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw) if raw else {}
        except ValueError:
            return e.code, {"raw": raw}


def rpc(base: str, token: str, name: str, args: dict) -> tuple[bool, dict]:
    st, body = http("POST", f"{base}/api/mcp", {
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": args},
    }, token)
    if st != 200:
        return True, {"http": st, "body": body}
    result = body.get("result") or {}
    payload = result.get("structuredContent")
    if not isinstance(payload, dict):
        text = ""
        for block in result.get("content") or []:
            if block.get("type") == "text":
                text = block.get("text", "")
                break
        try:
            payload = json.loads(text) if text else {}
        except ValueError:
            payload = {"text": text}
    return bool(result.get("isError")), payload


def mint_key(db: Db, tenant: str, user: str, role: str, name: str) -> str:
    """An issued `csk_` key exactly as `personal_api_keys::create` stores one (hash, never the secret)."""
    token = "csk_" + uuid.uuid4().hex + uuid.uuid4().hex
    digest = hashlib.sha256(token.encode()).hexdigest()
    key_id = db.one(
        "INSERT INTO personal_api_keys (tenant_id, user_id, name, key_hash, key_prefix, is_active, api_role) "
        f"VALUES ('{tenant}', '{user}', '{name}', '{digest}', '{token[:12]}', true, '{role}') RETURNING id"
    )
    if not key_id:
        raise SystemExit("could not mint the probe key")
    return token


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    ap.add_argument("--container", default="coreswiftcrm")
    ap.add_argument("--db-container", default="swift-postgres")
    ap.add_argument("--keep", action="store_true", help="leave the probe workspace behind")
    args = ap.parse_args()
    base = args.base.rstrip("/")
    db = Db(args.container, args.db_container)
    stamp = str(int(time.time()))[-7:]
    tenant = ""

    print(f"== CoreSwift MCP door: operator-owned key role @ {base} ({stamp})")
    status, _ = http("GET", f"{base}/api/ready")
    if status != 200:
        print(f"FAIL  app is not ready: /api/ready answered {status}")
        return 1

    base_tenants = db.one("SELECT count(*) FROM tenants")
    base_keys = db.one("SELECT count(*) FROM personal_api_keys")
    base_objects = db.one("SELECT count(*) FROM custom_objects")

    try:
        # ── probe workspace, one customer user and one platform-operator user ─────────────────
        tenant = db.one(
            "INSERT INTO tenants (name, slug, is_active, probe_harness) "
            f"VALUES ('probe-mcpkeyrole-{stamp}', 'probe-mcpkeyrole-{stamp}', true, "
            "'mcp-key-role-elevation-proof') RETURNING id"
        )
        dummy_hash = "$argon2id$v=19$m=19456,t=2,p=1$cHJvYmUtc2FsdA$cHJvYmUtc2FsdA"
        customer_user = db.one(
            "INSERT INTO users (tenant_id, email, password_hash, name, is_platform_admin) "
            f"VALUES ('{tenant}', 'probe-mcp-customer-{stamp}@local.coreswift', '{dummy_hash}', "
            f"'Probe Customer {stamp}', false) RETURNING id"
        )
        operator_user = db.one(
            "INSERT INTO users (tenant_id, email, password_hash, name, is_platform_admin) "
            f"VALUES ('{tenant}', 'probe-mcp-operator-{stamp}@local.coreswift', '{dummy_hash}', "
            f"'Probe Operator {stamp}', true) RETURNING id"
        )
        admins = db.one(
            "SELECT count(*) FROM users WHERE tenant_id = "
            f"'{tenant}' AND is_platform_admin IS TRUE"
        )
        check("A1 a throwaway probe workspace with one customer and one platform-operator user exists",
              bool(tenant) and bool(customer_user) and bool(operator_user) and admins == "1",
              f"tenant={tenant} customer={customer_user} operator={operator_user} admins={admins}")

        op_ro = mint_key(db, tenant, operator_user, "read_only", f"probe operator read-only {stamp}")
        op_rw = mint_key(db, tenant, operator_user, "workspace", f"probe operator workspace {stamp}")
        cu_ro = mint_key(db, tenant, customer_user, "read_only", f"probe customer read-only {stamp}")

        # ── O1 the operator's read_only key cannot write over MCP ─────────────────────────────
        hacked = f"mcp_hacked_{stamp}"
        err, payload = rpc(base, op_ro, "create_object", {"slug": hacked, "name": "mcp hacked"})
        text = json.dumps(payload)
        check("O1 operator-owned read_only key: MCP create_object is REFUSED (forbidden, role gate)",
              err and payload.get("error") == "forbidden" and MCP_REFUSAL_TAIL in text,
              f"isError={err} payload={text[:160]}")
        created = db.one(
            f"SELECT count(*) FROM custom_objects WHERE tenant_id='{tenant}' AND slug='{hacked}'")
        check("O1b nothing was created by that refused call", created == "0", f"objects={created}")

        # ── O2 the same key on REST: the two doors now agree ──────────────────────────────────
        st, body = http("POST", f"{base}/api/objects",
                        {"slug": f"rest_hacked_{stamp}", "name": "rest hacked"}, op_ro)
        rest_text = json.dumps(body)
        check("O2 the same operator read_only key is refused 403 on REST with the boundary sentence",
              st == 403 and REST_SENTENCE in rest_text and REST_SENTENCE_TAIL in rest_text,
              f"status={st} body={rest_text[:160]}")

        # ── O3 the key still READS over MCP ───────────────────────────────────────────────────
        err, payload = rpc(base, op_ro, "list_objects", {})
        check("O3 operator read_only key still reads over MCP (list_objects answers)",
              not err, f"isError={err} payload={json.dumps(payload)[:120]}")

        # ── O4 the operator's WORKSPACE key still writes over MCP (Admin is kept) ─────────────
        ok_slug = f"op_ok_{stamp}"
        err, payload = rpc(base, op_rw, "create_object", {"slug": ok_slug, "name": f"Operator write {stamp}"})
        exists = db.one(
            f"SELECT count(*) FROM custom_objects WHERE tenant_id='{tenant}' AND slug='{ok_slug}'")
        check("O4 operator-owned workspace key: MCP create_object answers and the object EXISTS",
              not err and exists == "1",
              f"isError={err} payload={json.dumps(payload)[:120]} objects={exists}")

        # ── O5 …and on REST (unchanged) ──────────────────────────────────────────────────────
        st, body = http("POST", f"{base}/api/objects",
                        {"slug": f"op_rest_ok_{stamp}", "name": f"Operator REST {stamp}"}, op_rw)
        check("O5 operator-owned workspace key still writes on REST (200)", st == 200,
              f"status={st} body={json.dumps(body)[:120]}")

        # ── C1/C2 a customer's read_only key is unchanged, both ways ─────────────────────────
        err, payload = rpc(base, cu_ro, "create_object",
                           {"slug": f"cust_hacked_{stamp}", "name": "cust hacked"})
        text = json.dumps(payload)
        check("C1 customer read_only key: MCP write still refused (unchanged)",
              err and payload.get("error") == "forbidden" and MCP_REFUSAL_TAIL in text,
              f"isError={err} payload={text[:160]}")
        err, payload = rpc(base, cu_ro, "list_objects", {})
        st, body = http("GET", f"{base}/api/objects", None, cu_ro)
        check("C2 customer read_only key still reads over MCP and REST (unchanged)",
              not err and st == 200, f"mcp_isError={err} rest_status={st}")

        # ── the binary the proof ran against ─────────────────────────────────────────────────
        sha_out = subprocess.run(["docker", "exec", args.container, "sha256sum", "/app/crm-swift"],
                                 capture_output=True, text=True).stdout.split()
        sha = sha_out[0] if sha_out else "(unavailable)"
        print(f"binary sha256 (/app/crm-swift in {args.container}) = {sha}")

    finally:
        if tenant and not args.keep:
            db.q(f"DELETE FROM custom_records WHERE tenant_id = '{tenant}'")
            db.q("DELETE FROM custom_fields WHERE object_id IN "
                 f"(SELECT id FROM custom_objects WHERE tenant_id = '{tenant}')")
            db.q(f"DELETE FROM custom_objects WHERE tenant_id = '{tenant}'")
            db.q(f"DELETE FROM personal_api_keys WHERE tenant_id = '{tenant}'")
            db.q(f"DELETE FROM users WHERE tenant_id = '{tenant}'")
            db.q(f"DELETE FROM tenants WHERE id = '{tenant}'")
            left_tenant = db.one(f"SELECT count(*) FROM tenants WHERE id = '{tenant}'")
            left_users = db.one(f"SELECT count(*) FROM users WHERE tenant_id = '{tenant}'")
            left_keys = db.one(f"SELECT count(*) FROM personal_api_keys WHERE tenant_id = '{tenant}'")
            left_objects = db.one(f"SELECT count(*) FROM custom_objects WHERE tenant_id = '{tenant}'")
            now_tenants = db.one("SELECT count(*) FROM tenants")
            now_keys = db.one("SELECT count(*) FROM personal_api_keys")
            now_objects = db.one("SELECT count(*) FROM custom_objects")
            check("Z1 the probe workspace left no tenant, user, key or object row",
                  left_tenant == "0" and left_users == "0" and left_keys == "0" and left_objects == "0",
                  f"tenant={left_tenant} users={left_users} keys={left_keys} objects={left_objects}")
            check("Z2 counters are back where they started",
                  now_tenants == base_tenants and now_keys == base_keys and now_objects == base_objects,
                  f"tenants {base_tenants}->{now_tenants}, keys {base_keys}->{now_keys}, "
                  f"objects {base_objects}->{now_objects}")

    print(f"\n{len(PASS)} PASS / {len(FAIL)} FAIL")
    if FAIL:
        print("FAILED: " + "; ".join(FAIL))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
