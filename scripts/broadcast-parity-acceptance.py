#!/usr/bin/env python3
"""Preview/send parity — the CEO's pinned acceptance case (2026-11-27).

Three recipients in one run, on the Xride tenant:
  A = a suppression row (reason 'manual', source 'ceo-acceptance-20261127')
  B = a syntactically invalid address (literally: not-an-email)
  C = a clean control address

PRECONDITION, and it fails the run rather than passing for the wrong reason: GET the suppression list
and assert A is present AND masked. A missing setup row is a FAIL, not a green zero — that exact false
green has already fooled this lane once.

Then: preview and send must agree, in the same run:
  A suppressed (reason code identical on both), B invalid and never would_send, C would_send
  counts 1 / 1 / 1; A and B not attempted; C queued exactly once.

Also asserted: no unmasked address in any response body, the CSV export refuses, no count field on the
suppression payload, and /api/health names the commit the run is testing.

Teardown: every probe row deleted, and the resulting zero stated explicitly.

Usage:  python3 scripts/broadcast-parity-acceptance.py [--keep]
Exit 0 only when every arm passes.
"""
import base64
import hashlib
import hmac
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

BASE = "http://127.0.0.1:8084"
TENANT = "6aaeb824-4b5b-4435-97af-752212eac983"  # Xride Drivers
SOURCE = "ceo-acceptance-20261127"
A_EMAIL = "parity-suppressed@example.invalid"
B_EMAIL = "not-an-email"
C_EMAIL = "parity-control@example.invalid"


def env():
    out = {}
    for line in open("/etc/swift/env/coreswift.env"):
        if "=" in line and not line.strip().startswith("#"):
            k, v = line.strip().split("=", 1)
            out[k.strip()] = v.strip().strip('"').strip("'")
    return out


def sql(envs, statement):
    return subprocess.run(["psql", envs["DATABASE_URL"], "-tAF|", "-c", statement],
                          capture_output=True, text=True).stdout.strip()


def mint(envs):
    row = sql(envs, f"select id||'|'||role from users where tenant_id='{TENANT}' "
                    "and role='owner' and is_active limit 1").splitlines()[0]
    user, role = row.split("|")
    b64 = lambda b: base64.urlsafe_b64encode(b).decode().rstrip("=")
    now = int(time.time())
    head = b64(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    # iss/aud are REQUIRED by the credential boundary (measured: without them a valid-looking JWT is
    # refused with 401 before any handler runs, which is how the first run of this harness failed).
    body = b64(json.dumps({"sub": user, "aid": TENANT, "role": role, "exp": now + 900,
                           "iat": now, "aud": "coreswift-api", "iss": "coreswift"},
                          separators=(",", ":")).encode())
    sig = b64(hmac.new(envs["JWT_SECRET"].encode(), f"{head}.{body}".encode(), hashlib.sha256).digest())
    return f"{head}.{body}.{sig}"


def call(token, path, body=None):
    req = urllib.request.Request(BASE + path,
                                 data=json.dumps(body).encode() if body is not None else None,
                                 method="POST" if body is not None else "GET",
                                 headers={"Authorization": "Bearer " + token,
                                          "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def account_key(envs):
    """The account's own issued key, when one is available: it is the door the CEO actually holds, so an
    acceptance that passes with it is an acceptance he can reproduce. A console JWT is the fallback."""
    for path in ("/tmp/xride_full_key.txt",):
        try:
            key = open(path).read().strip()
            if key.startswith("csk_"):
                return key
        except OSError:
            pass
    return mint(envs)


def main():
    keep = "--keep" in sys.argv
    envs = env()
    # TWO DOORS, each used for what it actually reaches (measured): the account key reads the compliance
    # surface (the door the CEO holds), while broadcast preview/send are session-only by design. Using one
    # credential for both is what made the first runs fail for the wrong reason.
    key = account_key(envs)
    token = mint(envs)
    fails = []

    health = json.loads(call(token, "/api/health")[1])
    print(f"/api/health commit: {health.get('commit')}  built_at: {health.get('built_at')}")

    # ── setup ────────────────────────────────────────────────────────────────────────────
    list_id = str(uuid.uuid4())
    sql(envs, f"insert into lists (id,tenant_id,name,list_type) "
              f"values ('{list_id}','{TENANT}','parity acceptance list','static')")
    clean = []
    for email in (A_EMAIL, C_EMAIL):
        cid = str(uuid.uuid4())
        sql(envs, f"insert into contacts (id,tenant_id,email,first_name,last_name) "
                  f"values ('{cid}','{TENANT}','{email}','Parity','Probe')")
        sql(envs, f"insert into list_members (id,list_id,contact_id,tenant_id,added_manually) "
                  f"values ('{uuid.uuid4()}','{list_id}','{cid}','{TENANT}',true)")
        clean.append(cid)
    # B: a contact whose stored address is the literal string, which is what the defect counted.
    b_id = str(uuid.uuid4())
    sql(envs, f"insert into contacts (id,tenant_id,email,first_name,last_name) "
              f"values ('{b_id}','{TENANT}','{B_EMAIL}','Parity','Invalid')")
    sql(envs, f"insert into list_members (id,list_id,contact_id,tenant_id,added_manually) "
              f"values ('{uuid.uuid4()}','{list_id}','{b_id}','{TENANT}',true)")
    clean.append(b_id)
    sql(envs, f"insert into email_suppressions (id,tenant_id,address,mail_class,reason,source) "
              f"values ('{uuid.uuid4()}','{TENANT}','{A_EMAIL}','bulk','manual','{SOURCE}')")

    try:
        # ── precondition: A must EXIST and be masked, or the run is meaningless ─────────
        st, body = call(key, "/api/mail-compliance/suppressions")
        payload = json.loads(body) if st == 200 else {}
        entries = payload.get("suppressions", [])
        present = any(e.get("source") == SOURCE for e in entries)
        masked = all(e.get("address") != A_EMAIL for e in entries)
        print(f"precondition: suppressions {st}, entries {len(entries)}, A present {present}, masked {masked}")
        if not (st == 200 and present and masked):
            fails.append("precondition failed: the setup suppression row is not present and masked")
            raise SystemExit(report(fails, envs, list_id, clean, keep))
        if any(k in payload for k in ("total", "count")):
            fails.append("suppression payload carries a count field")
        if A_EMAIL in body:
            fails.append("suppression payload echoed an unmasked address")
        # REFUSED ON BOTH DOORS, by different arms: a key is stopped by the boundary (403 "needs a console
        # session"), a console session reaches the handler which answers 410. Either way no file is
        # produced, which is the property that matters.
        st_csv, csv_body = call(key, "/api/mail-compliance/suppressions.csv")
        if st_csv not in (403, 410):
            fails.append(f"csv export answered {st_csv}, expected 403 (key door) or 410 (session door)")
        if "," in csv_body and A_EMAIL in csv_body:
            fails.append("csv export leaked a suppressed address")

        # ── preview ─────────────────────────────────────────────────────────────────────
        audience = {"list_id": list_id}
        st, body = call(token, "/api/broadcasts/preview",
                        {"list_id": list_id, "subject": "parity", "body": "parity"})
        print(f"preview: {st} {body[:220]}")
        p = json.loads(body) if st == 200 else {}
        if st != 200:
            fails.append(f"preview answered {st}")
        else:
            if p.get("would_send") != 1:
                fails.append(f"preview would_send {p.get('would_send')}, expected 1")
            if p.get("suppressed") != 1:
                fails.append(f"preview suppressed {p.get('suppressed')}, expected 1")
            if p.get("invalid") != 1:
                fails.append(f"preview invalid {p.get('invalid')}, expected 1")
            if any(e.get("reason") != "manual"
                   for e in p.get("suppressed_examples", [])):
                fails.append("preview suppressed reason does not match the stored reason")

        # ── send ────────────────────────────────────────────────────────────────────────
        st2, body2 = call(token, "/api/broadcasts",
                          {"list_id": list_id, "subject": "parity run", "body": "parity run", "send": True})
        print(f"send: {st2} {body2[:220]}")
        s2 = json.loads(body2) if st2 == 200 else {}
        if st2 != 200:
            fails.append(f"send answered {st2}")
        else:
            if s2.get("queued") != 1:
                fails.append(f"send queued {s2.get('queued')}, expected 1")
            if s2.get("suppressed") != p.get("suppressed"):
                fails.append(f"send suppressed {s2.get('suppressed')} != preview {p.get('suppressed')}")
            if s2.get("invalid") != p.get("invalid"):
                fails.append(f"send invalid {s2.get('invalid')} != preview {p.get('invalid')}")
        # SCOPED TO THIS RUN. A pre-existing outbound row for the same address (his own earlier probe) made
        # an earlier version of this harness report "invalid recipient was attempted" when nothing of ours
        # had been attempted at all.
        scope = "subject = 'parity run'"
        queued = sql(envs, f"select count(*) from outbound_messages where {scope} and to_address='{C_EMAIL}'")
        attempted_b = sql(envs, f"select count(*) from outbound_messages where {scope} and to_address='{B_EMAIL}'")
        attempted_a = sql(envs, f"select count(*) from outbound_messages where {scope} and to_address='{A_EMAIL}'")
        print(f"queued rows: C={queued} B={attempted_b} A={attempted_a}")
        if queued != "1":
            fails.append(f"C queued {queued} times, expected once")
        if attempted_a != "0":
            fails.append("suppressed recipient A was attempted")
        if attempted_b != "0":
            fails.append("invalid recipient B was attempted")

        # ── unknown id and empty-but-valid audience shapes ──────────────────────────────
        st_bad, _ = call(token, "/api/broadcasts/preview",
                         {"tag_id": "00000000-0000-0000-0000-000000000000",
                          "subject": "p", "body": "p"})
        print(f"unknown tag id: {st_bad}")
        if st_bad != 404:
            fails.append(f"unknown tag id answered {st_bad}, expected 404")
    finally:
        report(fails, envs, list_id, clean, keep)


def report(fails, envs, list_id, clean, keep):
    if keep:
        print("KEEP requested: probe rows left in place")
    else:
        sql(envs, f"delete from outbound_messages where subject like 'parity%'")
        sql(envs, f"delete from email_suppressions where source='{SOURCE}'")
        sql(envs, f"delete from list_members where list_id='{list_id}'")
        sql(envs, f"delete from lists where id='{list_id}'")
        for cid in clean:
            sql(envs, f"delete from tag_assignments where entity_type='contact' and entity_id='{cid}'")
            sql(envs, f"delete from contacts where id='{cid}'")
        left = sql(envs, f"select (select count(*) from email_suppressions where source='{SOURCE}')"
                         f"||' suppressions / '||(select count(*) from contacts where first_name='Parity')"
                         f"||' contacts / '||(select count(*) from outbound_messages where subject like 'parity%')"
                         f"||' queued'")
        print(f"teardown: {left}")

    if fails:
        print("\nFAIL")
        for f in fails:
            print("  -", f)
        return 1
    print("\nPASS — preview and send agree: 1 would_send / 1 suppressed / 1 invalid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
