#!/usr/bin/env python3
"""Acceptance harness for the MULTI-CURRENCY CORE (kanban t_44c1aaa4).

Proves David's amendment on the REAL HTTP surface, against a throw-away workspace whose DEFAULT
currency is EUR and whose REPORTING currency is USD (so a hardcoded dollar assumption cannot pass
by accident). Every check is asserted; the script exits non-zero on the first failure and deletes
the workspace it created (a suite that leaves residue fails the next run in a way that looks like
a product bug).

    python3 scripts/proof-multi-currency.py [--base http://127.0.0.1:8084]

What it proves, one to one with the card's pass conditions:
  1. two open deals in ONE pipeline priced in EUR and GBP: the pipeline total never returns a raw
     mixed sum — per-currency totals AND a reporting-currency equivalent that reconciles by hand;
  2. changing a rate later does not change a past won deal's reported value (the stored rate is
     what reports read);
  3. a deal pushed through the external API with no currency takes the workspace DEFAULT (EUR here,
     not USD);
  4. a money custom field persists with its own currency code and the deal form still loads.
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
import uuid

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
    out = subprocess.run(["psql", DB, "-Atc", sql], capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(f"psql failed: {out.stderr.strip()}\n{sql}")
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
    sig = b64url(hmac.new(SECRET.encode(), f"{head}.{body}".encode(), hashlib.sha256).digest())
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


CHECKS = []


def check(name, ok, detail=""):
    CHECKS.append((name, ok, detail))
    print(("ok    " if ok else "FAIL  ") + name + ("" if ok else f"  [{detail}]"))


def close(a, b, tol=0.02):
    return abs(float(a) - float(b)) <= tol


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    args = ap.parse_args()

    # ── 0. the schema is live ────────────────────────────────────────────────────────────────
    check(
        "migration 188 applied: fx_rates exists with a rate per currency",
        psql("select count(*) from information_schema.tables where table_name='fx_rates'") == "1"
        and int(psql("select count(*) from fx_rates")) > 20,
        "fx_rates missing or unseeded",
    )
    check(
        "migration 188 applied: opportunities.reporting_currency + money columns",
        psql(
            "select count(*) from information_schema.columns where "
            "(table_name='opportunities' and column_name='reporting_currency') or "
            "(table_name='deal_field_values' and column_name in ('money_amount','money_currency','money_exchange_rate','money_reporting_currency'))"
        )
        == "5",
        "columns missing",
    )

    # ── the probe workspace: default EUR, reporting USD ────────────────────────────────────
    tag = uuid.uuid4().hex[:8]
    slug = f"mc-probe-{tag}"
    tenant = str(uuid.uuid4())
    user = str(uuid.uuid4())
    key = "csk_" + uuid.uuid4().hex + uuid.uuid4().hex
    psql(
        "insert into tenants (id,name,slug,settings,is_active) values ("
        f"'{tenant}','Multi Currency Probe {tag}','{slug}',"
        """'{"default_currency":"EUR","reporting_currency":"USD"}'::jsonb,true)"""
    )
    psql(
        "insert into users (id,tenant_id,email,password_hash,name,role) values ("
        f"'{user}','{tenant}','mc-probe-{tag}@example.invalid','x','Probe Owner','admin')"
    )
    psql(
        "insert into personal_api_keys (id,tenant_id,user_id,name,key_hash,key_prefix) values ("
        f"'{uuid.uuid4()}','{tenant}','{user}','probe',"
        f"'{hashlib.sha256(key.encode()).hexdigest()}','{key[:12]}')"
    )
    token = mint(user, tenant, "admin")
    print(f"workspace={slug} ({tenant})")

    deal_ids = []
    try:
        # ── 1. the workspace states two currencies, validated against ISO 4217 ─────────────
        st, cur = call(args.base, "GET", "/api/deals/currency", token=token)
        check(
            "workspace default EUR + reporting USD are read back",
            st == 200 and cur.get("currency") == "EUR" and cur.get("reporting_currency") == "USD",
            f"HTTP {st} {cur}",
        )
        check(
            "the currency list is the real ISO 4217 list",
            st == 200 and "USD" in (cur.get("iso_currencies") or []) and "EUR" in cur.get("iso_currencies", []),
            f"HTTP {st}",
        )
        st, bad = call(args.base, "PUT", "/api/deals/currency", token=token, body={"currency": "ABC"})
        check("a made-up code is refused, not stored", st == 422 or st == 400, f"HTTP {st} {bad}")

        # ── a pipeline to hold the probe deals ────────────────────────────────────────────
        st, pipe = call(args.base, "POST", "/api/pipelines", token=token, body={"name": f"Probe {tag}"})
        pid = pipe.get("id") or (pipe.get("pipeline") or {}).get("id")
        check("probe pipeline created", st in (200, 201) and pid, f"HTTP {st} {pipe}")
        st, stage = call(
            args.base, "POST", f"/api/pipelines/{pid}/stages", token=token,
            body={"name": "Open", "probability": 50, "position": 1},
        )
        sid = stage.get("id") or (stage.get("stage") or {}).get("id")
        check("probe stage created", st in (200, 201) and sid, f"HTTP {st} {stage}")

        # ── 2. CONDITION 3: external push with NO currency takes the workspace default ────
        st, ext = call(
            args.base, "POST", "/api/external/deals", bearer=key,
            body={
                "external_ref": f"MC-{tag}-NOCUR",
                "title": "Quote with no currency stated",
                "value": 500,
                "pipeline_id": pid,
                "stage_id": sid,
            },
        )
        deal = ext.get("deal") or ext
        check(
            "external API: a deal with no currency takes the workspace default (EUR), never USD",
            st in (200, 201) and deal.get("currency") == "EUR",
            f"HTTP {st} {ext}",
        )
        # The push answers with the currency; the STORED rate is read back from the deal itself
        # (and the database), which is what a report actually uses.
        stored_ext = {}
        if deal.get("id"):
            deal_ids.append(deal["id"])
            st2, det = call(args.base, "GET", f"/api/deals/{deal['id']}", token=token)
            stored_ext = det.get("deal") or {}
        db_rate = psql(
            "select coalesce(exchange_rate::text,'')||'|'||upper(coalesce(reporting_currency,''))||'|'||"
            f"case when rate_date is null then '' else 'dated' end from opportunities where id='{deal.get('id')}'"
        )
        check(
            "that deal stores a rate and a rate date (1 EUR = 1.08695652 USD)",
            close(stored_ext.get("exchange_rate") or 0, 1.08695652, 1e-6)
            and stored_ext.get("rate_date")
            and (stored_ext.get("reporting_currency") or "").upper() == "USD"
            and db_rate.split("|")[1] == "USD"
            and db_rate.split("|")[2] == "dated",
            f"rate={stored_ext.get('exchange_rate')} reporting={stored_ext.get('reporting_currency')} db={db_rate}",
        )

        # ── 3. CONDITION 1: two currencies in ONE pipeline, never one raw total ───────────
        st, d_eur = call(
            args.base, "POST", "/api/deals", token=token,
            body={"name": "EUR deal", "value": 1000, "currency": "EUR",
                  "pipeline_id": pid, "stage_id": sid, "probability": 50},
        )
        eur = d_eur.get("deal") or d_eur
        st2, d_gbp = call(
            args.base, "POST", "/api/deals", token=token,
            body={"name": "GBP deal", "value": 1000, "currency": "GBP",
                  "pipeline_id": pid, "stage_id": sid, "probability": 50},
        )
        gbp = d_gbp.get("deal") or d_gbp
        check("EUR and GBP deals created in one pipeline",
              st in (200, 201) and st2 in (200, 201) and eur.get("id") and gbp.get("id"),
              f"{st}/{st2}")
        deal_ids.extend([eur.get("id"), gbp.get("id")])

        st, an = call(args.base, "GET", f"/api/pipelines/{pid}/analytics", token=token)
        by_cur = {r["currency"]: r for r in (an.get("by_currency") or [])}
        check(
            "pipeline analytics returns TWO currency rows, not one mixed total",
            st == 200 and set(by_cur) == {"EUR", "GBP"},
            f"HTTP {st} rows={list(by_cur)}",
        )
        check(
            "each currency row sums only its own deals (EUR 1500 = 500 + 1000)",
            close(by_cur.get("EUR", {}).get("total_value", -1), 1500)
            and close(by_cur.get("GBP", {}).get("total_value", -1), 1000),
            f"EUR={by_cur.get('EUR', {}).get('total_value')} GBP={by_cur.get('GBP', {}).get('total_value')}",
        )
        # 500 EUR + 1000 EUR at 1.08695652 = 1630.43 USD; 1000 GBP at 1.26582278 = 1265.82 USD
        expected_reporting = 1500 * 1.08695652 + 1000 * 1.26582278
        rep = an.get("reporting") or {}
        check(
            "analytics reporting equivalent reconciles by hand (2352.78-ish USD)",
            rep.get("currency") == "USD" and close(rep.get("total_value", -1), expected_reporting, 0.5),
            f"got {rep.get('total_value')} expected {expected_reporting:.2f}",
        )
        check(
            "no reported figure is the meaningless raw 2000/2500 mix",
            not close(rep.get("total_value", 0), 2500 + 0, 0.5) and not close(rep.get("total_value", 0), 2000, 0.5),
            f"got {rep.get('total_value')}",
        )

        st, listing = call(args.base, "GET", f"/api/deals?pipeline_id={pid}&per_page=50", token=token)
        tb = listing.get("totals_by_currency") or []
        check(
            "deals list totals are per currency and the single scalar is withheld",
            st == 200 and len(tb) == 2 and listing.get("total_value") is None,
            f"HTTP {st} totals={tb} total_value={listing.get('total_value')}",
        )

        # ── 4. CONDITION 2: a later rate change does not move a won deal's value ─────────
        st, _ = call(args.base, "POST", f"/api/deals/{eur['id']}/status", token=token, body={"status": "won"})
        check("EUR deal marked won", st == 200, f"HTTP {st}")
        st, rep_before = call(args.base, "GET", "/api/deals/report", token=token)
        rows = [r for r in (rep_before.get("reports") or []) if r.get("pipeline_id") == pid]
        won_before = None
        for r in rows:
            if (r.get("currency") or "").upper() == "EUR":
                won_before = r.get("won_value")
        rep_won_before = (rows[0].get("reporting") or {}).get("won_value") if rows else None
        check(
            "the report shows the won deal in its own currency AND the reporting currency",
            close(won_before or -1, 1000) and close(rep_won_before or -1, 1086.96, 0.5),
            f"won_eur={won_before} won_usd={rep_won_before}",
        )

        # Move the market: a brand-new EUR row at double the rate.
        psql("insert into fx_rates (currency,usd_rate,as_of,source) values "
             f"('EUR',2.0,current_date + 1,'proof-shock')")
        st, rep_after = call(args.base, "GET", "/api/deals/report", token=token)
        rows2 = [r for r in (rep_after.get("reports") or []) if r.get("pipeline_id") == pid]
        won_after = rep_won_after = None
        for r in rows2:
            if (r.get("currency") or "").upper() == "EUR":
                won_after = r.get("won_value")
        rep_won_after = (rows2[0].get("reporting") or {}).get("won_value") if rows2 else None
        check(
            "moving the rate does NOT change the won deal's reported value (stored rate wins)",
            close(won_after, won_before) and close(rep_won_after, rep_won_before),
            f"before {won_before}/{rep_won_before} after {won_after}/{rep_won_after}",
        )
        st, detail = call(args.base, "GET", f"/api/deals/{eur['id']}", token=token)
        stored = (detail.get("deal") or {})
        check(
            "the deal still carries the rate it was written with",
            close(stored.get("exchange_rate") or 0, 1.08695652, 1e-6),
            f"rate={stored.get('exchange_rate')}",
        )
        psql(f"delete from fx_rates where source='proof-shock'")

        # ── 5. CONDITION 4: a money custom field carries its own currency ───────────────
        st, fld = call(
            args.base, "POST", "/api/deals/custom-fields", token=token,
            body={"key": "deposit", "label": "Deposit", "field_type": "money"},
        )
        check("a money custom field can be defined", st in (200, 201), f"HTTP {st} {fld}")
        st, _ = call(
            args.base, "PATCH", f"/api/deals/{gbp['id']}", token=token,
            body={"custom_fields": {"deposit": {"amount": 250, "currency": "GBP"}}},
        )
        check("a money value can be written", st == 200, f"HTTP {st}")
        st, detail = call(args.base, "GET", f"/api/deals/{gbp['id']}", token=token)
        money = None
        for f in detail.get("custom_fields") or []:
            if f.get("key") == "deposit":
                money = f
        check(
            "the money field persists with its OWN currency and stored rate",
            st == 200 and money is not None
            and money.get("field_type") == "money"
            and close(money.get("money_amount") or -1, 250)
            and (money.get("money_currency") or "") == "GBP"
            and close(money.get("money_exchange_rate") or 0, 1.26582278, 1e-6)
            and (money.get("money_reporting_currency") or "") == "USD",
            f"HTTP {st} money={money}",
        )
        check(
            "the deal form still loads with the money field on it (no break)",
            st == 200 and (detail.get("deal") or {}).get("id") == gbp["id"],
            f"HTTP {st}",
        )

        # ── 6. same-currency deal stores rate 1 with a date ─────────────────────────────
        st, d_usd = call(
            args.base, "POST", "/api/deals", token=token,
            body={"name": "USD deal", "value": 100, "currency": "USD", "pipeline_id": pid, "stage_id": sid},
        )
        usd = d_usd.get("deal") or d_usd
        deal_ids.append(usd.get("id"))
        check(
            "a deal already in the reporting currency stores rate 1 and a rate date",
            st in (200, 201) and close(usd.get("exchange_rate") or 0, 1.0, 1e-9) and usd.get("rate_date"),
            f"HTTP {st} rate={usd.get('exchange_rate')}",
        )
    finally:
        # ── cleanup: the probe workspace and everything it holds ─────────────────────────
        psql(f"delete from fx_rates where source='proof-shock'")
        psql(f"delete from tenants where id='{tenant}'")
        residue = psql(f"select count(*) from tenants where slug like 'mc-probe-%'")
        check("probe workspace removed (no residue)", residue == "0", f"residue={residue}")

    failed = [c for c in CHECKS if not c[1]]
    print(f"\n{len(CHECKS) - len(failed)} of {len(CHECKS)} checks passed")
    if failed:
        for name, _, detail in failed:
            print(f"FAILED: {name}  {detail}")
        sys.exit(1)


if __name__ == "__main__":
    main()
