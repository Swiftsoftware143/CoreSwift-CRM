//! Dynamic list membership: who belongs on a list that maintains itself.
//!
//! ## Rules are ANDed
//! Each rule is evaluated into its own set of contacts, and the sets are INTERSECTED. They used to be
//! unioned, which quietly turned "source = Dispatch App AND stage = Delivered" into "either" —
//! measured on a real account (2026-11-21): a brand-new dispatch quote appeared in *Delivered —
//! Awaiting Payment* the moment it was created. A list defined by two properties means both of them;
//! a list that wants either property is two lists, or one tag rule.
//!
//! ## Which rules exist
//! * `tag`         — the contact carries a tag (`value` = tag name).
//! * `score`       — the contact's score (`gte` / `lte`), or its category (`equals`).
//! * `deal_source` — the contact has a live deal from that source (`equals`). Deal-based lists are the
//!   reason these arms exist: "open quotes", "this week's jobs" and "delivered, unpaid" are properties
//!   of a DEAL, not of a tag somebody remembered to apply.
//! * `deal_stage`  — the contact has a live deal in one of these stages (`in` with an array, or `equals`).
//! * `deal_date`   — a date custom field on the deal is inside a window (`this_week`, `this_month`,
//!   `today`, otherwise "today or later"); `value` is the field key, e.g. `job_date`.
//!
//! Membership is always a CONTACT: a list member is a contact, so every deal-based rule resolves the
//! deal's `contact_id` and nothing else.

use super::models::{List, ListRule};
use crate::errors::AppError;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn evaluate_dynamic_list(
    db: &PgPool,
    list: &List,
) -> Result<serde_json::Value, AppError> {
    let rules: Vec<ListRule> = if let Some(ref r) = list.rules {
        serde_json::from_value(r.clone()).unwrap_or_default()
    } else {
        Vec::new()
    };
    if rules.is_empty() {
        return Ok(json!({"message":"No rules","added":0,"removed":0}));
    }

    // One set per rule, then the intersection (see the module docs).
    let mut per_rule: Vec<Vec<Uuid>> = Vec::with_capacity(rules.len());

    for rule in &rules {
        let set: Vec<Uuid> = match rule.field.as_str() {
            "tag" => {
                let tn = rule.value.as_str().unwrap_or("");
                // JOIN contacts: `tag_assignments.entity_id` is polymorphic with no FK to `contacts`, so a
                // deleted contact's assignment survives. Without this join a rule-based list would try to
                // add the dead id as a member — the INSERT then fails 23503 on list_members.contact_id and
                // the list silently misses the real members (kanban t_7a54cc6a).
                sqlx::query_scalar::<_, Uuid>(
                    "SELECT DISTINCT ta.entity_id FROM tag_assignments ta \
                       JOIN tags t ON t.id = ta.tag_id \
                       JOIN contacts c ON c.id = ta.entity_id AND c.tenant_id = ta.tenant_id \
                      WHERE ta.tenant_id = $1 AND ta.entity_type = 'contact' AND t.name = $2",
                )
                .bind(list.tenant_id)
                .bind(tn)
                .fetch_all(db)
                .await?
            }
            "score" => match rule.operator.as_str() {
                "gte" => {
                    let ms = rule.value.as_i64().unwrap_or(0) as i32;
                    sqlx::query_scalar::<_, Uuid>(
                        "SELECT contact_id FROM scores WHERE tenant_id = $1 AND total_score >= $2",
                    )
                    .bind(list.tenant_id)
                    .bind(ms)
                    .fetch_all(db)
                    .await?
                }
                "lte" => {
                    let ms = rule.value.as_i64().unwrap_or(0) as i32;
                    sqlx::query_scalar::<_, Uuid>(
                        "SELECT contact_id FROM scores WHERE tenant_id = $1 AND total_score <= $2",
                    )
                    .bind(list.tenant_id)
                    .bind(ms)
                    .fetch_all(db)
                    .await?
                }
                _ => {
                    let cat = rule.value.as_str().unwrap_or("cold");
                    sqlx::query_scalar::<_, Uuid>(
                        "SELECT contact_id FROM scores WHERE tenant_id = $1 AND category = $2::score_category",
                    )
                    .bind(list.tenant_id)
                    .bind(cat)
                    .fetch_all(db)
                    .await?
                }
            },
            // A CONTACT'S OWN FIELDS (2026-11-25). The rule vocabulary was deal-shaped only, so "everyone in
            // Palm Bay" or "everyone with job title Owner" could not be expressed at all — the two examples
            // every CRM's dynamic-list documentation leads with. The column is chosen from a fixed list here,
            // never from the rule, so a rule cannot name a column that is not in it.
            "contact_field" => {
                let column = rule
                    .value
                    .get("column")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                const ALLOWED: [&str; 9] = [
                    "city",
                    "state",
                    "country",
                    "job_title",
                    "title",
                    "company",
                    "source",
                    "score_category",
                    "gender",
                ];
                if !ALLOWED.contains(&column) {
                    return Err(crate::errors::AppError::Validation(format!(
                        "\"{column}\" is not a contact field a list can be built on; use one of: {}",
                        ALLOWED.join(", ")
                    )));
                }
                let wanted = rule
                    .value
                    .get("equals")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // NO CONCATENATED SQL. The column is a parameter here ($3) and the statement is a const
                // literal, because the fleet gate refuses format!-built SQL and is right to: it cannot tell a
                // whitelisted column from an injected one, and neither can a reader six months later. Each
                // allowed column gets its own OR arm, guarded by the column name AND the operator.
                const CONTACT_FIELD_SQL: &str = "SELECT id FROM contacts WHERE tenant_id = $1 AND ( \
                     ($4 = 'contains' AND ( \
                         ($3 = 'city'           AND lower(city)           LIKE '%' || lower($2) || '%') OR \
                         ($3 = 'state'          AND lower(state)          LIKE '%' || lower($2) || '%') OR \
                         ($3 = 'country'        AND lower(country)        LIKE '%' || lower($2) || '%') OR \
                         ($3 = 'job_title'      AND lower(job_title)      LIKE '%' || lower($2) || '%') OR \
                         ($3 = 'title'          AND lower(title)          LIKE '%' || lower($2) || '%') OR \
                         ($3 = 'company'        AND lower(company)        LIKE '%' || lower($2) || '%') OR \
                         ($3 = 'source'         AND lower(source)         LIKE '%' || lower($2) || '%') OR \
                         ($3 = 'score_category' AND lower(score_category) LIKE '%' || lower($2) || '%') OR \
                         ($3 = 'gender'         AND lower(gender)         LIKE '%' || lower($2) || '%') )) \
                     OR ($4 <> 'contains' AND ( \
                         ($3 = 'city'           AND lower(city)           = lower($2)) OR \
                         ($3 = 'state'          AND lower(state)          = lower($2)) OR \
                         ($3 = 'country'        AND lower(country)        = lower($2)) OR \
                         ($3 = 'job_title'      AND lower(job_title)      = lower($2)) OR \
                         ($3 = 'title'          AND lower(title)          = lower($2)) OR \
                         ($3 = 'company'        AND lower(company)        = lower($2)) OR \
                         ($3 = 'source'         AND lower(source)         = lower($2)) OR \
                         ($3 = 'score_category' AND lower(score_category) = lower($2)) OR \
                         ($3 = 'gender'         AND lower(gender)         = lower($2)) )) )";
                sqlx::query_scalar::<_, Uuid>(CONTACT_FIELD_SQL)
                    .bind(list.tenant_id)
                    .bind(&wanted)
                    .bind(column)
                    .bind(&rule.operator)
                    .fetch_all(db)
                    .await?
            }
            "deal_source" => {
                let src = rule.value.as_str().unwrap_or("");
                sqlx::query_scalar::<_, Uuid>(
                    "SELECT DISTINCT o.contact_id FROM opportunities o \
                      WHERE o.tenant_id = $1 AND o.contact_id IS NOT NULL AND o.source = $2 \
                        AND o.is_lost IS NOT TRUE",
                )
                .bind(list.tenant_id)
                .bind(src)
                .fetch_all(db)
                .await?
            }
            "deal_stage" => {
                // Stages are matched by NAME: that is what the owner types and what the board shows,
                // so a renamed stage drops out of the list instead of silently matching by id.
                let stages: Vec<String> = match rule.value.as_array() {
                    Some(a) => a
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                    None => rule
                        .value
                        .as_str()
                        .map(|v| vec![v.to_string()])
                        .unwrap_or_default(),
                };
                if stages.is_empty() {
                    Vec::new()
                } else {
                    sqlx::query_scalar::<_, Uuid>(
                        "SELECT DISTINCT o.contact_id FROM opportunities o \
                           JOIN pipeline_stages st ON st.id = o.stage_id \
                          WHERE o.tenant_id = $1 AND o.contact_id IS NOT NULL \
                            AND st.name = ANY($2) AND o.is_lost IS NOT TRUE",
                    )
                    .bind(list.tenant_id)
                    .bind(&stages)
                    .fetch_all(db)
                    .await?
                }
            }
            "deal_date" => {
                // The window travels as a BIND and is resolved with CASE inside ONE constant
                // statement: the deploy gate refuses run-time-built SQL, and rightly so.
                //
                // A JUNK VALUE MUST NOT EMPTY EVERY LIST (2026-11-21): the cast was `d.value::date`,
                // and one deal whose date field held "22" made the whole evaluation fail with
                // `date/time field value out of range` — so every list refresh for the account
                // silently stopped updating. The value is now shape-checked and parsed with to_date,
                // and unparseable dates simply do not match instead of breaking the refresh.
                let field_key = rule.value.as_str().unwrap_or("job_date");
                sqlx::query_scalar::<_, Uuid>(
                    "SELECT DISTINCT o.contact_id \
                       FROM opportunities o \
                       JOIN deal_field_values d ON d.opportunity_id = o.id \
                       JOIN deal_custom_fields f ON f.id = d.field_id \
                      WHERE o.tenant_id = $1 AND o.contact_id IS NOT NULL \
                        AND f.key = $2 AND d.value IS NOT NULL AND d.value <> '' \
                        AND d.value ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}' \
                        AND o.is_lost IS NOT TRUE \
                        AND CASE $3 \
                              WHEN 'this_week' THEN to_date(d.value, 'YYYY-MM-DD') \
                                   BETWEEN date_trunc('week', now())::date \
                                       AND (date_trunc('week', now()) + interval '6 days')::date \
                              WHEN 'this_month' THEN to_date(d.value, 'YYYY-MM-DD') \
                                   BETWEEN date_trunc('month', now())::date \
                                       AND (date_trunc('month', now()) + interval '1 month - 1 day')::date \
                              WHEN 'today' THEN to_date(d.value, 'YYYY-MM-DD') = current_date \
                              ELSE to_date(d.value, 'YYYY-MM-DD') >= current_date \
                            END",
                )
                .bind(list.tenant_id)
                .bind(field_key)
                .bind(rule.operator.as_str())
                .fetch_all(db)
                .await?
            }
            other => {
                tracing::warn!(
                    field = other,
                    "unknown dynamic list rule field — this rule matches nobody, so the list is empty"
                );
                Vec::new()
            }
        };
        per_rule.push(set);
    }

    // Intersection. A rule that matched nobody empties the list, which is the honest reading of
    // "every one of these must be true".
    let mut matched: Vec<Uuid> = per_rule.first().cloned().unwrap_or_default();
    for set in per_rule.iter().skip(1) {
        matched.retain(|id| set.contains(id));
    }
    matched.sort();
    matched.dedup();

    let existing: Vec<Uuid> =
        sqlx::query_scalar("SELECT contact_id FROM list_members WHERE list_id=$1 AND tenant_id=$2")
            .bind(list.id)
            .bind(list.tenant_id)
            .fetch_all(db)
            .await?;

    let to_add: Vec<Uuid> = matched
        .iter()
        .filter(|id| !existing.contains(id))
        .copied()
        .collect();
    let to_remove: Vec<Uuid> = existing
        .iter()
        .filter(|id| !matched.contains(id))
        .copied()
        .collect();

    // THE RULES MUST ANNOUNCE THEMSELVES (2026-11-25). `ListAdded` / `ListRemoved` were emitted only from
    // the manual add/remove handlers and from booking, so a contact who joined a dynamic list BECAUSE THE
    // RULES PUT THEM THERE fired nothing — the one case where nobody is watching to press a button. Each
    // membership change made here now fires the same trigger the manual path fires, with the same list id,
    // so an automation cannot tell the two apart (and should not: the customer's intent is "they're on the
    // list now").
    for cid in &to_add {
        let _ = sqlx::query("INSERT INTO list_members(id,list_id,contact_id,tenant_id,added_manually) VALUES($1,$2,$3,$4,false) ON CONFLICT DO NOTHING")
            .bind(Uuid::new_v4()).bind(list.id).bind(cid).bind(list.tenant_id).execute(db).await;
        crate::automation::engine::fire_list_trigger(
            db,
            list.tenant_id,
            *cid,
            list.id,
            "ListAdded",
        )
        .await;
    }
    for cid in &to_remove {
        let _ = sqlx::query(
            "DELETE FROM list_members WHERE list_id=$1 AND contact_id=$2 AND tenant_id=$3",
        )
        .bind(list.id)
        .bind(cid)
        .bind(list.tenant_id)
        .execute(db)
        .await;
        crate::automation::engine::fire_list_trigger(
            db,
            list.tenant_id,
            *cid,
            list.id,
            "ListRemoved",
        )
        .await;
    }

    // The count the console shows is written here, so it cannot disagree with the membership.
    let _ = sqlx::query(
        "UPDATE lists SET member_count = (SELECT count(*) FROM list_members WHERE list_id = $1), \
                          updated_at = now() \
          WHERE id = $1 AND tenant_id = $2",
    )
    .bind(list.id)
    .bind(list.tenant_id)
    .execute(db)
    .await;

    Ok(
        json!({"message":"Evaluated","total_matched":matched.len(),"added":to_add.len(),"removed":to_remove.len()}),
    )
}

/// Re-evaluate every DYNAMIC list of one account.
///
/// `evaluate_dynamic_list` was only ever called by its own HTTP handler, so a "self-maintaining" list
/// maintained itself only when somebody pressed Evaluate. This is the entry point the write paths
/// call: a deal created or moved, a tag added, a consent flag recorded. Static lists are skipped, and
/// a list whose rules are empty is a no-op, so the cost is bounded by the number of real dynamic lists.
pub async fn refresh_dynamic_lists(db: &PgPool, tenant_id: Uuid) {
    let lists: Vec<List> = match sqlx::query_as::<_, List>(
        "SELECT * FROM lists WHERE tenant_id = $1 AND is_active IS NOT FALSE AND dynamic_rules IS NOT NULL",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await
    {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(tenant = %tenant_id, error = %e, "could not read dynamic lists to refresh");
            return;
        }
    };
    for list in lists {
        if let Err(e) = evaluate_dynamic_list(db, &list).await {
            tracing::warn!(list = %list.id, name = %list.name, error = %e, "dynamic list refresh failed");
        }
    }
}
