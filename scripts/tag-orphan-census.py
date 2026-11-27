#!/usr/bin/env python3
"""Orphan tag-assignment census (kanban t_7a54cc6a).

`tag_assignments.entity_id` is polymorphic (`entity_type` + `entity_id`) and has no FK to `contacts`,
so nothing in the schema can stop an assignment outliving its contact. Tag counts now JOIN `contacts`
so a leftover cannot inflate a screen, and migration 224 deletes the children with the contact — but a
row can still appear if something deletes a contact with triggers disabled. This census makes that
visible per workspace; it is read-only and exits 1 when any orphan exists.

Usage: scripts/tag-orphan-census.py [--db postgres://...]
       (default: DATABASE_URL from /etc/swift/env/coreswift.env)
"""
import argparse
import subprocess
import sys

ORPHAN_SQL = """
SELECT tn.name AS workspace,
       tg.name AS tag,
       count(*) AS orphan_assignments
  FROM tag_assignments ta
  JOIN tags tg    ON tg.id = ta.tag_id
  JOIN tenants tn ON tn.id = tg.tenant_id
 WHERE ta.entity_type = 'contact'
   AND NOT EXISTS (SELECT 1 FROM contacts c WHERE c.id = ta.entity_id)
 GROUP BY tn.name, tg.name
 ORDER BY count(*) DESC, tn.name, tg.name
"""


def default_db():
    for line in open("/etc/swift/env/coreswift.env"):
        if line.startswith("DATABASE_URL="):
            return line.strip().split("=", 1)[1]
    raise SystemExit("DATABASE_URL not found in /etc/swift/env/coreswift.env")


ap = argparse.ArgumentParser()
ap.add_argument("--db", default=None)
args = ap.parse_args()
url = args.db or default_db()

p = subprocess.run(["psql", url, "-X", "-A", "-F", "\t", "-t", "-c", ORPHAN_SQL], capture_output=True, text=True)
if p.returncode != 0:
    raise SystemExit(f"census failed: {p.stderr.strip()}")

rows = [l.split("\t") for l in p.stdout.splitlines()
        if l.strip() and not l.startswith("(") and len(l.split("\t")) == 3]
if not rows:
    print("PASS  no orphan tag assignments (every contact assignment resolves to a live contact)")
    sys.exit(0)

per_ws = {}
for ws, tag, n in rows:
    per_ws[ws] = per_ws.get(ws, 0) + int(n)
print(f"FAIL  {sum(per_ws.values())} assignment(s) point at a deleted contact")
for ws, n in sorted(per_ws.items(), key=lambda kv: -kv[1]):
    print(f"  {ws}: {n}")
print("  per tag:")
for ws, tag, n in rows:
    print(f"    {ws} / {tag}: {n}")
sys.exit(1)
