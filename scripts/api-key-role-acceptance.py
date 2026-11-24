#!/usr/bin/env python3
"""api-key-role-acceptance — a `read_only` issued key is read-only on the REST surface too.

Kanban t_ce51cf34. Migration 210 gave `personal_api_keys` an `api_role`, and the MCP door refused a
`read_only` key on every write tool — but `/api/objects/**` and the write arms of `/api/external/**`
never consulted the role, so the same key created, updated and deleted over REST. This harness proves
the REST half, on a RUNNING deployment, over real HTTP, against a throwaway workspace:

  A. the key READS: GET /api/objects, /api/objects/<slug>/schema|records|audit, /api/external/lists
  B. every WRITE arm answers 403 with the sentence "this key is read-only — use a workspace key to
     write" (`custom_objects` create/patch/archive, field add/patch/archive, record
     insert/update/delete, contacts, deals, deal stage/status, the data-mapping intake)
  C. nothing landed: the object's name, field label, record body and the workspace's contact/deal
     counts are exactly what the fixtures set
  D. the MCP door is UNCHANGED: a read tool answers, a write tool is refused (`forbidden`)
  E. a `workspace` key is unaffected: same writes answer 200

Fixtures are one PROBE workspace (`probe_harness = 'api-key-role-acceptance'`, slug `probe-ro-*`)
deleted at the end, cascade and all; the run asserts zero residue.

    python3 scripts/api-key-role-acceptance.py                  # 127.0.0.1:8084, container coreswiftcrm
    python3 scripts/api-key-role-acceptance.py --base https://coreswiftcrm.com
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

# The exact sentence the boundary answers (src/auth/boundary.rs::READ_ONLY_REFUSAL).
SENTENCE = "this key is read-only"
SENTENCE_TAIL = "use a workspace key to write"


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
    req.add_header("User-Agent", "coreswift-api-key-role-acceptance/1")
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


def refused_sentence(status: int, body: dict) -> bool:
    text = json.dumps(body)
    return status == 403 and SENTENCE in text and SENTENCE_TAIL in text


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


def mint_key(db: Db, tenant: str, role: str, name: str) -> tuple[str, str]:
    token = "csk_" + uuid.uuid4().hex + uuid.uuid4().hex
    digest = hashlib.sha256(token.encode()).hexdigest()
    key_id = db.one(
        "INSERT INTO personal_api_keys (tenant_id, name, key_hash, key_prefix, is_active, api_role) "
        f"VALUES ('{tenant}', '{name}', '{digest}', '{token[:12]}', true, '{role}') RETURNING id"
    )
    if not key_id:
        raise SystemExit("could not mint the probe key")
    return token, key_id


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

    print(f"== CoreSwift read_only-key REST acceptance @ {base} ({stamp})")
    status, _ = http("GET", f"{base}/api/ready")
    if status != 200:
        print(f"FAIL  app is not ready: /api/ready answered {status}")
        return 1

    try:
        tenant = db.one(
            "INSERT INTO tenants (name, slug, is_active, probe_harness) "
            f"VALUES ('probe-ro-{stamp}', 'probe-ro-{stamp}', true, 'api-key-role-acceptance') "
            "RETURNING id"
        )
        ro_key, _ = mint_key(db, tenant, "read_only", "probe-read-only")
        rw_key, _ = mint_key(db, tenant, "workspace", "probe-workspace")
        slug = f"ro_probe_{stamp}"

        # ── fixtures, written with the WORKSPACE key (so they exist on any binary) ──────────────
        st, body = http("POST", f"{base}/api/objects", {"slug": slug, "name": f"Probe {stamp}"}, rw_key)
        obj = body.get("object") or body
        object_id = obj.get("id") or db.one(
            f"SELECT id FROM custom_objects WHERE tenant_id='{tenant}' AND slug='{slug}'"
        )
        check("fixture: a workspace key creates the object (POST /api/objects 200)",
              st == 200 and bool(object_id), f"status={st} id={object_id}")

        st, body = http("POST", f"{base}/api/objects/{slug}/fields",
                        {"key": "email", "label": "Email", "kind": "text", "unique": True}, rw_key)
        field_id = (body.get("field") or body).get("id") or db.one(
            f"SELECT f.id FROM custom_fields f JOIN custom_objects o ON o.id=f.object_id "
            f"WHERE o.id='{object_id}' AND f.key='email'"
        )
        check("fixture: a workspace key adds a field (POST …/fields 200)",
              st == 200 and bool(field_id), f"status={st} id={field_id}")

        st, body = http("POST", f"{base}/api/objects/{slug}/records",
                        {"email": "keep-me@example.test"}, rw_key)
        record_id = body.get("id") or db.one(
            f"SELECT id FROM custom_records WHERE object_id='{object_id}' AND deleted_at IS NULL"
        )
        check("fixture: a workspace key inserts a record (POST …/records 200)",
              st == 200 and bool(record_id), f"status={st} id={record_id}")

        contact_email = f"probe-ro-contact-{stamp}@example.test"
        deal_ref = f"probe-ro-deal-{stamp}"

        # ── A. the read_only key READS ─────────────────────────────────────────────────────────
        for label, method, path in [
            ("A1 GET /api/objects", "GET", "/api/objects"),
            ("A2 GET /api/external/lists", "GET", "/api/external/lists"),
            ("A3 GET …/schema", "GET", f"/api/objects/{slug}/schema"),
            ("A4 GET …/records", "GET", f"/api/objects/{slug}/records"),
            ("A5 GET …/audit", "GET", f"/api/objects/{slug}/audit"),
        ]:
            st, body = http(method, f"{base}{path}", None, ro_key)
            check(f"{label} answers 200 for the read_only key", st == 200,
                  f"status={st} body={json.dumps(body)[:120]}")

        # ── D. MCP unchanged ──────────────────────────────────────────────────────────────────
        err, payload = rpc(base, ro_key, "query_records", {"object": slug, "limit": 10})
        check("D1 MCP read tool answers for the read_only key (query_records)",
              not err, f"error={payload.get('error')} payload={json.dumps(payload)[:120]}")

        err, payload = rpc(base, ro_key, "create_object",
                          {"slug": f"mcp_hacked_{stamp}", "name": "mcp hacked"})
        check("D2 MCP write tool is still refused (create_object -> forbidden)",
              err and payload.get("error") == "forbidden",
              f"isError={err} payload={json.dumps(payload)[:120]}")

        # ── E. a workspace key is unaffected ──────────────────────────────────────────────────
        st, body = http("POST", f"{base}/api/objects",
                        {"slug": f"rw_ok_{stamp}", "name": f"RW {stamp}"}, rw_key)
        check("E1 a workspace key still creates an object (200)", st == 200,
              f"status={st} body={json.dumps(body)[:120]}")
        st, body = http("POST", f"{base}/api/objects/{slug}/records",
                        {"email": "second@example.test"}, rw_key)
        check("E2 a workspace key still inserts a record (200)", st == 200,
              f"status={st} body={json.dumps(body)[:120]}")
        st, body = http("GET", f"{base}/api/external/lists", None, rw_key)
        check("E3 a workspace key still reads /api/external/lists (200)", st == 200, f"status={st}")

        # ── B. every WRITE arm answers 403 + the sentence ─────────────────────────────────────
        writes = [
            ("B1  POST /api/objects (create_object)", "POST", "/api/objects",
             {"slug": f"hacked_{stamp}", "name": "hacked"}),
            ("B2  PATCH /api/objects/:slug", "PATCH", f"/api/objects/{slug}", {"unique_together": ["email"]}),
            ("B3  DELETE /api/objects/:slug", "DELETE", f"/api/objects/{slug}", None),
            ("B4  POST /api/objects/:slug/fields", "POST", f"/api/objects/{slug}/fields",
             {"key": "hacked", "label": "Hacked", "kind": "text"}),
            ("B5  PATCH …/fields/:id", "PATCH", f"/api/objects/{slug}/fields/{field_id}",
             {"label": "HACKED"}),
            ("B6  DELETE …/fields/:id", "DELETE", f"/api/objects/{slug}/fields/{field_id}", None),
            ("B7  POST …/records (insert)", "POST", f"/api/objects/{slug}/records",
             {"email": "hacked@example.test"}),
            ("B8  PATCH …/records/:id", "PATCH", f"/api/objects/{slug}/records/{record_id}",
             {"email": "hacked@example.test"}),
            ("B9  DELETE …/records/:id", "DELETE", f"/api/objects/{slug}/records/{record_id}", None),
            ("B10 POST /api/external/contacts", "POST", "/api/external/contacts",
             {"email": contact_email, "first_name": "Probe", "source_app": "funnelswift",
              "tags": [], "fields": {"channel": "rest-probe"}}),
            ("B11 POST /api/external/deals", "POST", "/api/external/deals",
             {"external_ref": deal_ref, "title": "Probe RO deal", "value": 10,
              "currency": "USD"}),
            ("B12 PATCH /api/external/deals/:ref/stage", "PATCH",
             f"/api/external/deals/{deal_ref}/stage", {"stage": "Closed Won"}),
            ("B13 POST /api/external/deals/:ref/status", "POST",
             f"/api/external/deals/{deal_ref}/status", {"status": "won"}),
            ("B14 POST /api/data-mapping/events", "POST", "/api/data-mapping/events",
             {"app": "funnelswift", "entity": "leads", "event_id": f"probe-{stamp}",
              "row": {"email": contact_email}}),
        ]
        for label, method, path, payload in writes:
            st, body = http(method, f"{base}{path}", payload, ro_key)
            check(f"{label} refuses the read_only key with 403 + the sentence",
                  refused_sentence(st, body), f"status={st} body={json.dumps(body)[:160]}")

        # ── C. nothing landed ──────────────────────────────────────────────────────────────────
        unchanged = db.one(
            "SELECT o.name||'|'||coalesce(o.archived_at::text,'null')||'|'||"
            "coalesce((SELECT f.label FROM custom_fields f WHERE f.id='" + field_id + "'),'MISSING')||'|'||"
            "coalesce((SELECT r.data->>'email' FROM custom_records r WHERE r.id='" + record_id + "'),'MISSING')||'|'||"
            "coalesce((SELECT r.deleted_at::text FROM custom_records r WHERE r.id='" + record_id + "'),'null') "
            f"FROM custom_objects o WHERE o.id='{object_id}'"
        )
        expected = f"Probe {stamp}|null|Email|keep-me@example.test|null"
        check("C1 the object, its field and its record are byte-identical to the fixtures",
              unchanged == expected, f"got={unchanged}")

        hacked_obj = db.one(f"SELECT count(*) FROM custom_objects WHERE tenant_id='{tenant}' AND slug='hacked_{stamp}'")
        hacked_field = db.one(f"SELECT count(*) FROM custom_fields WHERE object_id='{object_id}' AND key='hacked'")
        hacked_record = db.one(
            f"SELECT count(*) FROM custom_records WHERE object_id='{object_id}' AND data->>'email'='hacked@example.test'"
        )
        check("C2 no object, field or record was created through the read_only key",
              hacked_obj == "0" and hacked_field == "0" and hacked_record == "0",
              f"object={hacked_obj} field={hacked_field} record={hacked_record}")

        contact = db.one(
            f"SELECT count(*) FROM contacts WHERE tenant_id='{tenant}' AND email='{contact_email}'"
        )
        deal = db.one(
            f"SELECT count(*) FROM opportunities WHERE tenant_id='{tenant}' AND external_ref='{deal_ref}'"
        )
        check("C3 no contact and no deal were written through the read_only key",
              contact == "0" and deal == "0", f"contacts={contact} deals={deal}")

    finally:
        if tenant and not args.keep:
            db.q(f"DELETE FROM tenants WHERE id = '{tenant}'")
            left_tenants = db.one(f"SELECT count(*) FROM tenants WHERE slug LIKE 'probe-ro-{stamp}%'")
            left_keys = db.one(
                "SELECT count(*) FROM personal_api_keys "
                "WHERE tenant_id NOT IN (SELECT id FROM tenants)"
            )
            left_contacts = db.one(
                f"SELECT count(*) FROM contacts WHERE email = 'probe-ro-contact-{stamp}@example.test'"
            )
            print(f"cleanup: probe workspace removed (tenants left={left_tenants}, "
                  f"orphan keys={left_keys}, probe contacts={left_contacts})")
            check("F1 the probe workspace left no residue",
                  left_tenants == "0" and left_keys == "0" and left_contacts == "0",
                  f"tenants={left_tenants} orphan_keys={left_keys} contacts={left_contacts}")

    print(f"\n{len(PASS)} PASS / {len(FAIL)} FAIL")
    if FAIL:
        print("FAILED: " + "; ".join(FAIL))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
