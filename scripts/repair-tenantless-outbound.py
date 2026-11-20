#!/usr/bin/env python3
"""repair-tenantless-outbound.py — keep CoreSwift's failed-send alert alive.

Measured 2026-11-20 (CoreSwift-CRM): `alerts::send_failures` selects
`o.tenant_id` as a non-optional `Uuid`. The platform's operator alerts
(`send_support_request_operator_alert`, `send_plan_request_operator_alert`)
deliberately record their row with `tenant_id = NULL` ("this alert belongs to
no workspace"). One such message reaching `status='failed'` therefore makes the
WHOLE sweep fail to decode — every 60 seconds — and nobody is told about any
failed customer mail until that row ages out of the 7-day window.

Evidence measured that day: 19 + `could not read failed sends ... unexpected
null` lines per 24 h, `send_failure_alerts` frozen at 7 rows since 12:32, and
`notifications` for `email_failure` stuck at 1.

This is the DB-side repair (no Rust touched — the owning lane had a live writer
in that tree): a tenant-less platform mail is attributed to the operator's own
workspace, which is where it was sent for. The sweep then decodes it, classifies
it (a probe address is WITHHELD, a real one pages the workspace owner) and
records it in its own ledger.

Idempotent: a second run finds nothing. Read-only by default; `--apply` writes.
"""
import argparse, os, re, subprocess, sys

ENV = "/etc/swift/env/coreswift.env"
OPERATOR_WORKSPACE = "7c1f787c-6f10-4c4d-ac24-71b82c38934e"  # tenant 'SwiftSoftware'


def dsn():
    m = re.search(r"^DATABASE_URL=(.*)$", open(ENV).read(), re.M)
    if not m:
        sys.exit("no DATABASE_URL in " + ENV)
    return m.group(1).strip().strip('"').strip("'")


def sql(url, q, tuples_only=True):
    args = ["psql", url, "-At", "-F", "\t", "-c", q]
    p = subprocess.run(args, capture_output=True, text=True)
    if p.returncode != 0:
        sys.exit("SQL failed: " + p.stderr.strip().splitlines()[0][:300])
    return p.stdout.strip() if tuples_only else p.stdout


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true", help="write the repair (default: report only)")
    args = ap.parse_args()
    url = dsn()

    op = sql(url, "select name from tenants where id = '%s'" % OPERATOR_WORKSPACE)
    if not op:
        sys.exit("operator workspace %s not found" % OPERATOR_WORKSPACE)

    rows = sql(url, """
        select id || ' ' || status || ' ' || coalesce(left(to_address, 42), '-')
          from outbound_messages where tenant_id is null order by created_at""")
    ids = [r.split()[0] for r in rows.splitlines() if r.strip()]
    print("tenant-less outbound rows: %d" % len(ids))
    for r in rows.splitlines():
        print("  " + r)

    failed_unledgered = sql(url, """
        select count(*) from outbound_messages o
          left join send_failure_alerts a on a.message_id = o.id
         where o.status = 'failed' and a.message_id is null
           and o.created_at > now() - interval '7 days'""")
    print("failed sends still un-alerted (7d): %s" % failed_unledgered)

    if not args.apply:
        print("DRY RUN — rerun with --apply to attribute them to '%s'" % op)
        return
    if not ids:
        print("nothing to repair")
        return
    n = sql(url, "update outbound_messages set tenant_id = '%s' where tenant_id is null returning id"
            % OPERATOR_WORKSPACE)
    print("repaired: %d" % len([x for x in n.splitlines() if x.strip()]))


if __name__ == "__main__":
    main()
