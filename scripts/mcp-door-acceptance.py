#!/usr/bin/env python3
"""mcp-door-acceptance — the CoreSwift MCP port's app-specific proof, over real HTTP.

Checks (each printed PASS/FAIL with the number that decided it):

  1  workspace A sees its OWN contacts, workspace B sees its own — the two counts differ
  2  A's contact id asked for with B's key  -> not_found (never forbidden)
  3  the same id with A's key               -> the row, with A's data
  4  a read-only key reads (contacts) and is REFUSED on a Custom Objects write (create_object)
  4b the crate's ROLE FILTER: a read-only key's `tools/list` is exactly the 15 read-only tools and
     its raw bytes carry NO write tool name (mcp-door 0.1.2), while a workspace key still sees 23
  5  a revoked key                          -> 401
  6  a platform admin's key                 -> authenticated, protocol-era discovery answers
  7  the shared protocol probe               -> 14/14 (python3 <crate>/scripts/door-contract.py)
  8  the audit file: mode 0600, one line per call, and ZERO token strings in it

Fixtures are two PROBE workspaces (`probe_harness = true`, `probe-mcp-*`) that are deleted at the
end, cascade and all. Nothing touches a customer: no existing tenant id is ever used as a fixture,
and only the keystore/probe tenants are written.

    python3 scripts/mcp-door-acceptance.py                       # 127.0.0.1:8084, container coreswiftcrm
    python3 scripts/mcp-door-acceptance.py --base https://coreswiftcrm.com --container coreswiftcrm
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

PASS: list[str] = []
FAIL: list[str] = []


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
        m = re.match(r"postgres://([^:]+):([^@]+)@[^/]+/(\w+)", url)
        if not m:
            raise SystemExit("cannot parse DATABASE_URL from the app container")
        self.user, self.password, self.name = m.group(1), m.group(2), m.group(3)
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
        rows = []
        for line in out.stdout.splitlines():
            if line.strip():
                rows.append(line.split("\x1f"))
        return rows

    def one(self, sql: str) -> str:
        rows = self.q(sql)
        return rows[0][0] if rows else ""


def http(method: str, url: str, body=None, token: str | None = None) -> tuple[int, dict]:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", "coreswift-mcp-acceptance/1")
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


def rpc(base: str, token: str | None, method: str, params=None, ident: int = 1) -> tuple[int, dict]:
    payload = {"jsonrpc": "2.0", "id": ident, "method": method}
    if params is not None:
        payload["params"] = params
    return http("POST", f"{base}/api/mcp", payload, token)


def rpc_raw(base: str, token: str | None, method: str, params=None, ident: int = 1) -> tuple[int, str]:
    """One JSON-RPC call, returning the RAW response text.

    The role-filter proof is a claim about BYTES, not about what this script can parse: a write
    tool's name anywhere in a read-only key's `tools/list` response means the list lies to the
    caller, whether or not it is inside a `tools` entry.
    """
    payload = {"jsonrpc": "2.0", "id": ident, "method": method}
    if params is not None:
        payload["params"] = params
    data = json.dumps(payload).encode()
    req = urllib.request.Request(f"{base}/api/mcp", data=data, method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", "coreswift-mcp-door-acceptance/1")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def tool_names(raw: str) -> list[str]:
    try:
        body = json.loads(raw)
    except ValueError:
        return []
    return [t.get("name", "") for t in (((body.get("result") or {}).get("tools")) or [])]



def tool(base: str, token: str, name: str, args: dict) -> tuple[bool, dict]:
    """One tools/call. Returns (is_error, payload)."""
    st, body = rpc(base, token, "tools/call", {"name": name, "arguments": args})
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


def audit_lines(path: str) -> int:
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return sum(1 for line in fh if line.strip())


def mint_key(db: Db, tenant: str, role: str, user: str | None, name: str,
             active: bool = True) -> tuple[str, str]:
    """Mint one probe key straight into the DB (the door is what is under test, not the mint API).

    Returns `(token, id)`. The id matters for the ADMIN key: it hangs off the real operator
    workspace, not a probe one, so deleting the probe workspaces does NOT take it with them —
    without the id in the cleanup every run leaked an active `probe-admin` key onto that
    workspace (measured 2026-11-24: eight of them, eight of the ten slots in migration 207's
    active-key cap, before kanban t_8888093d swept them).
    """
    token = "csk_" + uuid.uuid4().hex + uuid.uuid4().hex
    digest = hashlib.sha256(token.encode()).hexdigest()
    user_sql = f"'{user}'" if user else "NULL"
    key_id = db.one(
        "INSERT INTO personal_api_keys (tenant_id, user_id, name, key_hash, key_prefix, is_active, api_role) "
        f"VALUES ('{tenant}', {user_sql}, '{name}', '{digest}', '{token[:12]}', "
        f"{'true' if active else 'false'}, '{role}') RETURNING id"
    )
    if not key_id:
        raise SystemExit("could not mint the probe key")
    return token, key_id


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    ap.add_argument("--container", default="coreswiftcrm")
    ap.add_argument("--db-container", default="swift-postgres")
    ap.add_argument("--crate", default="/opt/swift/mcp-door")
    ap.add_argument("--audit", default="/opt/swift/coreswiftcrm-data/mcp-audit.log")
    ap.add_argument("--keep", action="store_true", help="leave the probe workspaces behind")
    args = ap.parse_args()
    base = args.base.rstrip("/")
    db = Db(args.container, args.db_container)
    stamp = str(int(time.time()))

    print(f"== CoreSwift MCP door acceptance @ {base} ({stamp})")
    status, _ = http("GET", f"{base}/api/ready")
    if status != 200:
        print(f"FAIL  app is not ready: /api/ready answered {status}")
        return 1

    a = db.one(
        "INSERT INTO tenants (name, slug, is_active, probe_harness) "
        f"VALUES ('probe-mcp-a-{stamp}', 'probe-mcp-a-{stamp}', true, 'true') RETURNING id"
    )
    b = db.one(
        "INSERT INTO tenants (name, slug, is_active, probe_harness) "
        f"VALUES ('probe-mcp-b-{stamp}', 'probe-mcp-b-{stamp}', true, 'true') RETURNING id"
    )
    # Every probe key needs a LIVE creator: since kanban t_fbd8fc03 the door refuses a key whose
    # creator row is missing, disabled or closed (`creator_live` in src/mcp_door/mod.rs), so a key
    # minted with user_id NULL authenticates as 401 and would make this whole harness lie.
    ua = db.one(
        "INSERT INTO users (tenant_id, email, password_hash, name) "
        f"VALUES ('{a}', 'probe-a-{stamp}@probe.test', 'probe-harness', 'Probe A') RETURNING id"
    ) if a else ""
    ub = db.one(
        "INSERT INTO users (tenant_id, email, password_hash, name) "
        f"VALUES ('{b}', 'probe-b-{stamp}@probe.test', 'probe-harness', 'Probe B') RETURNING id"
    ) if b else ""
    if not (a and b and ua and ub):
        print("FAIL  could not create the probe workspaces")
        return 1
    admin_key_id = ""
    kro = ""

    try:
        ka, _ = mint_key(db, a, "workspace", ua, f"probe-workspace-a-{stamp}")
        kb, _ = mint_key(db, b, "workspace", ub, f"probe-workspace-b-{stamp}")
        kro, _ = mint_key(db, a, "read_only", ua, f"probe-read-only-{stamp}")
        kdead, _ = mint_key(db, a, "workspace", ua, f"probe-revoked-{stamp}", active=False)
        admin_user = db.one("SELECT id||'|'||tenant_id FROM users WHERE is_platform_admin LIMIT 1")
        admin_id, admin_tenant = admin_user.split("|")
        # The admin key lives on the REAL operator workspace, so its cleanup is explicit below.
        kadmin, admin_key_id = mint_key(db, admin_tenant, "workspace", admin_id, f"probe-admin-{stamp}")

        # ── fixtures: A has 2 contacts + a deal/ticket/line/message, B has 1 contact
        c1 = db.one(
            "INSERT INTO contacts (tenant_id, first_name, last_name, email, company, notes) "
            f"VALUES ('{a}', 'Probe', 'Alpha', 'alpha@probe.test', 'Alpha Co', 'probe note') RETURNING id"
        )
        db.q(
            "INSERT INTO contacts (tenant_id, first_name, last_name, email) "
            f"VALUES ('{a}', 'Probe', 'Beta', 'beta@probe.test')"
        )
        cb = db.one(
            "INSERT INTO contacts (tenant_id, first_name, last_name, email) "
            f"VALUES ('{b}', 'Probe', 'Gamma', 'gamma@probe.test') RETURNING id"
        )
        db.q(f"INSERT INTO companies (tenant_id, name) VALUES ('{a}', 'Probe Alpha Co')")
        pipe = db.one(
            f"INSERT INTO pipelines (tenant_id, name, is_default, is_active) VALUES ('{a}', 'Probe pipe', true, true) RETURNING id"
        )
        stage = db.one(
            "INSERT INTO pipeline_stages (pipeline_id, name, position, probability) "
            f"VALUES ('{pipe}', 'Probe stage', 0, 50) RETURNING id"
        )
        db.q(
            "INSERT INTO opportunities (tenant_id, pipeline_id, stage_id, contact_id, name, value, currency, probability) "
            f"VALUES ('{a}', '{pipe}', '{stage}', '{c1}', 'Probe deal', 4200.00, 'USD', 60)"
        )
        db.q(
            "INSERT INTO tickets (tenant_id, subject, description, status, priority, source) "
            f"VALUES ('{a}', 'Probe ticket', 'probe description', 'open', 'medium', 'api')"
        )
        plan = db.one("SELECT id FROM plans ORDER BY created_at LIMIT 1")
        if plan:
            db.q(
                "INSERT INTO plan_lines (tenant_id, plan_id, plan_slug, price_monthly, price_yearly, billing_cycle, started_at) "
                f"VALUES ('{a}', '{plan}', 'probe', 29.00, 290.00, 'monthly', now())"
            )
        db.q(
            "INSERT INTO private_email_messages (tenant_id, address, folder, direction, from_address, subject, body_plain, received_at) "
            f"VALUES ('{a}', 'probe@probe.test', 'inbox', 'inbound', 'sender@probe.test', 'Probe mail', 'probe body', now())"
        )

        # 1 + 2 + 3 ── scoping
        err_a, list_a = tool(base, ka, "coreswift_list_contacts", {})
        err_b, list_b = tool(base, kb, "coreswift_list_contacts", {})
        count_a = list_a.get("count", -1) if not err_a else -1
        count_b = list_b.get("count", -1) if not err_b else -1
        check("workspace A lists its own contacts", not err_a and count_a == 2, f"count={count_a}")
        check("workspace B lists its own contacts (different count)", not err_b and count_b == 1,
              f"a={count_a} b={count_b}")
        check("the two workspaces get different counts", count_a != count_b, f"{count_a} vs {count_b}")

        err_foreign, foreign = tool(base, kb, "coreswift_get_contact", {"id": c1})
        check("a foreign contact id answers not_found", err_foreign and foreign.get("error") == "not_found",
              f"{err_foreign} {foreign.get('error')}")
        err_own, own = tool(base, ka, "coreswift_get_contact", {"id": c1})
        check("its owner still reads it", not err_own and own.get("email") == "alpha@probe.test",
              f"{err_own} {own.get('email')}")
        err_own_b, own_b = tool(base, kb, "coreswift_get_contact", {"id": cb})
        check("B reads its own contact", not err_own_b and own_b.get("email") == "gamma@probe.test",
              f"{own_b.get('email')}")

        # the other domain reads answer for A
        for name, args_, key in [
            ("coreswift_list_companies", {}, "count"),
            ("coreswift_list_deals", {}, "count"),
            ("coreswift_get_deal", None, "value"),
            ("coreswift_list_pipelines", {}, "count"),
            ("coreswift_list_tickets", {}, "count"),
            ("coreswift_list_messages", {}, "count"),
            ("coreswift_list_billing", {}, "line_count"),
        ]:
            if name == "coreswift_get_deal":
                _, deals = tool(base, ka, "coreswift_list_deals", {})
                deal_id = (deals.get("items") or [{}])[0].get("id", "")
                err, payload = tool(base, ka, name, {"id": deal_id})
                ok = not err and payload.get("value") == 4200.0 and payload.get("currency") == "USD"
                check(f"{name} returns the deal WITH its currency", ok,
                      f"{payload.get('value')} {payload.get('currency')}")
                continue
            err, payload = tool(base, ka, name, args_)
            check(f"{name} answers", not err and payload.get(key, -1) not in (-1, None),
                  f"{key}={payload.get(key)}")
        _, avail = tool(base, ka, "coreswift_list_availability", {"from": time.strftime("%Y-%m-%d")})
        check("coreswift_list_availability answers a date window", "slot_count" in avail,
              str(avail.get("slot_count")))

        # 4 ── read-only credential
        err_ro, ro = tool(base, kro, "coreswift_list_contacts", {})
        check("a read-only key CAN read", not err_ro and ro.get("count") == 2, f"count={ro.get('count')}")
        err_w, w = tool(base, kro, "create_object", {"slug": "probe_ro", "name": "Probe RO"})
        check("a read-only key is REFUSED on a Custom Objects write", err_w and w.get("error") == "forbidden",
              f"{err_w} {w.get('error')}")
        err_w2, w2 = tool(base, kro, "insert_record", {"object": "probe_ro", "data": {}})
        check("and on a record insert", err_w2, f"{w2.get('error')}")

        # 4b ── the crate's ROLE FILTER (mcp-door 0.1.2, kanban t_46a81465).
        # On the 0.1.0 pin a read-only key was handed all 23 tools and only refused when it called
        # a write: the tool list LIED. The filter is a claim about response BYTES, so measure them.
        ro_floor = sorted({
            # the three Custom Objects tools the registry gates at Role::ReadOnly
            "list_objects", "describe_object", "query_records",
            # the twelve domain reads, each .min_role(Role::ReadOnly)
            "coreswift_list_contacts", "coreswift_get_contact", "coreswift_list_companies",
            "coreswift_get_company", "coreswift_list_deals", "coreswift_get_deal",
            "coreswift_list_pipelines", "coreswift_list_bookings", "coreswift_list_availability",
            "coreswift_list_tickets", "coreswift_list_billing", "coreswift_list_messages",
        })
        write_tools = ["create_object", "archive_object", "add_field", "archive_field",
                       "insert_record", "set_unique_together", "update_record", "delete_record"]
        st_ro, raw_ro = rpc_raw(base, kro, "tools/list")
        ro_names = sorted(tool_names(raw_ro))
        check("a read-only key's tools/list is exactly the read-only floor (15)",
              st_ro == 200 and ro_names == ro_floor,
              f"status={st_ro} n={len(ro_names)} "
              f"extra={sorted(set(ro_names) - set(ro_floor))} "
              f"missing={sorted(set(ro_floor) - set(ro_names))}")
        leaked = [n for n in write_tools if n in raw_ro]
        check("a read-only key's tools/list BYTES carry no write tool name", not leaked,
              "leaked=" + ",".join(leaked))
        st_ws, raw_ws = rpc_raw(base, ka, "tools/list")
        ws_names = set(tool_names(raw_ws))
        check("a workspace key still sees all 23 tools", st_ws == 200 and len(ws_names) == 23,
              f"status={st_ws} n={len(ws_names)}")
        check("a workspace key still sees every write tool", set(write_tools) <= ws_names,
              "missing=" + ",".join(sorted(set(write_tools) - ws_names)))
        err_lo, lo = tool(base, ka, "list_objects", {})
        check("a workspace key's list_objects still answers", not err_lo, str(lo)[:80])

        # 5 ── revoked key
        st_dead, _ = rpc(base, kdead, "ping")
        check("a revoked key answers 401", st_dead == 401, f"status={st_dead}")

        # 6 ── admin + protocol era
        st_admin, disc = rpc(base, kadmin, "server/discover")
        versions = ((disc.get("result") or {}).get("protocolVersions") or [])
        check("a platform admin key authenticates and discovers the protocol", st_admin == 200 and versions,
              f"status={st_admin} versions={versions[:1]}")
        _, tools_list = rpc(base, ka, "tools/list")
        names = [t.get("name") for t in ((tools_list.get("result") or {}).get("tools") or [])]
        check("the door offers 23 tools (11 custom objects + 12 reads)", len(names) == 23, f"n={len(names)}")

        # 7 ── the shared protocol probe. The crate grows checks (14 -> 16 when tools/list became
        # role-filtered), so assert "none failed and most of them ran" instead of a frozen count.
        probe = os.path.join(args.crate, "scripts", "door-contract.py")
        if os.path.exists(probe):
            cmd = [sys.executable, probe, f"{base}/api/mcp",
                   "--workspace-token", ka, "--readonly-token", kro,
                   "--admin-token", kadmin, "--revoked-token", kdead]
            out = subprocess.run(cmd, capture_output=True, text=True)
            passes = len(re.findall(r"^PASS ", out.stdout, re.M))
            fails = len(re.findall(r"^FAIL ", out.stdout, re.M))
            check(f"door-contract.py: every shared check passes ({passes})", fails == 0 and passes >= 14,
                  f"{passes} pass / {fails} fail")
            if fails:
                print(out.stdout[-2000:])
        else:
            check("door-contract.py present", False, probe)

        # 8 ── audit
        if os.path.exists(args.audit):
            mode = oct(os.stat(args.audit).st_mode & 0o777)
            before = audit_lines(args.audit)
            try:
                # Three authenticated calls, one at a time: the delta is the claim being proved.
                for _ in range(3):
                    tool(base, ka, "coreswift_list_contacts", {})
            except SystemExit:
                raise
            after = audit_lines(args.audit)
            text = open(args.audit, "r", encoding="utf-8", errors="replace").read()
            lines = [l for l in text.splitlines() if l.strip()]
            tokens = [t for t in (ka, kb, kro, kdead, kadmin) if t in text]
            check("audit file is 0600", mode == "0o600", mode)
            check("audit writes exactly one line per call", after - before == 3,
                  f"{before}->{after} for 3 calls")
            check("audit contains ZERO token strings", not tokens, f"{len(tokens)} leaked")
            try:
                json.loads(lines[-1])
                check("audit lines are JSON", True)
            except Exception as e:  # noqa: BLE001
                check("audit lines are JSON", False, str(e))
        else:
            check("audit file exists", False, args.audit)

    finally:
        if not args.keep:
            db.q("DELETE FROM tenants WHERE probe_harness = 'true' AND slug LIKE 'probe-mcp-%'")
            leftover = db.q(f"SELECT count(*) FROM tenants WHERE slug LIKE 'probe-mcp-{stamp}-%'")
            print(f"cleanup: probe workspaces remaining = {leftover[0][0] if leftover else '?'}")
            # Teardown is only proven by the CREDENTIAL dying, not by a row disappearing from a
            # list: the read-only probe key must answer 401 on the very next call.
            if kro:
                st_gone, _ = rpc(base, kro, "tools/list")
                check("a torn-down probe key answers 401", st_gone == 401, f"status={st_gone}")
        # The admin key is NOT a probe-tenant row: cascade does not reach it, so delete it here —
        # an un-cleaned one is an active credential on the operator's own workspace and one of
        # the ten slots migration 207 caps.
        if admin_key_id:
            db.q(f"DELETE FROM personal_api_keys WHERE id = '{admin_key_id}'")
            still = db.one(f"SELECT count(*) FROM personal_api_keys WHERE id = '{admin_key_id}'")
            print(f"cleanup: admin probe key removed = {'yes' if str(still) == '0' else 'NO'}")
        if not args.keep:
            keys_left = db.one(
                "SELECT count(*) FROM personal_api_keys "
                f"WHERE name LIKE 'probe-%-{stamp}'"
            )

            check("no probe key residue", str(keys_left) == "0", f"{keys_left} left")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    for name in FAIL:
        print(f"  FAILED: {name}")
    return 0 if not FAIL else 1


if __name__ == "__main__":
    sys.exit(main())
