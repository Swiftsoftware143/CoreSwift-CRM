//! Two tickets becoming one, and one becoming two.
//!
//! Both operations move MESSAGES, never copies of them, and both are transactions: a merge that moved the
//! messages but failed to close the source would leave two tickets both claiming the same conversation, and
//! a split that created the ticket but failed to move the message would leave the message nowhere. The
//! customer's words exist once, in one place, at every moment.
//!
//! The refusals matter more than the happy path:
//!
//!   * a merge cannot cross accounts — the source and target are both read with the caller's tenant;
//!   * a merge cannot target itself, or a ticket that has already been merged (which would build a chain
//!     nobody can follow), or a source that has already been merged (the same message moved twice);
//!   * a split cannot pull out an internal note or a system event — those belong to the ticket's history,
//!     not to a customer's question.

use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;

/// What a merge or split did, so the caller can say something true about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    pub survivor: Uuid,
    pub survivor_number: i32,
    pub merged: Uuid,
    pub merged_number: i32,
    pub messages_moved: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitOutcome {
    pub ticket_id: Uuid,
    pub number: i32,
    pub from_number: i32,
    pub moved_message: Uuid,
}

/// Move every message from one ticket onto another, then close the one that lost.
///
/// The survivor keeps its own number: the customer who was told `CS-1042` must not find that number now
/// means a different conversation. The merged ticket is closed and points at its survivor, which is what
/// lets the console answer "where did this go?" instead of silently losing the row.
pub async fn merge_tickets(
    pool: &PgPool,
    tenant_id: Uuid,
    from_id: Uuid,
    into_id: Uuid,
) -> Result<MergeOutcome, AppError> {
    if from_id == into_id {
        return Err(AppError::BadRequest(
            "A ticket cannot be merged into itself.".into(),
        ));
    }

    let mut tx = pool.begin().await.map_err(AppError::Database)?;

    // Lock in a fixed order (by id) so two simultaneous merges of the same pair cannot deadlock.
    let rows: Vec<(Uuid, i32, String, Option<Uuid>, String)> = sqlx::query_as(
        r#"SELECT id, number, subject, merged_into_id, status
             FROM tickets
            WHERE id = ANY($1) AND tenant_id = $2
            ORDER BY id
              FOR UPDATE"#,
    )
    .bind(vec![from_id, into_id])
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    if rows.len() != 2 {
        return Err(AppError::NotFound(
            "Both tickets have to be in your account to merge them.".into(),
        ));
    }
    let source = rows
        .iter()
        .find(|r| r.0 == from_id)
        .expect("source present");
    let target = rows
        .iter()
        .find(|r| r.0 == into_id)
        .expect("target present");

    if source.3.is_some() {
        return Err(AppError::BadRequest(format!(
            "#{} has already been merged into another ticket.",
            source.1
        )));
    }
    if target.3.is_some() {
        return Err(AppError::BadRequest(format!(
            "#{} was itself merged into another ticket — merge into that one instead.",
            target.1
        )));
    }

    let moved = sqlx::query("UPDATE ticket_messages SET ticket_id = $1 WHERE ticket_id = $2")
        .bind(into_id)
        .bind(from_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?
        .rows_affected();

    // The timeline of the survivor records where the extra messages came from; the closed ticket records
    // where it went. Both are system events, so neither is ever shown to a customer.
    sqlx::query(
        r#"INSERT INTO ticket_messages (ticket_id, sender_type, kind, message)
           VALUES ($1, 'system', 'event', $2)"#,
    )
    .bind(into_id)
    .bind(format!(
        "Merged #{} into this ticket ({} message{} moved).",
        source.1,
        moved,
        if moved == 1 { "" } else { "s" }
    ))
    .execute(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    sqlx::query(
        r#"INSERT INTO ticket_messages (ticket_id, sender_type, kind, message)
           VALUES ($1, 'system', 'event', $2)"#,
    )
    .bind(from_id)
    .bind(format!(
        "Merged into #{} — this ticket is closed and its conversation now lives there.",
        target.1
    ))
    .execute(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    sqlx::query(
        r#"UPDATE tickets
              SET status = 'closed', merged_into_id = $2, solved_at = COALESCE(solved_at, NOW()),
                  updated_at = NOW()
            WHERE id = $1"#,
    )
    .bind(from_id)
    .bind(into_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    sqlx::query("UPDATE tickets SET updated_at = NOW() WHERE id = $1")
        .bind(into_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;

    tx.commit().await.map_err(AppError::Database)?;

    tracing::info!(
        tenant = %tenant_id, survivor = %into_id, merged = %from_id, messages = moved,
        "tickets merged"
    );

    Ok(MergeOutcome {
        survivor: into_id,
        survivor_number: target.1,
        merged: from_id,
        merged_number: source.1,
        messages_moved: moved,
    })
}

/// Pull one message out of a ticket and into a new one.
///
/// Used when a single email in a long thread is really a separate question. Only a customer or agent
/// reply can be split: an internal note and a system event are the ticket's own history.
pub async fn split_message(
    pool: &PgPool,
    tenant_id: Uuid,
    from_id: Uuid,
    message_id: Uuid,
    subject: Option<String>,
) -> Result<SplitOutcome, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::Database)?;

    let parent: Option<(
        i32,
        String,
        String,
        String,
        Option<Uuid>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        r#"SELECT number, subject, priority, source, contact_id, contact_email, contact_name
                 FROM tickets WHERE id = $1 AND tenant_id = $2
                  FOR UPDATE"#,
    )
    .bind(from_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    let Some((
        from_number,
        parent_subject,
        priority,
        source,
        contact_id,
        contact_email,
        contact_name,
    )) = parent
    else {
        return Err(AppError::NotFound(
            "That ticket is not in your account.".into(),
        ));
    };

    let msg: Option<(String,)> = sqlx::query_as(
        "SELECT kind FROM ticket_messages WHERE id = $1 AND ticket_id = $2 FOR UPDATE",
    )
    .bind(message_id)
    .bind(from_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    let Some((kind,)) = msg else {
        return Err(AppError::NotFound(
            "That message is not on this ticket.".into(),
        ));
    };
    if kind != "reply" {
        return Err(AppError::BadRequest(
            "Only a reply can be split out — a note or a system event belongs to this ticket."
                .into(),
        ));
    }

    let new_subject = subject
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Split from #{}: {}", from_number, parent_subject));

    let (new_id, number): (Uuid, i32) = sqlx::query_as(
        r#"INSERT INTO tickets
             (tenant_id, subject, description, status, priority, source,
              contact_id, contact_email, contact_name, ai_state)
           VALUES ($1, $2, '', 'new', $3, $4, $5, $6, $7, 'none')
           RETURNING id, number"#,
    )
    .bind(tenant_id)
    .bind(&new_subject)
    .bind(&priority)
    .bind(&source)
    .bind(contact_id)
    .bind(&contact_email)
    .bind(&contact_name)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    sqlx::query("UPDATE ticket_messages SET ticket_id = $1 WHERE id = $2")
        .bind(new_id)
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;

    for (ticket, text) in [
        (
            new_id,
            format!(
                "Split out of #{} — this message is now its own ticket.",
                from_number
            ),
        ),
        (
            from_id,
            format!(
                "One message was split into #{} as a separate ticket.",
                number
            ),
        ),
    ] {
        sqlx::query(
            r#"INSERT INTO ticket_messages (ticket_id, sender_type, kind, message)
               VALUES ($1, 'system', 'event', $2)"#,
        )
        .bind(ticket)
        .bind(text)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;
    }

    sqlx::query("UPDATE tickets SET updated_at = NOW() WHERE id = ANY($1)")
        .bind(vec![from_id, new_id])
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;

    tx.commit().await.map_err(AppError::Database)?;

    Ok(SplitOutcome {
        ticket_id: new_id,
        number,
        from_number,
        moved_message: message_id,
    })
}
