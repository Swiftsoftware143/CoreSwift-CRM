#!/usr/bin/env python3
"""A DELETE that matched no row must refuse, not answer "deleted" (kanban t_2eba20dd).

Measured by the CEO's harness on 2026-11-25: `DELETE comms/templates/<made-up uuid>` through the
product MCP door answered `200 {"message":"Template deleted"}`. `campaigns::delete` and
`campaigns::delete_step` shared the shape: `execute()` with `rows_affected()` dropped.

This harness holds all three routes to one rule, on both doors the console and an agent use:

  * the product MCP door  POST /api/mcp  tool `coreswift_api`
  * the console route     DELETE /api/...  with a real session token (HS256, this app's own claims)

and in both modes:

  * `--mode pre`   the OLD bytes (bug present): each missing row answers 200 "…deleted"
  * `--mode post`  the FIXED bytes: each missing row answers 404 naming the row

It also creates a real row of each kind, deletes it, and reads the row count back from the database,
so the 404 is a refusal of the no-op and NOT a refusal of every delete. Probe rows are removed by id
and the tenant's own row counts are compared with the baseline taken at the start.

Usage:
    python3 scripts/proof-noop-delete-refusal.py --mode pre
    python3 scripts/proof-noop-delete-refusal.py --mode post
    python3 scripts/proof-noop-delete-refusal.py --mode post --base https://app.coreswiftcrm.com

Exit code 0 only when every leg matched the mode it asserts and the residue is zero.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

# The public edge answers a bare urllib client with a bot-wall page (Cloudflare 1010), not the app.
UA = "Mozilla/5.0 (X11; Linux x86_64) CoreSwift-proof/1.0"

PASS: list[str] = []
FAIL: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    (PASS if ok else FAIL).append(name)
    print(("PASS  " if ok else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))


class Db:
    """psql on the host. Credentials come from the app's own env file, never from the command line."""

    def __init__(self) -> None:
        url = ""
        for path in ("/etc/swift/env/coreswift.env",):
            if os.path.exists(path):
                for line in open(path, encoding="utf-8", errors="replace"):
                    if line.startswith("DATABASE_URL="):
                        url = line.split("=", 1)[1].strip().strip('"').strip("'")
                        break
        if not url:
            raise SystemExit("no DATABASE_URL in /etc/swift/env/coreswift.env")
        m = re.match(r"postgres(?:ql)?://([^:]+):([^@]+)@([^/:]+)(?::(\d+))?/(\w+)", url)
        if not m:
            raise SystemExit("cannot parse DATABASE_URL")
        self.user, self.password, self.host = m.group(1), m.group(2), m.group(3)
        self.port = m.group(4) or "5432"
        self.name = m.group(5)

    def one(self, sql: str) -> str:
        env = dict(os.environ, PGPASSWORD=self.password)
        out = subprocess.run(
            ["psql", "-h", self.host, "-p", self.port, "-U", self.user, "-d", self.name,
             "-At", "-v", "ON_ERROR_STOP=1", "-c", sql],
            capture_output=True, text=True, env=env,
        )
        if out.returncode != 0:
            raise SystemExit(f"psql failed: {out.stderr.strip()[:400]}")
        # `-At` still prints the command tag for RETURNING ("INSERT 0 1"): the value is the FIRST line.
        return out.stdout.strip().splitlines()[0] if out.stdout.strip() else ""

    def count(self, sql: str) -> int:
        return int(self.one(sql) or 0)


def b64(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def mint_session(secret: str, user: str, tenant: str, role: str = "owner") -> str:
    """The console's own HS256 claims — same shape `create_access_token` signs in-process."""
    now = int(time.time())
    claims = {"sub": user, "aid": tenant, "role": role, "exp": now + 120, "iat": now,
              "aud": "coreswift-api", "iss": "coreswift"}
    signing_input = b64(json.dumps({"alg": "HS256", "typ": "JWT"}).encode()) + "." + \
        b64(json.dumps(claims).encode())
    sig = hmac.new(secret.encode(), signing_input.encode(), hashlib.sha256).digest()
    return signing_input + "." + b64(sig)


class Door:
    def __init__(self, base: str) -> None:
        self.base = base.rstrip("/")

    def rpc(self, token: str, method: str, params: dict | None = None) -> dict:
        payload = {"jsonrpc": "2.0", "id": 1, "method": method}
        if params is not None:
            payload["params"] = params
        req = urllib.request.Request(
            self.base + "/api/mcp",
            data=json.dumps(payload).encode(),
            method="POST",
            headers={"Content-Type": "application/json",
                     "Accept": "application/json, text/event-stream",
                     "User-Agent": UA,
                     "Authorization": "Bearer " + token},
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                return json.loads(resp.read().decode())
        except urllib.error.HTTPError as err:
            return {"http": err.code, "body": err.read().decode()[:300]}

    def call(self, token: str, args: dict) -> tuple[str, str, str]:
        """-> (status-or-'error', body-text, refusal-sentence)"""
        out = self.rpc(token, "tools/call", {"name": "coreswift_api", "arguments": args})
        result = out.get("result") or {}
        text = (result.get("content") or [{}])[0].get("text", "")
        if result.get("isError"):
            return "error", "", text
        try:
            parsed = json.loads(text)
            return str(parsed.get("status")), json.dumps(parsed.get("body")), ""
        except Exception:
            return "?", "", text

    def http(self, method: str, path: str, token: str, body: dict | None = None) -> tuple[int, str]:
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            self.base + path, data=data, method=method,
            headers={"Content-Type": "application/json", "User-Agent": UA,
                     "Authorization": "Bearer " + token},
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                return resp.status, resp.read().decode()[:400]
        except urllib.error.HTTPError as err:
            return err.code, err.read().decode()[:400]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=("pre", "post"), required=True)
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    ap.add_argument("--tenant", default="821f3ad5-26a8-4a74-be49-772c0798d568",
                    help="the account the probe runs in (the owner's own: no customer rows)")
    args = ap.parse_args()
    fixed = args.mode == "post"

    db = Db()
    door = Door(args.base)
    slug = "probe-t-2eba20dd"

    secret = ""
    for line in open("/etc/swift/env/coreswift.env", encoding="utf-8", errors="replace"):
        if line.startswith("JWT_SECRET="):
            secret = line.split("=", 1)[1].strip().strip('"').strip("'")
    if not secret:
        raise SystemExit("no JWT_SECRET in /etc/swift/env/coreswift.env")

    user = db.one(
        "SELECT id FROM users WHERE tenant_id = '%s' AND (is_active IS NULL OR is_active) "
        "ORDER BY (role = 'owner' OR role = 'account_owner') DESC, created_at LIMIT 1" % args.tenant
    )
    if not user:
        raise SystemExit("the probe account has no active user to act as")

    # An issued workspace key, minted the same way the app mints one (sha256 of the token).
    key_token = "csk_" + uuid.uuid4().hex + uuid.uuid4().hex
    key_id = db.one(
        "INSERT INTO personal_api_keys (tenant_id, user_id, name, key_hash, key_prefix, is_active, api_role) "
        "VALUES ('%s', '%s', '%s', '%s', '%s', true, 'workspace') RETURNING id"
        % (args.tenant, user, slug, hashlib.sha256(key_token.encode()).hexdigest(), key_token[:12])
    )
    session = mint_session(secret, user, args.tenant)

    base_templates = db.count("SELECT count(*) FROM message_templates WHERE tenant_id = '%s'" % args.tenant)
    base_campaigns = db.count("SELECT count(*) FROM email_campaigns WHERE tenant_id = '%s'" % args.tenant)

    # Routes, missing-row arm. (label, path with a made-up uuid, name the refusal must carry)
    missing = [
        ("template", "comms/templates/%s", "Template"),
        ("campaign", "campaigns/%s", "Campaign"),
        ("campaign step", "campaigns/steps/%s", "Campaign step"),
    ]

    def verdict(label: str, got_status: str, got_body: str, sentence: str, row_name: str) -> None:
        where = f"{label}: missing row"
        if fixed:
            carries = (got_status == "error" and "404" in sentence and f"{row_name} not found" in sentence)
            check(f"{where} refused by the door (404 naming the row)", carries,
                  (sentence or got_body)[:110])
        else:
            deleted = got_status == "200" and "deleted" in got_body.lower()
            check(f"{where} wrongly reported deleted (the bug)", deleted,
                  f"status {got_status} {got_body[:80]}")

    try:
        for label, pattern, row_name in missing:
            path = pattern % uuid.uuid4()
            status, body, sentence = door.call(key_token, {"method": "DELETE", "path": path})
            verdict(label, status, body, sentence, row_name)

        # ── the console door, with a real session token, must answer the same way ──────────────
        for label, pattern, row_name in missing:
            path = "/api/" + pattern % uuid.uuid4()
            code, body = door.http("DELETE", path, session)
            where = f"{label}: missing row, console session"
            if fixed:
                check(f"{where} refused (404 naming the row)",
                      code == 404 and f"{row_name} not found" in body, f"{code} {body[:110]}")
            else:
                check(f"{where} wrongly reported deleted (the bug)",
                      code == 200 and "deleted" in body.lower(), f"{code} {body[:80]}")

        # ── a real row still deletes, and the database agrees ────────────────────────────────
        status, body, sentence = door.call(key_token, {
            "method": "POST", "path": "comms/templates",
            "body": {"name": slug, "channel": "email", "subject": "probe", "body": "probe"},
        })
        tpl_id = json.loads(body).get("id", "") if status in ("200", "201") else ""
        check("probe template created", bool(tpl_id), f"status {status} {sentence[:80]}")

        status, body, sentence = door.call(key_token, {
            "method": "POST", "path": "campaigns", "body": {"name": slug, "description": "probe"},
        })
        camp_id = ""
        if status in ("200", "201"):
            created = json.loads(body)
            camp_id = (created.get("campaign") or {}).get("id", "") if isinstance(created, dict) else ""
        check("probe campaign created", bool(camp_id), f"status {status} {sentence[:80]}")

        status, body, sentence = door.call(key_token, {
            "method": "POST", "path": "campaigns/steps",
            "body": {"campaign_id": camp_id, "template_name": slug, "subject": "probe",
                     "body": "probe", "delay_days": 0},
        })
        step_id = json.loads(body).get("id", "") if status in ("200", "201") else ""
        check("probe campaign step created", bool(step_id), f"status {status} {sentence[:80]}")

        if tpl_id:
            status, body, sentence = door.call(key_token, {"method": "DELETE", "path": f"comms/templates/{tpl_id}"})
            gone = db.count("SELECT count(*) FROM message_templates WHERE id = '%s'" % tpl_id) == 0
            check("own template deletes with 200 and the row is gone",
                  status == "200" and gone, f"status {status} db-left {int(not gone)}")

        if step_id:
            code, body = door.http("DELETE", f"/api/campaigns/steps/{step_id}", session)
            gone = db.count("SELECT count(*) FROM email_campaign_steps WHERE id = '%s'" % step_id) == 0
            check("own campaign step deletes over the console and the row is gone",
                  code == 200 and gone, f"{code} db-left {int(not gone)}")

        if camp_id:
            code, body = door.http("DELETE", f"/api/campaigns/{camp_id}", session)
            gone = db.count("SELECT count(*) FROM email_campaigns WHERE id = '%s'" % camp_id) == 0
            check("own campaign deletes over the console and the row is gone",
                  code == 200 and gone, f"{code} db-left {int(not gone)}")

    finally:
        # never leave a probe row behind, whatever the run did
        db.one("DELETE FROM email_campaign_steps WHERE campaign_id IN "
               "(SELECT id FROM email_campaigns WHERE tenant_id = '%s' AND name = '%s')"
               % (args.tenant, slug))
        db.one("DELETE FROM email_campaigns WHERE tenant_id = '%s' AND name = '%s'" % (args.tenant, slug))
        db.one("DELETE FROM message_templates WHERE tenant_id = '%s' AND name = '%s'" % (args.tenant, slug))
        db.one("DELETE FROM personal_api_keys WHERE id = '%s'" % key_id)

    residue = (
        db.count("SELECT count(*) FROM message_templates WHERE tenant_id = '%s' AND name = '%s'" % (args.tenant, slug))
        + db.count("SELECT count(*) FROM email_campaigns WHERE tenant_id = '%s' AND name = '%s'" % (args.tenant, slug))
        + db.count("SELECT count(*) FROM personal_api_keys WHERE tenant_id = '%s' AND name = '%s'" % (args.tenant, slug))
    )
    check("probe residue is zero", residue == 0, f"{residue} rows")
    after_templates = db.count("SELECT count(*) FROM message_templates WHERE tenant_id = '%s'" % args.tenant)
    after_campaigns = db.count("SELECT count(*) FROM email_campaigns WHERE tenant_id = '%s'" % args.tenant)
    check("tenant template count back to baseline", after_templates == base_templates,
          f"{base_templates} -> {after_templates}")
    check("tenant campaign count back to baseline", after_campaigns == base_campaigns,
          f"{base_campaigns} -> {after_campaigns}")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed  ({args.mode} arm, {args.base})")
    return 0 if not FAIL else 1


if __name__ == "__main__":
    sys.exit(main())
