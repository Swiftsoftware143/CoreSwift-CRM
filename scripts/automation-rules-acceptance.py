#!/usr/bin/env python3
"""automation-rules-acceptance — the rule-config surface proof gate (kanban t_990c386e).

Driven over real HTTP against a RUNNING deployment, on throwaway tenants (`probe_harness`), so no
customer row is read or written. Six checks, one per acceptance item on the card:

  A. a draft rule with tag trigger + template + delay + no-reply condition is created and reads back
     with is_active=false and the full trigger_config / action_config
  B. the six rule SHAPES the approved ZaarHub drafts need are all expressible, as drafts
  C. nothing armed or sent: active-rule and outbound-message counts unchanged
  D. the no-reply condition both ways: a contact with no reply executes, a contact who replied does
     not — with a NON-SENDING action (AddTag), and the queue row records why
  E. no regression: tag, list, score and stage triggers still match and still run their action
  P. PAUSE (kanban t_b54589a3): pause cancels a step that is still waiting, the executor refuses a
     paused rule even for a row that escaped the sweep, and resuming affects future triggers only
  (F is the guide, checked by eye; G is the deploy.)

The script deletes its tenants at the end (a cascade takes rules, contacts, tags, lists, templates,
queue rows and assignments with them) and asserts the baseline came back.

    python3 scripts/automation-rules-acceptance.py
    python3 scripts/automation-rules-acceptance.py --base https://coreswiftcrm.com --container coreswiftcrm

Exit code 0 only when every check passed.
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


def http(method, url, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", "coreswift-automation-rules-acceptance/1")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=45) as r:
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


def jwt(secret, sub, aid, role="owner", ttl=900):
    now = int(time.time())
    head = b64(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    body = b64(json.dumps({
        "sub": sub, "aid": aid, "role": role, "iat": now, "exp": now + ttl,
        "iss": "coreswift", "aud": "coreswift-api",
    }, separators=(",", ":")).encode())
    sig = b64(hmac.new(secret.encode(), f"{head}.{body}".encode(), hashlib.sha256).digest())
    return f"{head}.{body}.{sig}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    ap.add_argument("--container", default="coreswiftcrm")
    ap.add_argument("--skip-stage", action="store_true", help="skip the stage-trigger arm (E4)")
    args = ap.parse_args()
    base = args.base.rstrip("/")
    db = Db(args.container)
    stamp = str(int(time.time()))[-6:]
    tenants = []

    def new_tenant(tag):
        tid = db.q(
            "INSERT INTO tenants (name, slug, probe_harness) "
            f"VALUES ('Probe Rule Surface {tag}', 'probe-rules-{tag}-{stamp}', "
            "'automation-rules-acceptance') RETURNING id"
        ).splitlines()[0]
        tenants.append(tid)
        return tid

    def new_user(tid, tag):
        return db.q(
            "INSERT INTO users (tenant_id, email, password_hash, name) "
            f"VALUES ('{tid}', 'probe-rules-{tag}-{stamp}@example.test', 'x', 'Probe {tag}') "
            "RETURNING id"
        ).splitlines()[0]

    def counts():
        active = db.q("SELECT count(*) FROM automation_rules WHERE is_active IS NOT FALSE")
        out = db.q("SELECT count(*) FROM outbound_messages")
        return active, out

    def seed(tid, session):
        """A tag, two action tags, a list, a template and two contacts — everything a rule needs."""
        out = {}
        for tag in ("Probe VIP", "Probe Action One", "Probe Action Two"):
            st, b = http("POST", f"{base}/api/tags", {"name": f"{tag} {stamp}", "color": "#3366ff"}, session)
            out[tag] = (b or {}).get("id")
            if st not in (200, 201) or not out[tag]:
                raise RuntimeError(f"tag {tag} create failed: {st} {b}")
        st, b = http("POST", f"{base}/api/lists", {"name": f"Probe List {stamp}"}, session)
        if st not in (200, 201):
            raise RuntimeError(f"list create failed: {st} {b}")
        out["list"] = b.get("id")
        st, b = http("POST", f"{base}/api/comms/templates",
                     {"name": f"Probe Template {stamp}", "channel": "email",
                      "subject": f"Probe subject {stamp}", "body": "Hello {{first_name}}, probe body."},
                     session)
        if st not in (200, 201):
            raise RuntimeError(f"template create failed: {st} {b}")
        out["template"] = b.get("id")
        out["contacts"] = []
        for i in (1, 2, 3, 4):
            st, b = http("POST", f"{base}/api/contacts",
                         {"first_name": f"Probe{i}", "last_name": f"Rule{stamp}",
                          "email": f"probe-rules-{stamp}-{i}@example.test"}, session)
            if st not in (200, 201):
                raise RuntimeError(f"contact {i} create failed: {st} {b}")
            out["contacts"].append(b["id"])
        return out

    def create_rule(session, body):
        return http("POST", f"{base}/api/automation/rules", body, session)

    before_active, before_out = counts()
    try:
        a = new_tenant("a")
        user_a = new_user(a, "a")
        session_a = jwt(db.secret, user_a, a)
        seed_a = seed(a, session_a)

        # ── A. the composed draft ──────────────────────────────────────────────────────────────
        draft_body = {
            "name": f"Probe: no reply in 2 days {stamp}",
            "trigger_type": "TagAdded",
            "trigger_config": {"tag_id": seed_a["Probe VIP"], "delay_minutes": 2880,
                               "condition": {"type": "no_reply"}},
            "action_type": "send_email",
            "action_config": {"template_id": seed_a["template"]},
            "is_active": False,
        }
        st, created = create_rule(session_a, draft_body)
        rid = (created or {}).get("id")
        st2, listing = http("GET", f"{base}/api/automation/rules", None, session_a)
        row = next((r for r in (listing or {}).get("rules", []) if r.get("id") == rid), None)
        print("\n--- A. GET /api/automation/rules row for the composed draft ---")
        print(json.dumps(row, indent=2, sort_keys=True))
        ok = (
            st == 201 and row is not None
            and row.get("is_active") is False
            and row["trigger_config"].get("tag_id") == seed_a["Probe VIP"]
            and row["trigger_config"].get("delay_minutes") == 2880
            and row["trigger_config"].get("condition", {}).get("type") == "no_reply"
            and row["action_config"].get("template_id") == seed_a["template"]
        )
        check("A draft rule stores tag trigger + template + delay + no-reply, is_active=false", ok,
              f"status={st} row={json.dumps(row)[:300]}")
        # The same rule must NOT be visible to the engine as runnable.
        active_ids = db.q(
            "SELECT count(*) FROM automation_rules "
            f"WHERE id='{rid}' AND is_active IS NOT FALSE"
        )
        check("A2 the draft is not a runnable rule for the engine", active_ids == "0",
              f"active_rows={active_ids}")

        # ── B. the six approved shapes, as drafts ──────────────────────────────────────────────
        six = [
            ("welcome", {"trigger_type": "contact.created", "trigger_config": {},
                         "action_type": "send_email", "action_config": {"template_id": seed_a["template"]}}),
            ("win-back", {"trigger_type": "contact.inactive", "trigger_config": {"days": 60},
                          "action_type": "AddTag", "action_config": {"tag_id": seed_a["Probe Action One"]}}),
            ("instant new-lead follow-up",
             {"trigger_type": "contact.created", "trigger_config": {"delay_minutes": 5},
              "action_type": "send_email", "action_config": {"template_id": seed_a["template"]}}),
            ("no-reply sequence",
             {"trigger_type": "TagAdded",
              "trigger_config": {"tag_id": seed_a["Probe VIP"], "delay_minutes": 4320,
                                 "condition": {"type": "no_reply"}},
              "action_type": "send_email", "action_config": {"template_id": seed_a["template"]}}),
            ("proposal follow-up",
             {"trigger_type": "StageChanged", "trigger_config": {"delay_minutes": 10080},
              "action_type": "NotifyUser", "action_config": {"message": "Chase the proposal"}}),
            ("renewal reminder",
             {"trigger_type": "date.renewal",
              "trigger_config": {"field": "renewal_date", "days_before": 30, "delay_minutes": 0},
              "action_type": "send_email", "action_config": {"template_id": seed_a["template"]}}),
        ]
        print("\n--- B. six approved rule shapes created as INACTIVE drafts ---")
        six_ok = True
        six_ids = []
        for name, extra in six:
            body = {"name": f"Probe {name} {stamp}", "is_active": False, **extra}
            st, b = create_rule(session_a, body)
            got = (b or {})
            six_ids.append(got.get("id"))
            six_ok = six_ok and st == 201 and got.get("is_active") is False
            print(json.dumps({
                "rule": name, "status": st, "is_active": got.get("is_active"),
                "trigger_type": got.get("trigger_type"), "trigger_config": got.get("trigger_config"),
                "action_type": got.get("action_type"), "action_config": got.get("action_config"),
            }, sort_keys=True))
        check("B all six approved shapes exist as inactive drafts", six_ok)
        # Remove the six probes (the composed draft stays for D).
        for sid in six_ids:
            if sid:
                http("DELETE", f"{base}/api/automation/rules/{sid}", None, session_a)

        # ── C. nothing armed, nothing sent ─────────────────────────────────────────────────────
        mid_active, mid_out = counts()
        out_after_drafts = db.q(
            "SELECT count(*) FROM outbound_messages WHERE tenant_id='" + a + "'")
        print(f"\n--- C. counts: active rules {before_active} -> {mid_active}; "
              f"outbound_messages {before_out} -> {mid_out}; probe tenant outbound {out_after_drafts} ---")
        check("C no rule was armed by creating drafts", mid_active == before_active,
              f"{before_active} -> {mid_active}")
        check("C2 no outbound message was created", mid_out == before_out, f"{before_out} -> {mid_out}")
        check("C3 the probe tenant has zero outbound messages", out_after_drafts == "0",
              out_after_drafts)

        # ── D. the no-reply condition, both ways, with a non-sending action ────────────────────
        c_noreply, c_replied = seed_a["contacts"][0], seed_a["contacts"][1]
        email_replied = db.q(f"SELECT email FROM contacts WHERE id='{c_replied}'")
        db.q(
            "INSERT INTO private_email_messages (id, tenant_id, address, direction, from_address, "
            "to_address, subject, body_plain, received_at) VALUES "
            f"(gen_random_uuid(), '{a}', 'probe-inbox@example.test', 'inbound', "
            f"'{email_replied}', 'probe-inbox@example.test', 'Re: probe', 'Yes please', now())"
        )
        # Two queue rows on the SAME rule, each with its own action tag, so "executed" and
        # "suppressed" are distinguishable by which tag landed on which contact. The rule must be
        # LIVE here: since kanban t_b54589a3 the executor re-reads `is_active` and a draft can never
        # deliver a wait (that IS the fix), so this arm activates it and restores the draft after.
        http("PATCH", f"{base}/api/automation/rules/{rid}", {"is_active": True}, session_a)
        tag_one, tag_two = seed_a["Probe Action One"], seed_a["Probe Action Two"]
        rows = {}
        for label, cid, tag in (("no_reply", c_noreply, tag_one), ("replied", c_replied, tag_two)):
            cfg_json = json.dumps({
                "contact_id": cid, "entity_id": cid, "entity_type": "contact",
                "since": time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(time.time() - 60)),
            }).replace("'", "''")
            tag_json = json.dumps({"tag_id": tag}).replace("'", "''")
            da_id = db.q(
                "INSERT INTO delayed_actions (id, tenant_id, source_rule_id, action_type, action_config, "
                "entity_type, entity_id, condition_type, condition_config, execute_at, created_at, updated_at) "
                f"VALUES (gen_random_uuid(), '{a}', '{rid}', 'AddTag', '{tag_json}'::jsonb, 'contact', "
                f"'{cid}', 'no_reply', '{cfg_json}'::jsonb, now(), now(), now()) RETURNING id"
            )
            rows[label] = (da_id.splitlines()[0], cid, tag)
        # The worker pass runs every 60s; wait up to 3 minutes for both rows to be evaluated.
        deadline = time.time() + 190
        while time.time() < deadline:
            done = db.q(
                "SELECT count(*) FROM delayed_actions WHERE id IN ("
                + ",".join(f"'{v[0]}'" for v in rows.values()) + ") AND executed"
            )
            if done == "2":
                break
            time.sleep(5)
        print("\n--- D. queue rows after the worker pass ---")
        for label, (da_id, cid, tag) in rows.items():
            raw = db.q(
                "SELECT json_build_object('id',id,'condition_type',condition_type,"
                "'executed',executed,'result',result,'entity_id',entity_id) FROM delayed_actions "
                f"WHERE id='{da_id}'"
            )
            assigned = db.q(
                f"SELECT count(*) FROM tag_assignments WHERE tag_id='{tag}' AND entity_id='{cid}'")
            print(label + ": " + raw)
            print(label + " action applied (tag_assignments rows): " + assigned)
            rows[label] = (da_id, cid, tag, raw, assigned)
        no_reply_ok = ("\"executed\": true" in rows["no_reply"][3] and rows["no_reply"][4] == "1")
        replied_ok = ("\"executed\": false" in rows["replied"][3] and rows["replied"][4] == "0")
        check("D contact with no reply -> the queued action ran", no_reply_ok)
        check("D2 contact who replied -> the queued action did NOT run", replied_ok)
        # Back to a draft: the rest of the card's claims are about a rule that is not armed.
        http("PATCH", f"{base}/api/automation/rules/{rid}", {"is_active": False}, session_a)

        # ── E. no regression on the four existing trigger kinds ───────────────────────────────
        reg_tags = {}
        for tag in ("Probe Reg Tag", "Probe Reg List", "Probe Reg Score", "Probe Reg Stage"):
            st, b = http("POST", f"{base}/api/tags", {"name": f"{tag} {stamp}"}, session_a)
            reg_tags[tag] = b.get("id")

        # E1 tag trigger
        st, tag_rule = create_rule(session_a, {
            "name": f"Probe regression tag {stamp}", "trigger_type": "TagAdded",
            "trigger_config": {"tag_id": seed_a["Probe VIP"]}, "action_type": "AddTag",
            "action_config": {"tag_id": reg_tags["Probe Reg Tag"]}, "is_active": True})
        http("POST", f"{base}/api/tags/assign",
             {"tag_id": seed_a["Probe VIP"], "entity_type": "contact", "entity_id": c_noreply},
             session_a)
        e1 = db.q(f"SELECT count(*) FROM tag_assignments WHERE tag_id='{reg_tags['Probe Reg Tag']}' "
                  f"AND entity_id='{c_noreply}'")
        e1_count = db.q(f"SELECT execution_count FROM automation_rules WHERE id='{tag_rule.get('id')}'")
        print(f"\n--- E1 tag trigger: assigned={e1} execution_count={e1_count} ---")
        check("E1 TagAdded still matches and runs its action", e1 == "1" and e1_count != "0",
              f"assigned={e1} count={e1_count}")

        # E2 list trigger
        st, list_rule = create_rule(session_a, {
            "name": f"Probe regression list {stamp}", "trigger_type": "ListAdded",
            "trigger_config": {"list_id": seed_a["list"]}, "action_type": "AddTag",
            "action_config": {"tag_id": reg_tags["Probe Reg List"]}, "is_active": True})
        st, b = http("POST", f"{base}/api/lists/{seed_a['list']}/members",
                     {"contact_id": seed_a["contacts"][2]}, session_a)
        e2 = db.q(f"SELECT count(*) FROM tag_assignments WHERE tag_id='{reg_tags['Probe Reg List']}' "
                  f"AND entity_id='{seed_a['contacts'][2]}'")
        print(f"--- E2 list trigger: member_status={st} assigned={e2} ---")
        check("E2 ListAdded still matches and runs its action", e2 == "1", f"assigned={e2}")

        # E3 score trigger — a scoring rule moves the category, which is what fires ScoreChanged.
        http("POST", f"{base}/api/scoring/rules",
             {"name": f"Probe score rule {stamp}", "event_type": "probe.event", "points": 60,
              "direction": "add"}, session_a)
        st, score_rule = create_rule(session_a, {
            "name": f"Probe regression score {stamp}", "trigger_type": "ScoreChanged",
            "trigger_config": {"min_score": 0}, "action_type": "AddTag",
            "action_config": {"tag_id": reg_tags["Probe Reg Score"]}, "is_active": True})
        st, sb = http("POST", f"{base}/api/scoring/calculate/{seed_a['contacts'][3]}",
                      {"event_type": "probe.event"}, session_a)
        e3 = db.q(f"SELECT count(*) FROM tag_assignments WHERE tag_id='{reg_tags['Probe Reg Score']}' "
                  f"AND entity_id='{seed_a['contacts'][3]}'")
        print(f"--- E3 score trigger: calc_status={st} category={(sb or {}).get('category')} assigned={e3} ---")
        check("E3 ScoreChanged still matches and runs its action", e3 == "1", f"assigned={e3}")

        # E4 stage trigger — a deal moved onto a stage whose hook is `workflow`.
        if not args.skip_stage:
            st, p = http("POST", f"{base}/api/pipelines", {"name": f"Probe Pipeline {stamp}"}, session_a)
            pipeline_id = (p or {}).get("id")
            st, s1 = http("POST", f"{base}/api/pipelines/{pipeline_id}/stages",
                          {"name": "Probe A", "position": 1, "probability": 10}, session_a)
            st, s2 = http("POST", f"{base}/api/pipelines/{pipeline_id}/stages",
                          {"name": "Probe B", "position": 2, "probability": 50}, session_a)
            stage_a, stage_b = (s1 or {}).get("id"), (s2 or {}).get("id")
            st, needle = http("POST", f"{base}/api/deals", {
                "name": f"Probe Deal {stamp}", "pipeline_id": pipeline_id, "stage_id": stage_a,
                "contact_id": seed_a["contacts"][0], "value": 100,
                "currency": "USD"}, session_a)
            deal_id = (needle or {}).get("deal", {}).get("id") or (needle or {}).get("id")
            http("POST", f"{base}/api/deals/stage-automations",
                 {"stage_id": stage_b, "action_kind": "workflow"}, session_a)
            st, stage_rule = create_rule(session_a, {
                "name": f"Probe regression stage {stamp}", "trigger_type": "StageChanged",
                "trigger_config": {"stage_id": stage_b}, "action_type": "AddTag",
                "action_config": {"tag_id": reg_tags["Probe Reg Stage"]}, "is_active": True})
            st, mv = http("POST", f"{base}/api/deals/{deal_id}/stage", {"stage_id": stage_b}, session_a)
            e4 = db.q(f"SELECT count(*) FROM tag_assignments WHERE tag_id='{reg_tags['Probe Reg Stage']}' "
                      f"AND entity_id='{seed_a['contacts'][0]}'")
            print(f"--- E4 stage trigger: move_status={st} deal={deal_id} assigned={e4} ---")
            check("E4 StageChanged still matches and runs its action", e4 == "1", f"assigned={e4}")

        # ── P. Pause cancels a step already in the queue, and the executor refuses a paused rule ──
        # (kanban t_b54589a3). Two belts, both proved separately:
        #   P1-P3 the PATCH cancels the waiting row (half 1)
        #   P4-P6 a row that escaped the sweep (not cancelled) still does not fire, because the
        #         executor re-reads the rule's is_active (half 2)
        #   P7-P8 resuming affects FUTURE triggers only: a new trigger queues, the cancelled row
        #         does not resurrect
        pause_trigger = http("POST", f"{base}/api/tags", {"name": f"Probe Pause Trigger {stamp}"}, session_a)[1].get("id")
        pause_action = http("POST", f"{base}/api/tags", {"name": f"Probe Pause Action {stamp}"}, session_a)[1].get("id")
        st, pause_rule = create_rule(session_a, {
            "name": f"Probe pause cancels wait {stamp}", "trigger_type": "TagAdded",
            "trigger_config": {"tag_id": pause_trigger, "delay_minutes": 30},
            "action_type": "AddTag", "action_config": {"tag_id": pause_action}, "is_active": True})
        pause_rid = (pause_rule or {}).get("id")
        c_pause, c_resume = seed_a["contacts"][0], seed_a["contacts"][3]

        # P1: a real trigger queues the waiting step.
        st, _ = http("POST", f"{base}/api/tags/assign",
                     {"tag_id": pause_trigger, "entity_type": "contact", "entity_id": c_pause}, session_a)
        pending = db.q(
            "SELECT id FROM delayed_actions WHERE source_rule_id='" + str(pause_rid) + "' "
            "AND executed = false AND cancelled = false")
        da_pause = pending.splitlines()[0] if pending else ""
        print(f"\n--- P1 trigger (assign status {st}) queued pending row: {da_pause or 'NONE'} ---")
        check("P1 a real trigger queues a pending delayed-action row for the rule", bool(da_pause), pending)

        # P2: Pause cancels it, in the same request, and says how many.
        st, paused = http("PATCH", f"{base}/api/automation/rules/{pause_rid}", {"is_active": False}, session_a)
        returned = (paused or {}).get("cancelled_pending")
        print(f"--- P2 PATCH is_active=false -> status={st} cancelled_pending={returned} ---")
        check("P2 pause reports the cancelled step (cancelled_pending == 1)", returned == 1,
              f"status={st} body={json.dumps(paused)[:300]}")

        # P3: the row is cancelled in the DB, with the reason on it, and the rule is inactive.
        state = db.q(
            "SELECT jsonb_build_object('cancelled',cancelled,'status',status,'reason',result->>'reason',"
            "'executed',executed) FROM delayed_actions WHERE id='" + da_pause + "'")
        rule_state = db.q("SELECT is_active FROM automation_rules WHERE id='" + str(pause_rid) + "'")
        print(f"--- P3 queue row: {state} rule is_active={rule_state} ---")
        check("P3 the waiting row is cancelled and records 'rule paused - not sent'",
              '"cancelled": true' in state and '"rule paused - not sent"' in state and rule_state == "f",
              state)

        # P4: the executor belt. Force the row due and un-cancel it by hand (simulating a row that
        # escaped the sweep, or one queued before this fix), so the ONLY thing stopping it is the
        # rule's is_active re-read inside evaluate_delayed_action.
        db.q("UPDATE delayed_actions SET cancelled = false, status = NULL, result = NULL, "
             "execute_at = now() - interval '1 minute' WHERE id='" + da_pause + "'")
        deadline = time.time() + 190
        while time.time() < deadline:
            if db.q("SELECT executed FROM delayed_actions WHERE id='" + da_pause + "'") == "t":
                break
            time.sleep(5)
        p4 = db.q(
            "SELECT jsonb_build_object('executed',executed,'cancelled',cancelled,"
            "'result',result) FROM delayed_actions WHERE id='" + da_pause + "'")
        assigned = db.q(
            f"SELECT count(*) FROM tag_assignments WHERE tag_id='{pause_action}' AND entity_id='{c_pause}'")
        print(f"--- P4 after the worker pass on a due, un-cancelled row: {p4}; action tag rows={assigned} ---")
        check("P4 the worker evaluated the row but the paused rule did not fire",
              '"executed": true' in p4 and '"executed": false' in p4
              and '"rule paused - not sent"' in p4 and assigned == "0", p4)
        check("P5 no tag assignment was applied by the paused rule", assigned == "0", assigned)

        # P6: resume — the cancelled row must not resurrect, and a NEW trigger must queue again.
        st, resumed = http("PATCH", f"{base}/api/automation/rules/{pause_rid}", {"is_active": True}, session_a)
        st2, _ = http("POST", f"{base}/api/tags/assign",
                      {"tag_id": pause_trigger, "entity_type": "contact", "entity_id": c_resume}, session_a)
        new_pending = db.q(
            "SELECT count(*) FROM delayed_actions WHERE source_rule_id='" + str(pause_rid) + "' "
            "AND entity_id='" + c_resume + "' AND executed = false AND cancelled = false")
        old_row = db.q("SELECT jsonb_build_object('cancelled',cancelled,'executed',executed) "
                       "FROM delayed_actions WHERE id='" + da_pause + "'")
        assigned_old = db.q(
            f"SELECT count(*) FROM tag_assignments WHERE tag_id='{pause_action}' AND entity_id='{c_pause}'")
        print(f"--- P6 resume status={st} new trigger status={st2} new pending={new_pending} "
              f"old row={old_row} old action rows={assigned_old} ---")
        check("P6 resumed rule queues a NEW trigger", new_pending == "1", new_pending)
        check("P7 the row cancelled while paused did not resurrect or run",
              '"executed": true' in old_row and assigned_old == "0", old_row)

        # Leave the probe rule inactive so the final baseline count is honest even before the
        # tenant cascade runs.
        http("PATCH", f"{base}/api/automation/rules/{pause_rid}", {"is_active": False}, session_a)

        # Deactivate everything this script armed before the final count.
        for rule in (tag_rule, list_rule, score_rule):
            if rule.get("id"):
                http("PATCH", f"{base}/api/automation/rules/{rule['id']}", {"is_active": False}, session_a)

    finally:
        # Delete every probe tenant: the cascade removes the rules, contacts, tags, lists, templates,
        # queue rows, assignments, deals and pipelines this run created.
        for tid in tenants:
            subprocess.run(["psql", db.url, "-c", f"DELETE FROM tenants WHERE id='{tid}'"],
                           capture_output=True, text=True)
        after_active, after_out = counts()
        print(f"\n--- baseline: active rules {before_active} -> {after_active}; "
              f"outbound_messages {before_out} -> {after_out} ---")
        check("C4 active-rule count is back to the baseline", before_active == after_active,
              f"{before_active} -> {after_active}")
        check("C5 outbound_messages count is back to the baseline", before_out == after_out,
              f"{before_out} -> {after_out}")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    if FAIL:
        print("FAILED: " + ", ".join(FAIL))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
