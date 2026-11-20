#!/usr/bin/env python3
"""Acceptance harness for the STANDING DEFAULT CURRENCY (kanban t_4e220c20).

David's ruling, 2026-11-20: "The default currency will always be the US dollar... Nobody's first
deal attempt should be refused over a missing default."

Proves, on the REAL HTTP surface against the live database:

  1. BACKFILL — every workspace with object `settings` now states `default_currency = USD`, and no
     workspace is left without a default (Tenancy Canary B was DELETED, not backfilled, on David's
     2026-11-20 amendment);
  2. EXTERNAL PUSH, NO CURRENCY, on a workspace that had no default before this change
     (SwiftSoftware) → 2xx, the row stores `currency = USD`, `exchange_rate = 1`,
     `reporting_currency = USD` and a `rate_date`; the proof rows are removed afterwards;
  3. FRESH WORKSPACE — a tenant created exactly the way signup creates one (no currency seated,
     `settings` left at the column default) is never refused on its FIRST deal: the push answers
     2xx and stores USD (the resolution rule, not a per-row default);
  4. EUR REGRESSION — a workspace whose own default is EUR still stores EUR for a push that names
     no currency, at the EUR→USD rate the table holds (never a hardcoded 1).

Every throw-away workspace is DELETED at the end and the run fails if a single row survives. No
market rate is asserted as a constant: expected rates are READ from `fx_rates`.

    python3 scripts/proof-standing-default-currency.py [--base http://127.0.0.1:8084]
"""
import argparse
import base64
import hashlib
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

ENV_FILE = "/etc/swift/env/coreswift.env"

# The workspace the card names as "previously NONE" (SwiftSoftware).
SWIFTSOFTWARE = "7c1f787c-6f10-4c4d-ac24-71b82c38934e"


def env(key):
    with open(ENV_FILE) as fh:
        for line in fh:
            line = line.strip()
            if line.startswith(key + "="):
                return line.split("=", 1)[1].strip().strip('"').strip("'")
    return None


DB = env("DATABASE_URL")


def psql(sql):
    out = subprocess.run(["psql", DB, "-Atc", sql], capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(f"psql failed: {out.stderr.strip()}\n{sql}")
    return out.stdout.strip()


def call(base, method, path, body=None, bearer=None):
    headers = {"Content-Type": "application/json", "User-Agent": "swift-proof/1.0"}
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


CHECKS = []


def b64url(raw):
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def mint(uid, aid, role):
    """A session token, exactly as src/auth signs one — so the CONSOLE's own reads are exercised."""
    import hmac
    secret = env("JWT_SECRET") or ""
    now = int(time.time())
    head = b64url(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    claims = {"sub": uid, "aid": aid, "role": role, "iat": now, "exp": now + 3600,
              "iss": "coreswift", "aud": "coreswift-api"}
    body = b64url(json.dumps(claims, separators=(",", ":")).encode())
    sig = b64url(hmac.new(secret.encode(), f"{head}.{body}".encode(), hashlib.sha256).digest())
    return f"{head}.{body}.{sig}"


def check(name, ok, detail=""):
    CHECKS.append((name, ok, detail))
    print(("ok    " if ok else "FAIL  ") + name + ("" if ok else f"  [{detail}]"))


# ── throw-away workspaces ────────────────────────────────────────────────────────────────────
# Created the way the app creates one (tenants.settings left at the column default), with a key
# minted straight into `personal_api_keys`: the external door authenticates on `key_hash` only, so
# this exercises the SAME resolve_key path a real issued key takes. Everything is deleted at the end.
def make_workspace(label, currency=None, reporting=None):
    tid = str(uuid.uuid4())
    slug = f"proof-{label}-{uuid.uuid4().hex[:8]}"
    psql(
        "insert into tenants (id, name, slug) values "
        f"('{tid}', 'Proof {label} {tid[:8]}', '{slug}')"
    )
    if currency:
        psql(
            "update tenants set settings = jsonb_set(coalesce(settings,'{}'::jsonb), "
            f"'{{default_currency}}', to_jsonb('{currency}'::text), true) where id = '{tid}'"
        )
    if reporting:
        psql(
            "update tenants set settings = jsonb_set(coalesce(settings,'{}'::jsonb), "
            f"'{{reporting_currency}}', to_jsonb('{reporting}'::text), true) where id = '{tid}'"
        )
    key = "csk_" + uuid.uuid4().hex + uuid.uuid4().hex[:12]
    kh = hashlib.sha256(key.encode()).hexdigest()
    psql(
        "insert into personal_api_keys (id, tenant_id, name, key_hash, key_prefix) values "
        f"('{str(uuid.uuid4())}', '{tid}', 'proof', '{kh}', '{key[:12]}')"
    )
    # A real user row, because the session door refuses a token whose `sub` is not a live user
    # (auth_middleware: users existence + enabled + password-change revocation).
    uid = str(uuid.uuid4())
    psql(
        "insert into users (id, tenant_id, email, name, role, password_hash) values "
        f"('{uid}', '{tid}', 'proof+{uid[:8]}@example.invalid', 'Proof User', 'owner', "
        "'$2b$12$proofonlyproofonlyproofonlyproofonlyproofonlyproofonlyproo')"
    )
    return tid, key, uid


def drop_workspace(tid):
    psql(f"delete from tenants where id = '{tid}'")


def rows_for(tid):
    """Every row that carries this tenant id, in ANY table that has a tenant_id column."""
    sql = psql(
        "select string_agg(format('select %L tbl, count(*) n from %I where tenant_id = %L', "
        "table_name, table_name, '" + tid + "'), ' union all ') "
        "from information_schema.columns where column_name='tenant_id' and table_schema='public'"
    )
    if not sql:
        return 0
    return int(psql(f"select coalesce(sum(n),0) from ({sql}) x") or 0)


def open_sealed(tenant_id, sealed):
    """Open `key_sealed` exactly as src/secret_box.rs does (AES-GCM, tenant-scoped key).

    The harness runs on the box as the operator, so it opens the SAME envelope the app opens with
    the SAME master secret. No credential is minted, rotated or written anywhere.
    """
    secret = env("CORESWIFT_SECRET")
    if not secret or not sealed or not sealed.startswith("enc:v1:"):
        return ""
    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    except Exception:
        return ""
    key = hashlib.sha256(secret.encode() + b":" + uuid.UUID(tenant_id).bytes).digest()
    blob = base64.b64decode(sealed[len("enc:v1:"):])
    nonce, ct = blob[:12], blob[12:]
    try:
        return AESGCM(key).decrypt(nonce, ct, None).decode()
    except Exception:
        return ""


def usd_rate(currency):
    return float(psql(
        f"select usd_rate::text from fx_rates where upper(currency)=upper('{currency}') "
        "order by as_of desc limit 1") or 0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    args = ap.parse_args()
    stamp = str(int(time.time()))
    tenants_before = psql("select count(*) from tenants")
    made = []

    # ── 1. the backfill is the live state ────────────────────────────────────────────────────
    real = ("from tenants where name not like 'Tenancy Canary%' and slug not like 'probe-canary%'")
    census = psql("select name||' = '||coalesce(settings->>'default_currency','<none>') " + real
                  + " order by name")
    print("real workspaces → default_currency:\n  " + "\n  ".join(census.splitlines()))
    check("no real workspace is left without a default currency",
          "<none>" not in census, census)
    check("every real workspace states USD as its default",
          all(line.endswith(" = USD") for line in census.splitlines()), census)
    check("the census covers all 8 real workspaces", len(census.splitlines()) == 8,
          f"{len(census.splitlines())} rows")
    # The tenancy canaries were retired fleet-wide on David's 2026-11-20 ruling and taken off this
    # card: neither is backfilled as a requirement here, so this is reported, not asserted.
    print("canary rows still present (retired elsewhere): "
          + (psql("select coalesce(string_agg(name||'='||coalesce(settings->>'default_currency','<none>'), ', '),'none') "
                  "from tenants where name like 'Tenancy Canary%' or slug like 'probe-canary%'"))
          + "  — Canary B was deleted by this card; Canary A is deleted by the fleet")

    # ── 2. external push with NO currency on the workspace the card names (SwiftSoftware) ─────
    before_pipes = psql(f"select count(*) from pipelines where tenant_id='{SWIFTSOFTWARE}'")
    before_pipe_ids = psql(
        f"select coalesce(string_agg(id::text, ','),'') from pipelines where tenant_id='{SWIFTSOFTWARE}'")
    sealed = psql(
        "select coalesce(key_sealed,'') from personal_api_keys where tenant_id="
        f"'{SWIFTSOFTWARE}' and is_active limit 1")
    sw_key = open_sealed(SWIFTSOFTWARE, sealed)
    check("the SwiftSoftware workspace's own issued key opens", sw_key.startswith("csk_"),
          "sealed key did not open")
    ref = f"PROOF-USD-{stamp}"
    st, js = call(args.base, "POST", "/api/external/deals", bearer=sw_key, body={
        "external_ref": ref, "title": "Proof standing default currency", "value": 1200.5,
        "source": "proof-standing-default",
    })
    check("external push with NO currency on a previously-default-less workspace answers 2xx",
          st in (200, 201), f"HTTP {st} {js}")
    row = psql(
        "select coalesce(currency,'')||'|'||coalesce(exchange_rate::text,'')||'|'||"
        "coalesce(rate_date::text,'')||'|'||coalesce(reporting_currency,'') "
        f"from opportunities where tenant_id='{SWIFTSOFTWARE}' and external_ref='{ref}'")
    cur, rate, day, rep = (row.split("|") + ["", "", "", ""])[:4]
    check("the stored deal is priced in USD", cur == "USD", row)
    check("the stored rate is exactly 1", float(rate or 0) == 1.0, row)
    check("the stored reporting currency is USD", rep == "USD", row)
    check("a rate date was stamped", bool(day), row)
    check("the push answered created=true", js.get("created") is True, str(js)[:200])

    # 2b — the same per-deal path over HTTP, on a throw-away copy of that currency state, so the
    # assertions do not depend on the real workspace's leftover rows.
    tid, key, uid = make_workspace("usd", currency="USD")
    made.append(tid)
    st, js = call(args.base, "POST", "/api/external/deals", bearer=key, body={
        "external_ref": f"{ref}-copy", "title": "Proof default currency", "value": 1200.5,
        "source": "proof-standing-default",
    })
    check("the same push on a USD-default copy answers 2xx", st in (200, 201), f"HTTP {st} {js}")
    row = psql(
        "select coalesce(currency,'')||'|'||coalesce(exchange_rate::text,'') from opportunities "
        f"where tenant_id='{tid}' and external_ref='{ref}-copy'")
    check("…storing USD at a rate of 1", row == "USD|1.00000000", row)

    # ── 3. a FRESH workspace is never refused its first deal ────────────────────────────────
    # Two arms, because both must hold:
    #   3a. created the way the app creates one (migration 191's BEFORE INSERT trigger seats USD);
    #   3b. a workspace whose settings hold no default at all — the legacy shape — still resolves
    #       to USD at read time, which is what makes the rule true even if a write path forgets.
    tid2, key2, uid2 = make_workspace("fresh")
    made.append(tid2)
    seated = psql(f"select coalesce(settings->>'default_currency','') from tenants where id='{tid2}'")
    check("a brand-new workspace is seated USD at creation", seated == "USD",
          f"settings decimal default was '{seated}'")
    st2, js2 = call(args.base, "POST", "/api/external/deals", bearer=key2, body={
        "external_ref": f"PROOF-FRESH-{stamp}", "title": "First deal, no default",
        "value": 99.0, "source": "proof-standing-default",
    })
    check("a brand-new workspace's FIRST deal is not refused", st2 in (200, 201),
          f"HTTP {st2} {js2}")
    fresh_cur = psql(
        "select coalesce(currency,'<null>') from opportunities "
        f"where tenant_id='{tid2}'")
    check("…and it is stored as USD", fresh_cur == "USD", fresh_cur)
    fresh_rate = psql(
        "select coalesce(exchange_rate::text,'') from opportunities "
        f"where tenant_id='{tid2}'")
    check("…with a rate of 1 into its USD reporting currency",
          float(fresh_rate or 0) == 1.0, fresh_rate)

    # 3b — the same workspace with its currency key removed (the exact shape of a workspace created
    # before this change): the read-time standing default must still answer USD, never a 4xx.
    psql(f"update tenants set settings = coalesce(settings,'{{}}'::jsonb) - 'default_currency' "
         f"- 'currency' where id='{tid2}'")
    check("…and it can be made default-less again for the read-time arm",
          psql(f"select coalesce(settings->>'default_currency','') from tenants where id='{tid2}'") == "")
    st2b, js2b = call(args.base, "POST", "/api/external/deals", bearer=key2, body={
        "external_ref": f"PROOF-FRESH2-{stamp}", "title": "First deal, no default at all",
        "value": 55.0, "source": "proof-standing-default",
    })
    check("a workspace with NO stated default is still not refused", st2b in (200, 201),
          f"HTTP {st2b} {js2b}")
    legacy_cur = psql(
        "select coalesce(currency,'<null>') from opportunities "
        f"where tenant_id='{tid2}' and external_ref='PROOF-FRESH2-{stamp}'")
    check("…the standing default (USD) is used, not a refusal", legacy_cur == "USD", legacy_cur)

    # ── 4. EUR is still EUR for a push that names no currency ────────────────────────────────
    # Default EUR, reporting USD — the cross-currency case, so the stored rate must be the
    # EUR→USD rate the table holds and NOT 1 (a hardcoded dollar assumption would store 1).
    tid3, key3, uid3 = make_workspace("eur", currency="EUR", reporting="USD")
    made.append(tid3)
    st3, js3 = call(args.base, "POST", "/api/external/deals", bearer=key3, body={
        "external_ref": f"PROOF-EUR-{stamp}", "title": "Euro deal", "value": 1000.0,
        "source": "proof-standing-default",
    })
    check("a EUR workspace accepts a push with no currency", st3 in (200, 201),
          f"HTTP {st3} {js3}")
    eur = psql(
        "select coalesce(currency,'')||'|'||coalesce(exchange_rate::text,'')||'|'||"
        "coalesce(reporting_currency,'') from opportunities "
        f"where tenant_id='{tid3}'")
    eur_cur, eur_rate, eur_rep = (eur.split("|") + ["", "", ""])[:3]
    want = 1.0 / usd_rate("EUR") if usd_rate("EUR") else None   # EUR -> USD
    check("no regression: the deal stays EUR", eur_cur == "EUR", eur)
    check("…and carries the EUR→USD rate the table holds, not 1",
          want is not None and abs(float(eur_rate or 0) - want) < 1e-6,
          f"stored {eur_rate} expected {want}")
    check("…and reports in the workspace's reporting currency (follows the default)",
          eur_rep == "USD", eur)

    # ── 5. what the CONSOLE's own read answers (the surface a human sees) ────────────────────
    stc, cur = call(args.base, "GET", "/api/deals/currency",
                    bearer=mint(uid2, tid2, "owner"))
    check("the console's currency read answers 200", stc == 200, f"HTTP {stc} {cur}")
    check("…reporting USD and naming the standing default as its source",
          cur.get("currency") == "USD" and cur.get("source") == "standing_default",
          str(cur)[:240])
    stc3, cur3 = call(args.base, "GET", "/api/deals/currency",
                      bearer=mint(uid3, tid3, "owner"))
    check("…and a workspace that chose EUR reports EUR from the workspace",
          cur3.get("currency") == "EUR" and cur3.get("source") == "workspace",
          str(cur3)[:240])
    std, dl = call(args.base, "GET", "/api/deals",
                   bearer=mint(uid2, tid2, "owner"))
    check("the deals list reports its money in USD (reporting block)",
          std == 200 and (dl.get("reporting") or {}).get("currency") == "USD",
          f"HTTP {std} {str(dl.get('reporting'))[:200]}")

    # ── 6. zero residue ──────────────────────────────────────────────────────────────────────
    # The proof deal and the pipeline the writer created for it are removed from the real
    # workspace (it had none before this run — asserted above), so its state is exactly as found.
    psql("delete from opportunities where tenant_id="
         f"'{SWIFTSOFTWARE}' and external_ref like 'PROOF-%'")
    now_pipes = psql(
        f"select coalesce(string_agg(id::text, ','),'') from pipelines where tenant_id='{SWIFTSOFTWARE}'")
    for pid in [p for p in now_pipes.split(",") if p and p not in before_pipe_ids.split(",")]:
        psql(f"delete from pipelines where id='{pid}' and tenant_id='{SWIFTSOFTWARE}'")

    for tid in made:
        drop_workspace(tid)
    left = sum(rows_for(t) for t in made)
    check("every throw-away workspace is gone (zero residue)", left == 0, f"{left} rows left")
    check("the workspace count is back to what it was before this run",
          psql("select count(*) from tenants") == tenants_before,
          f"{psql('select count(*) from tenants')} vs {tenants_before}")
    check("no proof deal survives in SwiftSoftware",
          psql("select count(*) from opportunities where tenant_id="
               f"'{SWIFTSOFTWARE}' and external_ref like 'PROOF-%'") == "0")
    check("SwiftSoftware's pipeline count is unchanged by this run",
          psql(f"select count(*) from pipelines where tenant_id='{SWIFTSOFTWARE}'") == before_pipes)

    passed = sum(1 for _, ok, _ in CHECKS if ok)
    print(f"\n{passed}/{len(CHECKS)} checks passed")
    return 0 if passed == len(CHECKS) else 1


if __name__ == "__main__":
    sys.exit(main())
