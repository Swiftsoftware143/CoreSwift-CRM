//! THE FILES THAT TRAVEL WITH A MESSAGE (kanban t_f919924f).
//!
//! Before this module the receiver counted a delivery's file parts, added their bytes to the
//! message's storage figure and then threw the bytes away: the inbox marked a message as carrying
//! an attachment and there was nothing to open. The app's own branded mail made that visible on
//! every send — the logo is an inline `cid:` part, so the reader had no image to draw.
//!
//! Two directions, one model:
//!
//! * **Inbound** — a receiver parses the delivery's file parts into [`InboundFile`], and
//!   [`plan_inbound`] decides what can be KEPT before the message row is written: a part over the
//!   per-file ceiling, past the per-message count/total ceiling, or with no room left in the
//!   account is not stored (a notice row carries the reason instead, so the reader says so).
//!   Inbound MAIL is still never refused and nothing is deleted — only the bytes that cannot be
//!   kept are declined, which is the same rule the outbound door applies through
//!   `storage::enforce_file_room`.
//! * **Outbound** — the composer posts base64 parts ([`AttachmentUpload`]); [`decode_uploads`]
//!   enforces the size ceiling and returns [`OutgoingAttachment`] values for the provider, and the
//!   same rows are written against the "Sent" message so the sender can re-open what they sent.
//!
//! STORAGE IS COUNTED ONCE. `private_email_messages.size_bytes` already includes a delivery's file
//! bytes (the meter, the banner and the 100% refusal all read that), so this table is the payload
//! and nothing sums it a second time.
//!
//! TENANCY. Every read takes the caller's account id; the download route resolves the file through
//! `(tenant_id, message_id, id)` in one statement, so a guessed attachment id from another account
//! finds no row.

use std::collections::HashMap;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::errors::AppError;

/// One file part as it arrived, before it is written to the database.
#[derive(Debug, Clone)]
pub struct InboundFile {
    /// The sender's name for the part, already made safe for a header and a filename.
    pub filename: String,
    pub content_type: String,
    /// MIME Content-ID without angle brackets, when the delivery named one.
    pub content_id: Option<String>,
    /// Whether the body draws this part in place (`cid:`) rather than offering it as a download.
    pub is_inline: bool,
    pub bytes: Vec<u8>,
}

/// One file part on its way OUT through a provider.
#[derive(Debug, Clone)]
pub struct OutgoingAttachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// A part the receiver could NOT keep, with the plain sentence the reader shows on its chip.
#[derive(Debug, Clone)]
pub struct SkippedFile {
    pub file: InboundFile,
    pub reason: String,
}

/// What an arriving delivery will keep, and what it must decline, decided BEFORE the message row is
/// written so `size_bytes` can count only the bytes that are really stored.
pub struct InboundPlan {
    pub keep: Vec<InboundFile>,
    pub skipped: Vec<SkippedFile>,
    /// Bytes of the kept parts — the figure the message's storage size adds.
    pub kept_bytes: usize,
}

/// Decide, before anything is stored, which arriving parts this account can keep.
///
/// The rules are the customer-facing ones, in the order they are checked:
///   1. at most [`MAX_FILES_PER_MESSAGE`] files on one message;
///   2. one file at most [`MAX_INBOUND_FILE_BYTES`];
///   3. all files on one message at most [`MAX_INBOUND_TOTAL_BYTES`];
///   4. the file must FIT in the room the account has left — a single file can never take the
///      account past its allowance, which is what "a cap so one 40 MB file cannot fill an account"
///      means at a 1 MB trial quota as much as at 2 GB.
///
/// A declined part never refuses the message: it leaves a notice row ([`store_skipped`]), so the
/// reader says "this file was not saved" with the reason instead of silently losing it.
pub fn plan_inbound(files: Vec<InboundFile>, used_bytes: i64, quota_bytes: i64) -> InboundPlan {
    let used = used_bytes.max(0);
    let quota = quota_bytes.max(0);
    let mut plan = InboundPlan {
        keep: Vec::new(),
        skipped: Vec::new(),
        kept_bytes: 0,
    };
    for (index, file) in files.into_iter().enumerate() {
        let size = file.bytes.len();
        let reason = if index >= MAX_FILES_PER_MESSAGE {
            Some(format!(
                "This message carried more than {} files, so this one was not saved.",
                MAX_FILES_PER_MESSAGE
            ))
        } else if size > MAX_INBOUND_FILE_BYTES {
            Some(format!(
                "This file is {} — one attachment may be up to {}.",
                crate::private_email::storage::human_bytes(size as i64),
                crate::private_email::storage::human_bytes(MAX_INBOUND_FILE_BYTES as i64)
            ))
        } else if plan.kept_bytes + size > MAX_INBOUND_TOTAL_BYTES {
            Some(format!(
                "The files on this message add up to more than {}, so this one was not saved.",
                crate::private_email::storage::human_bytes(MAX_INBOUND_TOTAL_BYTES as i64)
            ))
        } else {
            let room = (quota - used - plan.kept_bytes as i64).max(0);
            if quota > 0 && size as i64 > room {
                Some(format!(
                    "Your storage has {} left of {} and this file is {}, so it was not saved. \
                     The message still arrived and nothing was deleted — free some space or \
                     upgrade for more storage, and new attachments are kept again.",
                    crate::private_email::storage::human_bytes(room),
                    crate::private_email::storage::human_bytes(quota),
                    crate::private_email::storage::human_bytes(size as i64)
                ))
            } else {
                None
            }
        };
        match reason {
            Some(reason) => plan.skipped.push(SkippedFile { file, reason }),
            None => {
                plan.kept_bytes += size;
                plan.keep.push(file);
            }
        }
    }
    plan
}

/// A file the composer posts: base64 in JSON, because the console has no multipart client.
#[derive(Debug, Clone, Deserialize)]
pub struct AttachmentUpload {
    pub filename: String,
    #[serde(default)]
    pub content_type: Option<String>,
    /// Standard base64, no `data:` prefix.
    pub data_base64: String,
}

/// The most ONE outbound file may be. The edge allows 25 MB on the two send routes (scoped, so no
/// other route's ceiling moved) and base64 inflates by a third, so 10 MB of file is a comfortable
/// 13.4 MB request.
pub const MAX_OUTBOUND_FILE_BYTES: usize = 10 * 1024 * 1024;
/// The most a single message's attachments may add up to, before base64. 18 MB of raw bytes is
/// 24 MB once base64-encoded, which fits the 26 MB request ceiling the send routes declare — a
/// 20 MB total would have needed a 26.7 MB request and been answered 413 at the door before the
/// app could say why (kanban t_0b3f3165).
pub const MAX_OUTBOUND_TOTAL_BYTES: usize = 18 * 1024 * 1024;
/// The most ONE arriving part may be. The receiver's envelope is 26 MB, so a part far larger than
/// this could fill a small account in one delivery; the ceiling is the same 10 MB the composer
/// allows, so the product has one number a customer can remember.
pub const MAX_INBOUND_FILE_BYTES: usize = 10 * 1024 * 1024;
/// The most one message's arriving files may add up to before the rest are declined.
pub const MAX_INBOUND_TOTAL_BYTES: usize = 20 * 1024 * 1024;
/// The most files one message may carry, in either direction, so a delivery cannot turn into a
/// thousand rows the reader has to draw.
pub const MAX_FILES_PER_MESSAGE: usize = 20;
/// An inline image larger than this is NOT inlined into the message JSON; it stays a downloadable
/// part instead, so opening a message can never ship a 10 MB base64 blob into the browser.
pub const MAX_INLINE_EMBED_BYTES: usize = 1_500_000;
/// The total inline budget for one message body.
pub const MAX_INLINE_EMBED_TOTAL_BYTES: usize = 4 * 1024 * 1024;

/// Turn the composer's base64 parts into provider-ready attachments, refusing (with a plain
/// sentence) anything over the ceiling. The storage rule is enforced by the CALLER through
/// `storage::enforce_new_file_room`, because that is the one door the plan owns.
pub fn decode_uploads(items: &[AttachmentUpload]) -> Result<Vec<OutgoingAttachment>, AppError> {
    let mut out: Vec<OutgoingAttachment> = Vec::with_capacity(items.len());
    let mut total = 0usize;
    for item in items {
        if out.len() >= MAX_FILES_PER_MESSAGE {
            return Err(AppError::Validation(format!(
                "One message may carry up to {} files — send fewer, or send a second message.",
                MAX_FILES_PER_MESSAGE
            )));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(item.data_base64.trim())
            .map_err(|_| {
                AppError::Validation(format!(
                    "The file '{}' could not be read — attach it again.",
                    item.filename
                ))
            })?;
        // A ZERO-BYTE file is still a file the customer attached. Keeping it (a 0 B chip) is the
        // honest arm: the one thing this path may never do is drop a named part and still answer
        // "sent" (kanban t_41f04fd9).
        if bytes.len() > MAX_OUTBOUND_FILE_BYTES {
            return Err(AppError::Validation(format!(
                "'{}' is {} — one attachment may be up to {}.",
                item.filename,
                crate::private_email::storage::human_bytes(bytes.len() as i64),
                crate::private_email::storage::human_bytes(MAX_OUTBOUND_FILE_BYTES as i64)
            )));
        }
        total += bytes.len();
        if total > MAX_OUTBOUND_TOTAL_BYTES {
            return Err(AppError::Validation(format!(
                "Those attachments add up to more than {} — send fewer, or smaller, files.",
                crate::private_email::storage::human_bytes(MAX_OUTBOUND_TOTAL_BYTES as i64)
            )));
        }
        out.push(OutgoingAttachment {
            filename: safe_filename(&item.filename, out.len() + 1),
            content_type: item
                .content_type
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .unwrap_or("application/octet-stream")
                .to_string(),
            bytes,
        });
    }
    Ok(out)
}

/// Write the parts the receiver could NOT keep, as zero-byte rows that carry the reason.
///
/// Inbound mail is never refused, so this is how a full (or oversized) delivery stays honest: the
/// message and its text are stored, the bytes are not, and the reader draws a chip that says why.
pub async fn store_skipped(
    db: &PgPool,
    tenant_id: Uuid,
    message_id: Uuid,
    skipped: &[SkippedFile],
) {
    if skipped.is_empty() || message_id == Uuid::nil() {
        return;
    }
    for item in skipped {
        insert_row(
            db,
            tenant_id,
            message_id,
            &item.file.filename,
            &item.file.content_type,
            item.file.content_id.as_deref(),
            // A part with no bytes can never be drawn in place; it is a chip with a reason.
            false,
            &[],
            Some(&item.reason),
        )
        .await;
    }
}

/// Write the files a delivery carried against the message it created.
///
/// A failure here is LOGGED and swallowed: the message has already been accepted, and a provider
/// retry cannot be relied on to bring the bytes back. Losing the mail because one file row failed
/// would be the worse defect — the row is simply absent and the message says so.
pub async fn store_for_message(
    db: &PgPool,
    tenant_id: Uuid,
    message_id: Uuid,
    files: &[InboundFile],
) {
    if files.is_empty() || message_id == Uuid::nil() {
        return;
    }
    for file in files {
        insert_row(
            db,
            tenant_id,
            message_id,
            &file.filename,
            &file.content_type,
            file.content_id.as_deref(),
            file.is_inline || file.content_id.is_some(),
            &file.bytes,
            None,
        )
        .await;
    }
}

/// Write the files a SENT message carried against the "Sent" row, so the sender can re-open what
/// they sent and the bytes are counted once (in the message's own `size_bytes`).
pub async fn store_outgoing(
    db: &PgPool,
    tenant_id: Uuid,
    message_id: Uuid,
    files: &[OutgoingAttachment],
) {
    if files.is_empty() || message_id == Uuid::nil() {
        return;
    }
    for file in files {
        insert_row(
            db,
            tenant_id,
            message_id,
            &file.filename,
            &file.content_type,
            None,
            false,
            &file.bytes,
            None,
        )
        .await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_row(
    db: &PgPool,
    tenant_id: Uuid,
    message_id: Uuid,
    filename: &str,
    content_type: &str,
    content_id: Option<&str>,
    is_inline: bool,
    bytes: &[u8],
    not_stored_reason: Option<&str>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO private_email_attachments
            (tenant_id, message_id, filename, content_type, content_id, is_inline, size_bytes, data,
             not_stored_reason)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(tenant_id)
    .bind(message_id)
    .bind(filename)
    .bind(content_type)
    .bind(content_id)
    .bind(is_inline)
    .bind(bytes.len() as i32)
    .bind(bytes)
    .bind(not_stored_reason)
    .execute(db)
    .await;
    if let Err(e) = result {
        tracing::warn!(
            tenant = %tenant_id,
            message = %message_id,
            filename = %filename,
            error = %e,
            "an attachment could not be stored with its message"
        );
    }
}

/// What the reader needs to draw a chip — never the bytes themselves.
#[derive(Debug, Clone, Serialize)]
pub struct AttachmentMeta {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i32,
    pub is_inline: bool,
    /// None when the bytes are stored. Otherwise the plain sentence explaining why this file was
    /// not saved; the reader shows it and offers no download.
    pub not_stored_reason: Option<String>,
}

/// Every file attached to one message of this account, newest last.
pub async fn list_for_message(
    db: &PgPool,
    tenant_id: Uuid,
    message_id: Uuid,
) -> Result<Vec<AttachmentMeta>, AppError> {
    let rows = sqlx::query(
        r#"
        SELECT id, filename, content_type, size_bytes, is_inline, not_stored_reason
          FROM private_email_attachments
         WHERE tenant_id = $1 AND message_id = $2
         ORDER BY created_at ASC, id ASC
        "#,
    )
    .bind(tenant_id)
    .bind(message_id)
    .fetch_all(db)
    .await
    .map_err(AppError::Database)?;

    Ok(rows
        .into_iter()
        .map(|r| AttachmentMeta {
            id: r.try_get("id").unwrap_or_default(),
            filename: r.try_get("filename").unwrap_or_default(),
            content_type: r
                .try_get("content_type")
                .unwrap_or_else(|_| "application/octet-stream".into()),
            size_bytes: r.try_get("size_bytes").unwrap_or(0),
            is_inline: r.try_get("is_inline").unwrap_or(false),
            not_stored_reason: r.try_get("not_stored_reason").unwrap_or(None),
        })
        .collect())
}

/// One stored file as the download route needs it.
pub struct StoredFile {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
    /// Some when this row never held bytes (the part could not be kept); the route refuses it.
    pub not_stored_reason: Option<String>,
}

/// The bytes of ONE file, resolved through the account AND the message it belongs to.
pub async fn load_bytes(
    db: &PgPool,
    tenant_id: Uuid,
    message_id: Uuid,
    attachment_id: Uuid,
) -> Result<Option<StoredFile>, AppError> {
    let row = sqlx::query(
        r#"
        SELECT filename, content_type, data, not_stored_reason
          FROM private_email_attachments
         WHERE id = $1 AND tenant_id = $2 AND message_id = $3
        "#,
    )
    .bind(attachment_id)
    .bind(tenant_id)
    .bind(message_id)
    .fetch_optional(db)
    .await
    .map_err(AppError::Database)?;

    Ok(row.map(|r| StoredFile {
        filename: r.try_get("filename").unwrap_or_default(),
        content_type: r
            .try_get("content_type")
            .unwrap_or_else(|_| "application/octet-stream".into()),
        bytes: r.try_get("data").unwrap_or_default(),
        not_stored_reason: r.try_get("not_stored_reason").unwrap_or(None),
    }))
}

/// The inline parts small enough to embed in the message JSON, keyed for `cid:` lookup.
///
/// Only `is_inline` rows are read, and only under [`MAX_INLINE_EMBED_BYTES`]: opening a message must
/// not ship a 10 MB PDF into the browser as base64. A part that is too large stays a chip.
pub async fn inline_parts_for_message(
    db: &PgPool,
    tenant_id: Uuid,
    message_id: Uuid,
) -> Result<Vec<(String, String, Vec<u8>)>, AppError> {
    let rows = sqlx::query(
        r#"
        SELECT filename, content_id, content_type, data
          FROM private_email_attachments
         WHERE tenant_id = $1 AND message_id = $2
           AND is_inline = true
           AND size_bytes > 0
           AND size_bytes <= $3
         ORDER BY created_at ASC, id ASC
        "#,
    )
    .bind(tenant_id)
    .bind(message_id)
    .bind(MAX_INLINE_EMBED_BYTES as i32)
    .fetch_all(db)
    .await
    .map_err(AppError::Database)?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let filename: String = r.try_get("filename").unwrap_or_default();
            let content_id: Option<String> = r.try_get("content_id").unwrap_or(None);
            // The lookup key: the Content-ID when the delivery named one, else the filename.
            let key = content_id
                .map(|c| c.trim().trim_matches(['<', '>']).to_string())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| filename.clone());
            (
                key,
                r.try_get::<String, _>("content_type")
                    .unwrap_or_else(|_| "application/octet-stream".into()),
                r.try_get::<Vec<u8>, _>("data").unwrap_or_default(),
            )
        })
        .collect())
}

/// A file part's name, made safe for a header and for a saved filename.
///
/// Never a path: separators, control characters and quotes are removed, a leading dot is dropped,
/// and an empty result becomes `attachment-<n>`. The name the customer sees is preserved otherwise.
pub fn safe_filename(raw: &str, ordinal: usize) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '\'' | '\\' | '/' | ':' | '*' | '?'))
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    if cleaned.is_empty() {
        format!("attachment-{ordinal}")
    } else {
        cleaned.chars().take(180).collect()
    }
}

/// A `Content-Disposition` value that survives a non-ASCII filename: an ASCII fallback plus
/// RFC 5987 `filename*` when the name is not plain ASCII.
pub fn content_disposition(disposition: &str, filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii = if ascii.trim().is_empty() {
        "attachment".to_string()
    } else {
        ascii
    };
    if filename.is_ascii() {
        format!("{disposition}; filename=\"{ascii}\"")
    } else {
        let encoded: String = filename
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        format!("{disposition}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
    }
}

/// Replace `cid:<name>` references in a message body with `data:` URIs built from the stored
/// inline parts, so a branded message draws its own logo without a second, unauthenticated route.
///
/// Matching is by Content-ID first, then the filename, then its basename. If the body carries a
/// single `cid:` reference this delivery did not name and there is exactly ONE inline part, that
/// part is used — the shape a provider produces when it rewrites the Content-ID in transit.
/// A part over the inline budget is left as `cid:` and appears as a download chip instead.
pub fn rewrite_inline_images(html: &str, parts: &[(String, String, Vec<u8>)]) -> String {
    if parts.is_empty() || !html.to_ascii_lowercase().contains("cid:") {
        return html.to_string();
    }

    struct Part<'a> {
        content_type: &'a str,
        bytes: &'a [u8],
    }
    let mut by_key: HashMap<String, Part<'_>> = HashMap::new();
    let mut inline_count = 0usize;
    let mut single: Option<Part<'_>> = None;
    for (key, content_type, bytes) in parts {
        if bytes.len() > MAX_INLINE_EMBED_BYTES {
            continue;
        }
        inline_count += 1;
        single = Some(Part {
            content_type,
            bytes,
        });
        let key = key.trim().trim_matches(['<', '>']).to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        let part = Part {
            content_type,
            bytes,
        };
        if let Some(base) = key.rsplit('/').next() {
            by_key.entry(base.to_string()).or_insert(Part {
                content_type: part.content_type,
                bytes: part.bytes,
            });
        }
        by_key.entry(key).or_insert(Part {
            content_type: part.content_type,
            bytes: part.bytes,
        });
    }

    let mut out = String::with_capacity(html.len());
    let bytes = html.as_bytes();
    let mut i = 0usize;
    let mut budget = MAX_INLINE_EMBED_TOTAL_BYTES;
    while i < bytes.len() {
        if i + 4 <= bytes.len() && bytes[i..i + 4].eq_ignore_ascii_case(b"cid:") {
            let mut j = i + 4;
            while j < bytes.len() {
                let b = bytes[j];
                let ok = b.is_ascii_alphanumeric()
                    || matches!(b, b'.' | b'_' | b'@' | b'-' | b'+' | b'%' | b'~');
                if !ok {
                    break;
                }
                j += 1;
            }
            let token = &html[i + 4..j];
            let token_key = token.trim_matches(['<', '>']).to_ascii_lowercase();
            let chosen = by_key.get(&token_key).or_else(|| {
                if inline_count == 1 {
                    single.as_ref()
                } else {
                    None
                }
            });
            if let Some(part) = chosen {
                if part.bytes.len() <= budget {
                    budget -= part.bytes.len();
                    out.push_str("data:");
                    out.push_str(part.content_type);
                    out.push_str(";base64,");
                    out.push_str(&base64::engine::general_purpose::STANDARD.encode(part.bytes));
                    i = j;
                    continue;
                }
            }
        }
        // Advance one CHARACTER, so multi-byte text is copied intact.
        let ch = html[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Is this delivery's file part referenced from the body as an inline image?
///
/// Mailgun does not always publish a Content-ID on the inbound leg, so the body's own `cid:`
/// reference is the reliable signal: a part whose filename is named there is embedded, everything
/// else is a downloadable attachment.
pub fn looks_inline(filename: &str, body_html: Option<&str>) -> bool {
    let Some(html) = body_html else {
        return false;
    };
    let lower = html.to_ascii_lowercase();
    let name = filename.to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    lower.contains(&format!("cid:{name}"))
        || name
            .rsplit('/')
            .next()
            .map(|base| !base.is_empty() && lower.contains(&format!("cid:{base}")))
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(key: &str, ctype: &str, bytes: &[u8]) -> (String, String, Vec<u8>) {
        (key.to_string(), ctype.to_string(), bytes.to_vec())
    }

    #[test]
    fn a_cid_reference_becomes_a_data_uri() {
        let html = r#"<p>Regards<img src="cid:logo.png"></p>"#;
        let out = rewrite_inline_images(html, &[part("logo.png", "image/png", b"PNG")]);
        assert!(out.contains("src=\"data:image/png;base64,UE5H\""), "{out}");
        assert!(!out.contains("cid:"), "{out}");
    }

    #[test]
    fn a_content_id_with_angle_brackets_matches() {
        let html = r#"<img src="cid:abc@mailgun">"#;
        let out = rewrite_inline_images(html, &[part("<abc@mailgun>", "image/png", b"PNG")]);
        assert!(out.contains("data:image/png;base64,"), "{out}");
    }

    #[test]
    fn the_only_inline_part_covers_an_unnamed_reference() {
        let html = r#"<img src="cid:i-did-not-name-this">"#;
        let out = rewrite_inline_images(html, &[part("logo.png", "image/jpeg", b"JPG")]);
        assert!(out.contains("data:image/jpeg;base64,"), "{out}");
    }

    #[test]
    fn an_unmatched_reference_is_left_alone_when_several_parts_exist() {
        let html = r#"<img src="cid:other.png">"#;
        let out = rewrite_inline_images(
            html,
            &[
                part("logo.png", "image/png", b"A"),
                part("second.png", "image/png", b"B"),
            ],
        );
        assert!(out.contains("cid:other.png"), "{out}");
    }

    #[test]
    fn plain_text_with_no_cid_is_unchanged() {
        let html = "<p>hello — ünïcode</p>";
        assert_eq!(rewrite_inline_images(html, &[]), html);
        assert_eq!(
            rewrite_inline_images(html, &[part("logo.png", "image/png", b"A")]),
            html
        );
    }

    #[test]
    fn a_filename_is_never_a_path() {
        assert_eq!(safe_filename("../../etc/passwd", 1), "passwd");
        assert_eq!(
            safe_filename("invoice \"march\".pdf", 1),
            "invoice march.pdf"
        );
        assert_eq!(safe_filename("   ", 3), "attachment-3");
        assert_eq!(safe_filename("..", 2), "attachment-2");
    }

    #[test]
    fn a_disposition_survives_a_non_ascii_name() {
        let d = content_disposition("attachment", "faktura-märz.pdf");
        assert!(d.starts_with("attachment; filename=\"faktura-m_rz.pdf\""));
        assert!(d.contains("filename*=UTF-8''faktura-m%C3%A4rz.pdf"), "{d}");
        // A quote or a newline can never break out of the header.
        let d = content_disposition("inline", "a\"b\r\nc.png");
        assert!(!d.contains('\n') && !d.contains('\r'));
        assert!(!d.contains("a\"b"));
    }

    #[test]
    fn a_body_that_names_the_file_is_inline() {
        assert!(looks_inline(
            "logo.png",
            Some(r#"<img src="cid:logo.png">"#)
        ));
        assert!(!looks_inline(
            "invoice.pdf",
            Some(r#"<img src="cid:logo.png">"#)
        ));
        assert!(!looks_inline("invoice.pdf", None));
    }

    #[test]
    fn uploads_over_the_ceiling_are_refused_with_a_sentence() {
        let big = AttachmentUpload {
            filename: "huge.bin".into(),
            content_type: None,
            data_base64: base64::engine::general_purpose::STANDARD.encode(vec![
                0u8;
                MAX_OUTBOUND_FILE_BYTES
                    + 1
            ]),
        };
        let err = decode_uploads(&[big]).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[test]
    fn a_zero_byte_upload_is_kept_not_dropped() {
        // A named part with no bytes is still a part the customer attached. Dropping it here would
        // answer "sent" over a file that never left (kanban t_41f04fd9).
        let items = vec![AttachmentUpload {
            filename: "empty.txt".into(),
            content_type: Some("text/plain".into()),
            data_base64: String::new(),
        }];
        let out = decode_uploads(&items).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].filename, "empty.txt");
        assert!(out[0].bytes.is_empty());
    }

    #[test]
    fn a_small_upload_decodes_with_a_safe_name() {
        let items = vec![AttachmentUpload {
            filename: "../note.txt".into(),
            content_type: Some("text/plain".into()),
            data_base64: base64::engine::general_purpose::STANDARD.encode(b"hello"),
        }];
        let out = decode_uploads(&items).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].filename, "note.txt");
        assert_eq!(out[0].content_type, "text/plain");
        assert_eq!(out[0].bytes, b"hello");
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn file(name: &str, size: usize) -> InboundFile {
        InboundFile {
            filename: name.to_string(),
            content_type: "application/octet-stream".into(),
            content_id: None,
            is_inline: false,
            bytes: vec![7u8; size],
        }
    }

    #[test]
    fn a_file_that_fits_the_room_is_kept_and_counted() {
        let plan = plan_inbound(vec![file("a.bin", 400), file("b.bin", 500)], 100, 2000);
        assert_eq!(plan.keep.len(), 2);
        assert!(plan.skipped.is_empty());
        assert_eq!(plan.kept_bytes, 900);
    }

    #[test]
    fn a_file_over_the_accounts_remaining_room_is_declined_not_the_message() {
        let plan = plan_inbound(vec![file("big.bin", 900), file("small.bin", 50)], 900, 1000);
        assert_eq!(plan.keep.len(), 1);
        assert_eq!(plan.keep[0].filename, "small.bin");
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].reason.contains("was not saved"));
        assert_eq!(plan.kept_bytes, 50);
    }

    #[test]
    fn one_file_over_the_per_file_ceiling_is_declined_even_with_room() {
        let plan = plan_inbound(
            vec![file("huge.bin", MAX_INBOUND_FILE_BYTES + 1)],
            0,
            10 * 1024 * 1024 * 1024,
        );
        assert!(plan.keep.is_empty());
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0]
            .reason
            .contains("one attachment may be up to"));
    }

    #[test]
    fn files_past_the_count_ceiling_are_declined_with_a_reason() {
        let many: Vec<InboundFile> = (0..MAX_FILES_PER_MESSAGE + 2)
            .map(|i| file(&format!("f{i}.bin"), 10))
            .collect();
        let plan = plan_inbound(many, 0, 10 * 1024 * 1024 * 1024);
        assert_eq!(plan.keep.len(), MAX_FILES_PER_MESSAGE);
        assert_eq!(plan.skipped.len(), 2);
        assert!(plan.skipped[0].reason.contains("more than"));
    }
}
