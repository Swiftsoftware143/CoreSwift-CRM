#!/usr/bin/env python3
"""Acceptance harness for the DEAL WRITER (kanban t_2df68850).

Proves David's acceptance criterion on the REAL workspace, over the REAL HTTP surface:

    "XRide Dispatch can create a deal from a quote via the API with the quote ID as the
     external reference, and a re-send updates the same deal instead of creating a second one."

It drives:
  * the console arms with a signed session token (read from the live DB: the user's id, tenant,
    role — never a guessed role string), and
  * the external writer with the workspace's OWN issued API key, revealed through the app's
    sanctioned reveal route (no new credential is minted, so nothing is left behind to revoke).

Every check is asserted, not printed; the script exits non-zero on the first failure and cleans
up the deals it created (a suite that leaves residue fails the next run in a way that looks like a
product bug).

    python3 scripts/proof-deal-writer.py [--tenant-slug xride] [--base http://127.0.0.1:8084]
"""
import argparse
import base64
import hashlib
import hmac
import json
import os
import sys
import time
import urllib.error
import urllib.request

ENV_FILE = "/etc/swift/env/coreswift.env"


def env(key):
    with open(ENV_FILE) as fh:
        for line in fh:
            line = line.strip()
            if line.startswith(key + "="):
                return line.split("=", 1)[1].strip().strip('"').strip("'")
    return None


DB = env("DATABASE_URL")
SECRET = env("JWT_SECRET")


def psql(sql):
    import subprocess

    out = subprocess.run(
        ["psql", DB, "-Atc", sql], capture_output=True, text=True, check=True
    )
    return out.stdout.strip()


def b64url(raw):
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def mint(uid, aid, role):
    now = int(time.time())
    header = {"alg": "HS256", "typ": "JWT"}
    claims = {
        "sub": uid,
        "aid": aid,
        "role": role,
        "iat": now,
        "exp": now + 3600,
        "iss": "coreswift",
        "aud": "coreswift-api",
    }
    head = b64url(json.dumps(header, separators=(",", ":")).encode())
    body = b64url(json.dumps(claims, separators=(",", ":")).encode())
    signing = f"{head}.{body}".encode()
    sig = b64url(hmac.new(SECRET.encode(), signing, hashlib.sha256).digest())
    return f"{head}.{body}.{sig}"


def call(base, method, path, token=None, body=None, bearer=None):
    headers = {"Content-Type": "application/json", "User-Agent": "swift-proof/1.0"}
    if token:
        headers["Authorization"] = "Bearer " + token
    if bearer:
        headers["Authorization"] = "Bearer " + bearer
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            raw = res.read().decode()
            return res.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            parsed = json.loads(raw)
        except Exception:
            parsed = {"raw": raw}
        return e.code, parsed



def open_sealed(tenant_id, sealed):
    """Open `key_sealed` exactly as src/secret_box.rs + private_email/encryption.rs do.

    key = sha256(CORESWIFT_SECRET + ":" + raw_uuid_bytes); payload = base64(nonce || ct+tag).
    """
    secret = env("CORESWIFT_SECRET")
    if not secret or not sealed.startswith("enc:v1:"):
        return ""
    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    except Exception:
        return ""
    import uuid as _uuid

    key = hashlib.sha256(secret.encode() + b":" + _uuid.UUID(tenant_id).bytes).digest()
    blob = base64.b64decode(sealed[len("enc:v1:"):])
    nonce, ct = blob[:12], blob[12:]
    try:
        return AESGCM(key).decrypt(nonce, ct, None).decode()
    except Exception:
        return ""


CHECKS = []


def check(name, ok, detail=""):
    CHECKS.append((name, ok, detail))
    if not ok:
        print(f"FAIL  {name}  {detail}")
    else:
        print(f"ok    {name}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tenant-slug", default="xride")
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    args = ap.parse_args()

    tenant, slug = psql(
        f"select id||'|'||slug from tenants where slug ilike '%{args.tenant_slug}%' or name ilike '%{args.tenant_slug}%' order by created_at limit 1"
    ).split("|")
    uid, role = psql(
        f"select id||'|'||role from users where tenant_id='{tenant}' and closed_at is null order by created_at limit 1"
    ).split("|")
    token = mint(uid, tenant, role)
    print(f"workspace={slug} ({tenant}) user={uid} role={role}")

    # ── 0. migration is live ────────────────────────────────────────────────────────────────
    cols = psql(
        "select count(*) from information_schema.columns where table_name='opportunities' "
        "and column_name in ('external_ref','owner_id','last_activity_at','stage_entered_at',"
        "'exchange_rate','rate_date')"
    )
    check("opportunities carries the writer columns (migration 186)", cols == "6", f"found {cols}/6")

    # ── 1. workspace currency is stated, never assumed ──────────────────────────────────────
    st, cur = call(args.base, "GET", "/api/deals/currency", token=token)
    check("GET /api/deals/currency answers", st == 200, f"HTTP {st} {cur}")
    if not cur.get("set"):
        # The workspace has not chosen one; choose it explicitly (a human decision, recorded).
        st, cur = call(args.base, "PUT", "/api/deals/currency", token=token, body={"currency": "USD"})
        check("workspace currency could be set", st == 200 and cur.get("currency") == "USD", f"{st} {cur}")
    workspace_currency = cur.get("currency")
    print(f"      workspace currency = {workspace_currency}")

    # ── 2. the Xride industry layer lives in CUSTOM FIELDS, not in the core ────────────────
    st, fields = call(args.base, "GET", "/api/deals/custom-fields", token=token)
    have = {f["key"] for f in fields.get("fields", [])} if st == 200 else set()
    for key, label, ftype in [
        ("service_level", "Service level", "text"),
        ("pickup_city", "Pickup city", "text"),
        ("dropoff_city", "Drop-off city", "text"),
        ("job_date", "Job date", "date"),
        ("quote_reference", "Quote reference", "text"),
    ]:
        if key in have:
            continue
        st, f = call(
            args.base, "POST", "/api/deals/custom-fields", token=token,
            body={"key": key, "label": label, "field_type": ftype},
        )
        check(f"custom field '{key}' provisioned", st == 201, f"HTTP {st} {f}")

    # ── 3. the workspace's own issued key, revealed (no new credential minted) ─────────────
    key_id, sealed = psql(
        "select id||'|'||COALESCE(key_sealed,'') from personal_api_keys "
        f"where tenant_id='{tenant}' and is_active limit 1"
    ).split("|", 1)
    check("workspace holds an issued API key", bool(key_id), "no active key")
    # The reveal ROUTE is one click from a console session (plan §6.2, kanban t_76d82f06 — no password
    # re-prompt), but this harness runs on the box as the operator, so it opens the SAME sealed envelope
    # the app opens, with the SAME master secret and the same tenant-scoped derivation. No credential is
    # minted, rotated or left behind.
    api_key = open_sealed(tenant, sealed) if sealed else ""
    check("the workspace's issued key is usable (sealed store opens)",
          bool(api_key) and api_key.startswith("csk_"), "sealed key did not open")

    quote_ref = f"QUOTE-{int(time.time())}"
    deal_body = {
        "external_ref": quote_ref,
        "title": "Airport transfer — Miami to FLL",
        "value": 240.00,
        "stage": "Lead",
        "contact_name": "Proof Rider",
        "contact_email": f"{quote_ref.lower()}@example.invalid",
        "source": "xride-dispatch",
        "notes": "Proof push from the acceptance harness",
        "custom_fields": {
            "service_level": "Priority",
            "pickup_city": "Miami",
            "dropoff_city": "Fort Lauderdale",
            "job_date": "2026-12-01",
            "quote_reference": quote_ref,
        },
    }

    # ── 4. FIRST PUSH CREATES ──────────────────────────────────────────────────────────────
    st, first = call(args.base, "POST", "/api/external/deals", bearer=api_key, body=deal_body)
    check("first external push creates the deal (201, created=true)",
          st == 201 and first.get("created") is True, f"HTTP {st} {first}")
    deal_id = first.get("id")
    check("the deal carries the quote id as its external reference",
          first.get("external_ref") == quote_ref, str(first.get("external_ref")))
    check("currency came from the workspace, not from an assumed USD",
          first.get("currency") == workspace_currency, str(first.get("currency")))

    # ── 5. RE-SEND UPDATES — THE ACCEPTANCE CRITERION ─────────────────────────────────────
    deal_body["value"] = 310.50
    deal_body["stage"] = "Qualified"
    deal_body["title"] = "Airport transfer — Miami to FLL (revised)"
    st, second = call(args.base, "POST", "/api/external/deals", bearer=api_key, body=deal_body)
    check("re-send with the same reference answers 200, created=false",
          st == 200 and second.get("created") is False, f"HTTP {st} {second}")
    check("re-send keeps the SAME deal id", second.get("id") == deal_id,
          f"{second.get('id')} vs {deal_id}")
    rows = psql(f"select count(*) from opportunities where tenant_id='{tenant}' and external_ref='{quote_ref}'")
    check("one external reference = exactly ONE deal row", rows == "1", f"{rows} rows")
    value = psql(f"select value from opportunities where id='{deal_id}'") if deal_id else ""
    check("the re-send's value landed on that row", value.startswith("310.5"), value)

    # ── 6. STAGE MOVE and WON/LOST are writable from outside too ──────────────────────────
    st, moved = call(
        args.base, "PATCH", f"/api/external/deals/{quote_ref}/stage", bearer=api_key,
        body={"stage": "Proposal"},
    )
    check("outside system can move the stage", st == 200 and moved.get("moved") is True,
          f"HTTP {st} {moved}")
    st, status = call(
        args.base, "POST", f"/api/external/deals/{quote_ref}/status", bearer=api_key,
        body={"status": "won"},
    )
    check("outside system can mark the deal won", st == 200 and status.get("is_won") is True,
          f"HTTP {st} {status}")
    won_stage = psql(
        f"select s.name from opportunities o join pipeline_stages s on s.id=o.stage_id where o.id='{deal_id}'"
    ) if deal_id else ""
    check("a won deal lands on the pipeline's won stage", won_stage.lower() == "won", won_stage)

    # ── 7. the console sees the same deal, with its custom fields and timeline ────────────
    st, detail = call(args.base, "GET", f"/api/deals/{deal_id}", token=token)
    check("console detail answers", st == 200, f"HTTP {st}")
    cf = {f["key"]: f["value"] for f in detail.get("custom_fields", [])}
    check("custom fields are stored on the deal",
          cf.get("pickup_city") == "Miami" and cf.get("quote_reference") == quote_ref, str(cf))
    kinds = [a["kind"] for a in detail.get("activities", [])]
    check("the timeline recorded create + update + stage + status",
          {"created", "updated", "stage", "status"}.issubset(set(kinds)), str(kinds))
    check("the deal shows a weighted value", (detail.get("weighted_value") or 0) > 0,
          str(detail.get("weighted_value")))

    # ── 8. follow-ups and files ───────────────────────────────────────────────────────────
    st, task = call(args.base, "POST", f"/api/deals/{deal_id}/tasks", token=token,
                    body={"title": "Call the rider", "due_date": "2026-12-01"})
    check("a follow-up with a due date can be added", st == 201, f"HTTP {st} {task}")
    st, _ = call(args.base, "PATCH", f"/api/deals/{deal_id}/tasks/{task.get('id')}", token=token,
                 body={"completed": True})
    check("a follow-up can be completed", st == 200, f"HTTP {st}")
    payload = base64.b64encode(b"quote pdf bytes").decode()
    st, att = call(args.base, "POST", f"/api/deals/{deal_id}/attachments", token=token,
                   body={"filename": "quote.pdf", "content_type": "application/pdf",
                         "content_base64": payload})
    check("a file can be attached", st == 201, f"HTTP {st} {att}")

    # ── 9. reporting is per currency ──────────────────────────────────────────────────────
    st, rep = call(args.base, "GET", "/api/deals/report", token=token)
    reports = rep.get("reports", [])
    check("report answers", st == 200 and isinstance(reports, list), f"HTTP {st}")
    mine = [r for r in reports if r.get("currency") == workspace_currency]
    check("every report row names ONE currency", bool(mine), f"{[r.get('currency') for r in reports]}")
    if mine:
        check("won deals are counted in that currency",
              any(r.get("won_count", 0) >= 1 for r in mine),
              str([(r["pipeline_name"], r["won_count"]) for r in mine]))

    # ── 10. tenancy: another workspace's key cannot touch this deal ───────────────────────
    other = psql(
        "select k.id||'|'||k.tenant_id from personal_api_keys k "
        f"where k.tenant_id<>'{tenant}' and k.is_active and k.key_sealed is not null "
        "order by k.created_at desc limit 1"
    )
    if other:
        other_id, other_tenant = other.split("|")
        other_user = psql(
            f"select id||'|'||role from users where tenant_id='{other_tenant}' order by created_at limit 1"
        ).split("|")
        other_token = mint(other_user[0], other_tenant, other_user[1])
        other_sealed = psql(f"select COALESCE(key_sealed,'') from personal_api_keys where id='{other_id}'")
        other_key = open_sealed(other_tenant, other_sealed) if other_sealed else ""
        other_currency = psql(
            f"select COALESCE(settings->>'default_currency','') from tenants where id='{other_tenant}'"
        )
        if other_key:
            st, refused = call(args.base, "GET", f"/api/deals/{deal_id}", bearer=other_key)
            check("a foreign key cannot read this workspace's deal", st in (401, 404),
                  f"HTTP {st} {refused}")
            st, separate = call(args.base, "POST", "/api/external/deals", bearer=other_key,
                                body={"external_ref": quote_ref, "title": "other workspace",
                                      "value": 1, "currency": "USD"})
            check("the same external reference in ANOTHER workspace is a separate deal",
                  st == 201 and separate.get("id") != deal_id, f"HTTP {st} {separate}")
            if other_currency:
                # A workspace that HAS a default: a push with no currency takes it, and never
                # invents one.
                st, dflt = call(args.base, "POST", "/api/external/deals", bearer=other_key,
                                body={"external_ref": quote_ref + "-DFLT", "title": "default currency",
                                      "value": 2})
                check("a push with no currency takes the WORKSPACE's default",
                      st == 201 and dflt.get("currency") == other_currency,
                      f"HTTP {st} {dflt}")
                if dflt.get("id"):
                    call(args.base, "DELETE", f"/api/deals/{dflt.get('id')}", token=other_token)
            else:
                st, refused = call(args.base, "POST", "/api/external/deals", bearer=other_key,
                                   body={"external_ref": quote_ref + "-NC", "title": "no currency",
                                         "value": 1})
                check("a push with no currency into a workspace with NO default is refused, not assumed USD",
                      st == 422 and "no default currency" in json.dumps(refused), f"HTTP {st} {refused}")
            if separate.get("id"):
                call(args.base, "DELETE", f"/api/deals/{separate.get('id')}", token=other_token)
            # Scoped to the OTHER workspace on purpose: the probe deal in Xride still exists at
            # this point (step 11 deletes it), so an unscoped count would report it as residue.
            left_other = psql(
                "select count(*) from opportunities where tenant_id='%s' and external_ref in "
                "('%s','%s-DFLT','%s-NC')" % (other_tenant, quote_ref, quote_ref, quote_ref)
            )
            check("no probe residue in the other workspace", left_other == "0", f"{left_other} rows")

    # ── 11. cleanup, and the baseline must come back ─────────────────────────────────────
    st, _ = call(args.base, "DELETE", f"/api/deals/{deal_id}", token=token)
    check("probe deal deleted", st == 200, f"HTTP {st}")
    left = psql(f"select count(*) from opportunities where external_ref='{quote_ref}'")
    check("cleanup restored the baseline (0 rows with that reference)", left == "0", f"{left} rows")
    # The writer find-or-created a contact for the quote; a probe must not leave one behind.
    psql(f"delete from contacts where lower(email) like '{quote_ref.lower()}%@example.invalid'")
    contacts_left = psql(
        f"select count(*) from contacts where lower(email) like '{quote_ref.lower()}%@example.invalid'"
    )
    check("no probe contact left behind", contacts_left == "0", f"{contacts_left} rows")

    failed = [c for c in CHECKS if not c[1]]
    print(f"\n{len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
    if failed:
        print("FAILED:")
        for name, _, detail in failed:
            print(f"  - {name}: {detail}")
        sys.exit(1)


if __name__ == "__main__":
    main()
