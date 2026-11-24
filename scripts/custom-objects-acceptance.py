#!/usr/bin/env python3
"""custom-objects-acceptance — the Phase 1 proof gate, driven over real HTTP.

Eight checks, one per line of the spec's §8, against a RUNNING deployment:

  1. MCP create_object + add_field      -> rows in custom_objects / custom_fields
  2. REST insert with a workspace key   -> the record row carries that workspace
  3. Console session read               -> the same record, same id
  4. A second workspace's key           -> 404 on the first workspace's object
  5. archive_field                      -> the value is still in custom_records.data
  6. Unknown field key on insert        -> 400 and NO row written
  7. unique_value violation             -> 409 and NO duplicate row
  8. Audit rows                         -> one per schema change

It creates two throwaway tenants, two issued keys and one session token, then DELETES the tenants
(a cascade takes the objects, fields, records and audit rows with them) and asserts the baseline
came back. Nothing touches a real customer.

    python3 scripts/custom-objects-acceptance.py                 # 127.0.0.1:8084, container coreswiftcrm
    python3 scripts/custom-objects-acceptance.py --base https://coreswiftcrm.com --container coreswiftcrm
"""

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

PASS, FAIL = [], []


def check(name, ok, detail=""):
    (PASS if ok else FAIL).append(name)
    print(("PASS  " if ok else "FAIL  ") + name + (f"  [{detail}]" if detail and not ok else ""))


def http(method, url, body=None, token=None) -> "tuple[int, dict]":
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    # A bare urllib request is refused by some edges; an explicit UA keeps the transport honest.
    req.add_header("User-Agent", "coreswift-custom-objects-acceptance/1")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read().decode()
            return r.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw) if raw else {}
        except ValueError:
            return e.code, {"raw": raw}


class Db:
    def __init__(self, container):
        self.url = subprocess.run(
            ["docker", "exec", container, "printenv", "DATABASE_URL"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        self.secret = subprocess.run(
            ["docker", "exec", container, "printenv", "JWT_SECRET"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()

    def q(self, sql):
        out = subprocess.run(
            ["psql", self.url, "-tAc", sql, "-v", "ON_ERROR_STOP=1"],
            capture_output=True, text=True,
        )
        if out.returncode != 0:
            raise RuntimeError(f"psql failed: {out.stderr.strip()}\n{sql}")
        return out.stdout.strip()



def b64(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def jwt(secret, sub, aid, role="owner", ttl=600):
    now = int(time.time())
    head = b64(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    body = b64(json.dumps({
        "sub": sub, "aid": aid, "role": role, "iat": now, "exp": now + ttl,
        "iss": "coreswift", "aud": "coreswift-api",
    }, separators=(",", ":")).encode())
    sig = b64(hmac.new(secret.encode(), f"{head}.{body}".encode(), hashlib.sha256).digest())
    return f"{head}.{body}.{sig}"


def mcp(base, token, tool, args):
    status, body = http("POST", f"{base}/api/mcp", {
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": args},
    }, token)
    return status, body


def tool_out(body):
    """The tool's JSON payload (it travels as one text content block)."""
    try:
        return json.loads(body["result"]["content"][0]["text"])
    except (KeyError, IndexError, TypeError, ValueError):
        return {}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    ap.add_argument("--container", default="coreswiftcrm")
    args = ap.parse_args()
    base = args.base.rstrip("/")
    db = Db(args.container)
    stamp = str(int(time.time()))[-6:]
    tenants = []

    def new_tenant(tag):
        tid = db.q(
            "INSERT INTO tenants (name, slug, probe_harness) "
            f"VALUES ('Probe Custom Objects {tag}', 'probe-co-{tag}-{stamp}', "
            "'custom-objects-acceptance') RETURNING id"
        ).splitlines()[0]
        tenants.append(tid)
        return tid

    def new_key(tid, tag):
        key = "csk_" + hashlib.sha256(f"{tag}{stamp}{time.time()}".encode()).hexdigest()
        db.q(
            "INSERT INTO personal_api_keys (tenant_id, name, key_hash, key_prefix) VALUES "
            f"('{tid}', 'acceptance probe', '{hashlib.sha256(key.encode()).hexdigest()}', 'csk_probe')"
        )
        return key

    def new_user(tid, tag):
        # A REAL users row: the canonical boundary refuses a token whose `sub` names no live,
        # enabled user, so a probe session has to be a session of an actual account.
        return db.q(
            "INSERT INTO users (tenant_id, email, password_hash, name) "
            f"VALUES ('{tid}', 'probe-co-{tag}-{stamp}@example.test', 'x', 'Probe {tag}') "
            "RETURNING id"
        ).splitlines()[0]

    try:
        a = new_tenant("a")
        b = new_tenant("b")
        key_a, key_b = new_key(a, "a"), new_key(b, "b")
        user_a = new_user(a, "a")
        session_a = jwt(db.secret, user_a, a)

        # ── 1. MCP creates the object and its fields ────────────────────────────────────────────
        st, body = mcp(base, key_a, "create_object", {"slug": "orders", "name": "Orders " + stamp})
        obj = tool_out(body)
        created = st == 200 and obj.get("object", {}).get("slug") == "orders"
        rows = db.q(f"SELECT count(*) FROM custom_objects WHERE tenant_id='{a}' AND slug='orders'")
        check("1a MCP create_object answers and the object row exists", created and rows == "1",
              f"status={st} rows={rows} body={json.dumps(body)[:160]}")

        fields = [
            {"object": "orders", "key": "email", "label": "Email", "kind": "text",
             "required": True, "unique": True},
            {"object": "orders", "key": "amount", "label": "Amount", "kind": "number"},
            {"object": "orders", "key": "status", "label": "Status", "kind": "enum",
             "options": ["open", "paid"]},
            {"object": "orders", "key": "notes", "label": "Notes", "kind": "text"},
        ]
        ok = True
        for f in fields:
            st, body = mcp(base, key_a, "add_field", f)
            ok = ok and st == 200 and not body.get("result", {}).get("isError")
        fcount = db.q(
            "SELECT count(*) FROM custom_fields f JOIN custom_objects o ON o.id=f.object_id "
            f"WHERE o.tenant_id='{a}' AND o.slug='orders' AND f.archived_at IS NULL"
        )
        check("1b MCP add_field writes every field", ok and fcount == str(len(fields)),
              f"ok={ok} fields={fcount}")

        # ── 2. REST insert with the workspace key ───────────────────────────────────────────────
        st, body = http("POST", f"{base}/api/objects/orders/records",
                        {"email": "first@example.test", "amount": 10, "status": "open", "notes": "keep me"},
                        key_a)
        rid = body.get("id")
        tenant_of = db.q(f"SELECT tenant_id FROM custom_records WHERE id='{rid}'") if rid else ""
        check("2 REST insert with the key writes a row owned by that workspace",
              st == 200 and rid and tenant_of == a, f"status={st} tenant={tenant_of} body={json.dumps(body)[:160]}")

        # ── 3. A console session reads the same row ─────────────────────────────────────────────
        st, body = http("GET", f"{base}/api/objects/orders/records?limit=50", None, session_a)
        ids = [r["id"] for r in body.get("records", [])]
        check("3 console session read returns the same record",
              st == 200 and rid in ids, f"status={st} ids={ids[:3]}")

        # ── 4. The other workspace's key gets 404, not 403 ──────────────────────────────────────
        st, body = http("GET", f"{base}/api/objects/orders/records", None, key_b)
        st2, body2 = mcp(base, key_b, "describe_object", {"object": "orders"})
        refused = st == 404 and st2 == 200 and body2.get("result", {}).get("isError") is True
        check("4 a second workspace's key gets 404 on the first workspace's object",
              refused, f"rest={st} mcp={body2.get('result', {}).get('status')}")

        # ── 5. Archiving a field keeps its data ─────────────────────────────────────────────────
        st, body = mcp(base, key_a, "archive_field", {"object": "orders", "key": "notes"})
        kept = db.q(f"SELECT data->>'notes' FROM custom_records WHERE id='{rid}'")
        archived = db.q(
            "SELECT count(*) FROM custom_fields f JOIN custom_objects o ON o.id=f.object_id "
            f"WHERE o.tenant_id='{a}' AND f.key='notes' AND f.archived_at IS NOT NULL"
        )
        check("5 archive_field keeps the stored value", st == 200 and kept == "keep me" and archived == "1",
              f"status={st} data={kept!r} archived={archived}")

        # 5b. A patch on a record that still carries an archived field's value must NOT be refused: the
        # value is not writable any more, but it is not an error either. (This is the arm that a naive
        # "validate the merged row against live fields" check gets wrong.)
        st, body = http("PATCH", f"{base}/api/objects/orders/records/{rid}", {"amount": 12}, key_a)
        kept2 = db.q(f"SELECT data->>'notes' FROM custom_records WHERE id='{rid}'")
        check("5b PATCH still works while the record carries an archived field",
              st == 200 and kept2 == "keep me", f"status={st} body={json.dumps(body)[:160]}")

        # ── 6. An unknown field key is refused and nothing is written ───────────────────────────
        before = db.q(f"SELECT count(*) FROM custom_records WHERE tenant_id='{a}'")
        st, body = http("POST", f"{base}/api/objects/orders/records",
                        {"email": "second@example.test", "bogus_key": "x"}, key_a)
        after = db.q(f"SELECT count(*) FROM custom_records WHERE tenant_id='{a}'")
        check("6 unknown field key -> 400 and no row", st == 400 and before == after,
              f"status={st} before={before} after={after} body={json.dumps(body)[:160]}")

        # ── 7. unique_value violation -> 409, no duplicate ──────────────────────────────────────
        st, body = http("POST", f"{base}/api/objects/orders/records",
                        {"email": "first@example.test", "amount": 11, "status": "paid"}, key_a)
        dupes = db.q(
            "SELECT count(*) FROM custom_records WHERE tenant_id='%s' AND deleted_at IS NULL "
            "AND data->>'email'='first@example.test'" % a
        )
        check("7 unique_value violation -> 409 and no duplicate row", st == 409 and dupes == "1",
              f"status={st} dupes={dupes} body={json.dumps(body)[:160]}")

        # ── 8. Audit rows for every schema change ───────────────────────────────────────────────
        st, body = http("GET", f"{base}/api/objects/orders/audit", None, session_a)
        actions = sorted({r["action"] for r in body.get("audit", [])})
        db_schema_changes = db.q(
            "SELECT count(*) FROM custom_object_audit WHERE tenant_id='%s' AND action IN "
            "('object_created','field_added','field_archived')" % a
        )
        check("8 audit rows exist for every schema change",
              st == 200 and {"object_created", "field_added", "field_archived"} <= set(actions)
              and db_schema_changes == "6",  # 1 object_created + 4 field_added + 1 field_archived
              f"status={st} actions={actions} db={db_schema_changes}")

        # ── 9. §6a/§6b (kanban t_933dd588): you can undo what you created ────────────────────────
        # 9a. A second workspace cannot archive the first workspace's object — 404, and the object
        #     is still live afterwards (a refusal that silently archived would be worse than a 500).
        st, body = http("DELETE", f"{base}/api/objects/orders", None, key_b)
        still_live = db.q(f"SELECT count(*) FROM custom_objects WHERE tenant_id='{a}' AND slug='orders' AND archived_at IS NULL")
        check("9a a second workspace's key cannot archive the object (404, object untouched)",
              st == 404 and still_live == "1", f"status={st} live={still_live} body={json.dumps(body)[:160]}")

        # 9b. hard=true is an ACTUAL delete: the row count drops by exactly 1 and the siblings stay.
        r2 = http("POST", f"{base}/api/objects/orders/records",
                  {"email": "purge-me@example.test", "amount": 20, "status": "open"}, key_a)[1].get("id")
        r3 = http("POST", f"{base}/api/objects/orders/records",
                  {"email": "soft-me@example.test", "amount": 30, "status": "open"}, key_a)[1].get("id")
        rows_before = int(db.q(f"SELECT count(*) FROM custom_records WHERE tenant_id='{a}'"))
        # The wrong workspace cannot reach the record at all — 404 and the row is untouched.
        st_b, _ = http("DELETE", f"{base}/api/objects/orders/records/{r2}?hard=true", None, key_b)
        r2_intact = db.q(f"SELECT count(*) FROM custom_records WHERE id='{r2}'")
        check("9b0 a second workspace's key cannot hard-delete the record (404, row intact)",
              st_b == 404 and r2_intact == "1", f"status={st_b} row={r2_intact}")
        st, body = http("DELETE", f"{base}/api/objects/orders/records/{r2}?hard=true", None, key_a)
        rows_after = int(db.q(f"SELECT count(*) FROM custom_records WHERE tenant_id='{a}'"))
        gone = db.q(f"SELECT count(*) FROM custom_records WHERE id='{r2}'")
        siblings = db.q(f"SELECT count(*) FROM custom_records WHERE tenant_id='{a}' AND id IN ('{rid}','{r3}')")
        purged_audit = db.q(
            f"SELECT count(*) FROM custom_object_audit WHERE tenant_id='{a}' AND action='record_purged' "
            f"AND detail->>'id'='{r2}'"
        )
        purged_detail = db.q(
            f"SELECT detail::text FROM custom_object_audit "
            f"WHERE tenant_id='{a}' AND action='record_purged' ORDER BY id DESC LIMIT 1"
        )
        purged_keys = ",".join(sorted(json.loads(purged_detail).keys())) if purged_detail else ""
        check("9b hard=true really deletes one row (siblings stay, audit keeps the id only)",
              st == 200 and body.get("hard") is True and rows_before - rows_after == 1
              and gone == "0" and siblings == "2" and purged_audit == "1" and purged_keys == "id",
              f"status={st} rows={rows_before}->{rows_after} gone={gone} sibs={siblings} "
              f"audit={purged_audit} keys={purged_keys} body={json.dumps(body)[:160]}")

        # 9c. The default is STILL a soft delete: the row survives with deleted_at set.
        st, body = http("DELETE", f"{base}/api/objects/orders/records/{r3}", None, key_a)
        soft_row = db.q(f"SELECT (deleted_at IS NOT NULL)::text FROM custom_records WHERE id='{r3}'")
        live_now = db.q(f"SELECT count(*) FROM custom_records WHERE tenant_id='{a}' AND deleted_at IS NULL")
        check("9c the default delete is still soft (row survives with deleted_at)",
              st == 200 and body.get("hard") is False and soft_row == "true" and live_now == "1",
              f"status={st} soft={soft_row} live={live_now} body={json.dumps(body)[:160]}")

        # 9d. Archiving the OBJECT: gone from list/describe/every door; fields archived with it; the
        #     records are retained (rows + data still in custom_records).
        recs_before = db.q(f"SELECT count(*) FROM custom_records WHERE tenant_id='{a}'")
        st, body = http("DELETE", f"{base}/api/objects/orders", None, key_a)
        listed = [o["slug"] for o in http("GET", f"{base}/api/objects", None, key_a)[1].get("objects", [])]
        st_schema, _ = http("GET", f"{base}/api/objects/orders/schema", None, key_a)
        st_records, _ = http("GET", f"{base}/api/objects/orders/records", None, key_a)
        st_mcp, mcp_body = mcp(base, key_a, "describe_object", {"object": "orders"})
        obj_archived = db.q(f"SELECT (archived_at IS NOT NULL)::text FROM custom_objects WHERE tenant_id='{a}' AND slug='orders'")
        fields_live = db.q(
            "SELECT count(*) FROM custom_fields f JOIN custom_objects o ON o.id=f.object_id "
            f"WHERE o.tenant_id='{a}' AND o.slug='orders' AND f.archived_at IS NULL"
        )
        recs_after = db.q(f"SELECT count(*) FROM custom_records WHERE tenant_id='{a}'")
        check("9d archiving an object hides it everywhere and keeps every record row",
              st == 200 and body.get("archived") is True and body.get("already_archived") is False
              and "orders" not in listed and st_schema == 404 and st_records == 404
              and mcp_body.get("result", {}).get("isError") is True
              and mcp_body.get("result", {}).get("status") == 404
              and obj_archived == "true" and fields_live == "0" and recs_before == recs_after == "2",
              f"status={st} listed={listed} schema={st_schema} records={st_records} "
              f"mcp={mcp_body.get('result', {}).get('status')} archived={obj_archived} "
              f"fields_live={fields_live} recs={recs_before}->{recs_after} body={json.dumps(body)[:160]}")

        # 9e. Idempotent: a retry answers the same shape and changes nothing (no second archive).
        st, body = http("DELETE", f"{base}/api/objects/orders", None, key_a)
        archived_audit = db.q(
            f"SELECT count(*) FROM custom_object_audit WHERE tenant_id='{a}' AND action='object_archived'"
        )
        check("9e archiving twice is idempotent and each call leaves one audit row",
              st == 200 and body.get("archived") is True and body.get("already_archived") is True
              and body.get("fields_archived") == 0 and archived_audit == "2",
              f"status={st} audit={archived_audit} body={json.dumps(body)[:160]}")

        # 9f. The MCP door does both too: hard delete_record and archive_object.
        st, body = mcp(base, key_a, "create_object", {"slug": "cards", "name": "Cards " + stamp})
        mcp(base, key_a, "add_field", {"object": "cards", "key": "email", "kind": "text"})
        c1 = tool_out(mcp(base, key_a, "insert_record", {"object": "cards", "data": {"email": "c1@example.test"}})[1]).get("id")
        c2 = tool_out(mcp(base, key_a, "insert_record", {"object": "cards", "data": {"email": "c2@example.test"}})[1]).get("id")
        st_del, body_del = mcp(base, key_a, "delete_record", {"object": "cards", "id": c1, "hard": True})
        c_gone = db.q(f"SELECT count(*) FROM custom_records WHERE id='{c1}'")
        st_arch, body_arch = mcp(base, key_a, "archive_object", {"object": "cards"})
        cards_listed = [o["slug"] for o in tool_out(mcp(base, key_a, "list_objects", {})[1]).get("objects", [])]
        c_kept = db.q(
            "SELECT count(*) FROM custom_records r JOIN custom_objects o ON o.id=r.object_id "
            f"WHERE o.tenant_id='{a}' AND o.slug='cards'"
        )
        mcp_purge_audit = db.q(
            f"SELECT count(*) FROM custom_object_audit WHERE tenant_id='{a}' AND action='record_purged' "
            f"AND detail->>'id'='{c1}'"
        )
        check("9f MCP delete_record {hard:true} deletes and archive_object archives (records kept)",
              st_del == 200 and tool_out(body_del).get("hard") is True and c_gone == "0"
              and st_arch == 200 and tool_out(body_arch).get("archived") is True
              and "cards" not in cards_listed and c_kept == "1" and mcp_purge_audit == "1"
              and c1 and c2,
              f"del={st_del} gone={c_gone} arch={st_arch} listed={cards_listed} kept={c_kept}")

        # 9g. The slug is free again after archiving (a prototype shape is not permanent clutter).
        st, body = http("POST", f"{base}/api/objects", {"slug": "orders", "name": "Orders again"}, key_a)
        old_id = db.q(f"SELECT id FROM custom_objects WHERE tenant_id='{a}' AND slug='orders' AND archived_at IS NOT NULL")
        new_id = (body.get("object") or {}).get("id")
        check("9g the archived slug can be created again (unique among live objects)",
              st == 200 and new_id and old_id and new_id != old_id,
              f"status={st} old={old_id} new={new_id} body={json.dumps(body)[:160]}")

        # 9h. NO machine path reaches account-level erase. Tools are objects/fields/records only, an
        #     erase tool name is unknown, and the human exit door refuses an API key.
        st, body = http("POST", f"{base}/api/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/list"}, key_a)
        names = [t["name"] for t in body.get("result", {}).get("tools", [])]
        bad = [n for n in names if any(f in n.lower() for f in ("erase", "purge", "close", "delete_account"))]
        st_unknown, body_unknown = mcp(base, key_a, "erase_data", {})
        unknown_code = body_unknown.get("error", {}).get("code")
        st_exit, _ = http("POST", f"{base}/api/auth/erase-data", {"password": "x", "confirm": "DELETE"}, key_a)
        check("9h no API-key/MCP path can reach account-level erase",
              len(names) == 10 and "archive_object" in names and not bad
              and st_unknown == 200 and unknown_code == -32601 and st_exit == 401,
              f"names={names} bad={bad} unknown={unknown_code} exitdoor={st_exit}")


        st, _ = http("GET", f"{base}/api/objects", None, None)
        check("boundary: no credential -> 401", st == 401, f"status={st}")
        st, _ = http("GET", f"{base}/api/objects", None, "csk_" + "0" * 64)
        check("boundary: an unknown key -> 401", st == 401, f"status={st}")

    finally:
        for t in tenants:
            db.q(f"DELETE FROM tenants WHERE id='{t}'")
        left = db.q(
            "SELECT (SELECT count(*) FROM custom_objects WHERE tenant_id IN (%s)) + "
            "(SELECT count(*) FROM custom_records WHERE tenant_id IN (%s)) + "
            "(SELECT count(*) FROM custom_object_audit WHERE tenant_id IN (%s))"
            % (",".join(f"'{t}'" for t in tenants) or "NULL",
               ",".join(f"'{t}'" for t in tenants) or "NULL",
               ",".join(f"'{t}'" for t in tenants) or "NULL")
        ) if tenants else "0"
        check("cleanup: probe tenants and their rows are gone", left == "0", f"rows left={left}")

    total = len(PASS) + len(FAIL)
    print(f"\n{len(PASS)}/{total} checks passed")
    return 0 if not FAIL else 1


if __name__ == "__main__":
    sys.exit(main())
