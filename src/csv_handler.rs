//! CSV import/export handlers for contacts and opportunities.
//!
//! Provides drag-and-drop CSV upload with column mapping preview,
//! batch import up to 500 records, and streaming CSV export.

use axum::routing::{get, post};
use axum::{
    extract::{Multipart, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Extension, Json, Router,
};
use csv::{ReaderBuilder, WriterBuilder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

// ── Types ──────────────────────────────────────────────────────────────────

/// Result of a CSV import operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Column mapping from CSV header to contact field.
#[derive(Debug, Serialize, Deserialize)]
pub struct ColumnMapping {
    pub csv_header: String,
    pub contact_field: String,
}

/// CSV preview response with headers and sample rows.
#[derive(Debug, Serialize)]
pub struct CsvPreview {
    pub headers: Vec<String>,
    pub sample_rows: Vec<Vec<String>>,
}

/// Limit constants
const MAX_UPLOAD_BYTES: u64 = 5 * 1024 * 1024; // 5 MB
const MAX_ROWS: usize = 500;
const PREVIEW_ROWS: usize = 5;

// ── Helpers ────────────────────────────────────────────────────────────────

/// Simple email format check. Returns true if the string looks like an email.
fn is_valid_email(email: &str) -> bool {
    let email = email.trim();
    if email.is_empty() || !email.contains('@') || !email.contains('.') {
        return false;
    }
    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return false;
    }
    let domain_parts: Vec<&str> = parts[1].split('.').collect();
    if domain_parts.len() < 2 || domain_parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    true
}

/// Validate a single mapped CSV row. Returns Ok(()) or an error message.
fn validate_row(mapped: &std::collections::HashMap<String, String>) -> Result<(), String> {
    // First name is required
    if mapped.get("first_name").is_none_or(|v| v.trim().is_empty())
        && mapped.get("last_name").is_none_or(|v| v.trim().is_empty())
    {
        return Err("At least one of first_name or last_name is required".into());
    }

    // Email format check (if provided)
    if let Some(email) = mapped.get("email") {
        let email = email.trim();
        if !email.is_empty() && !is_valid_email(email) {
            return Err(format!("Invalid email format: {}", email));
        }
    }

    Ok(())
}

// ── Import ─────────────────────────────────────────────────────────────────

// ── One reader for every table format ─────────────────────────────────────────────────────────────────
// David's feature table lists "Import/Export (CSV, XLS, XLSX)" and only CSV was ever handled: measured
// 2026-10-02 there was no spreadsheet crate in the manifest and no xlsx/excel handling anywhere, so the
// claim was false. This is the single place the container format is decided.
//
// The format is sniffed by SIGNATURE, never by filename: an upload's extension is caller-supplied and
// routinely wrong — a spreadsheet exported from Excel as ".csv" still arrives as PK\x03\x04 — and a
// browser may send no filename at all. XLSX (any OOXML container) is a ZIP: PK\x03\x04. Legacy XLS is an
// OLE compound file: D0 CF 11 E0 A1 B1 1A E1.
//
// Spreadsheets are converted to `csv::StringRecord`, the type the mapping code already consumes, so the
// column mapping, validation and INSERT below stay ONE code path for all three formats. A second path
// would drift from the first, and the CSV path is the one with the bug history.
fn read_table(bytes: &[u8]) -> Result<(Vec<String>, Vec<csv::StringRecord>), AppError> {
    const OLE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    if bytes.starts_with(b"PK\x03\x04") || (bytes.len() >= 8 && bytes[..8] == OLE) {
        return read_spreadsheet(bytes);
    }

    let mut reader = ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(bytes);

    // Headers are returned AS AUTHORED. The two callers deliberately differ: the import keys its mapping
    // on lowercase names, while the preview must show the user the column names their file actually has.
    let headers = reader
        .headers()
        .map_err(|e| AppError::BadRequest(format!("Failed to read CSV headers: {}", e)))?
        .iter()
        .map(|h| h.to_string())
        .collect::<Vec<_>>();

    let mut records = Vec::new();
    for r in reader.records() {
        records.push(r.map_err(|e| AppError::BadRequest(format!("CSV parse error: {}", e)))?);
    }
    Ok((headers, records))
}

fn read_spreadsheet(bytes: &[u8]) -> Result<(Vec<String>, Vec<csv::StringRecord>), AppError> {
    // The trait must be in scope or `sheet_names` / `worksheet_range` are not found on Sheets<RS>.
    use calamine::Reader;
    let cursor = std::io::Cursor::new(bytes);
    let mut wb = calamine::open_workbook_auto_from_rs(cursor)
        .map_err(|e| AppError::BadRequest(format!("Unreadable spreadsheet: {}", e)))?;

    let sheet = wb
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| AppError::BadRequest("The spreadsheet has no sheets".into()))?;

    let range = wb
        .worksheet_range(&sheet)
        .map_err(|e| AppError::BadRequest(format!("Unreadable sheet '{}': {}", sheet, e)))?;

    let mut rows: Vec<Vec<String>> = range
        .rows()
        .map(|row| row.iter().map(cell_text).collect())
        .collect();

    if rows.is_empty() {
        return Err(AppError::BadRequest("The spreadsheet is empty".into()));
    }
    let headers = rows
        .remove(0)
        .iter()
        .map(|h| h.trim().to_string())
        .collect::<Vec<_>>();

    Ok((
        headers,
        rows.into_iter().map(csv::StringRecord::from).collect(),
    ))
}

/// One cell as text. `42.0` must arrive as `42`, or a phone or ID column imports as `5551234567.0` —
/// the failure a reader like this is most likely to cause silently.
fn cell_text(c: &calamine::Data) -> String {
    use calamine::Data;
    match c {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                format!("{}", f)
            }
        }
        Data::Int(i) => format!("{}", i),
        Data::Bool(b) => format!("{}", b),
        Data::DateTime(dt) => format!("{}", dt),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("{:?}", e),
    }
}

/// POST /api/csv/import/contacts
///
/// Accepts a multipart form with:
/// - `file`: .csv file (max 5 MB, max 500 rows)
/// - `mappings`: JSON array of `{csv_header, contact_field}` objects
///
/// Returns `{imported, skipped, errors[]}`.
pub async fn import_contacts(
    State(app_state): State<AppState>,
    Extension(claims): Extension<Claims>,
    mut multipart: Multipart,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let mut file_bytes: Option<Vec<u8>> = None;
    let mut mappings_str: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();
        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::BadRequest(format!("Failed to read field {}: {}", name, e)))?;

        match name.as_str() {
            "file" => file_bytes = Some(data.to_vec()),
            "mappings" => {
                mappings_str = Some(
                    String::from_utf8(data.to_vec())
                        .map_err(|_| AppError::BadRequest("mappings must be valid UTF-8".into()))?,
                )
            }
            _ => {}
        }
    }

    let file_bytes =
        file_bytes.ok_or_else(|| AppError::BadRequest("Missing 'file' field".into()))?;
    let mappings_str =
        mappings_str.ok_or_else(|| AppError::BadRequest("Missing 'mappings' field".into()))?;

    // Validate file size
    if file_bytes.len() as u64 > MAX_UPLOAD_BYTES {
        return Err(AppError::BadRequest(format!(
            "File too large: {} bytes (max {})",
            file_bytes.len(),
            MAX_UPLOAD_BYTES
        )));
    }

    // Parse mappings
    let mappings: Vec<ColumnMapping> = serde_json::from_str(&mappings_str)
        .map_err(|e| AppError::BadRequest(format!("Invalid mappings JSON: {}", e)))?;

    if mappings.is_empty() {
        return Err(AppError::BadRequest(
            "At least one column mapping is required".into(),
        ));
    }

    // Build a map from csv_header -> contact_field
    let mapping_map: std::collections::HashMap<String, String> = mappings
        .iter()
        .map(|m| (m.csv_header.to_lowercase(), m.contact_field.to_lowercase()))
        .collect();

    // CSV, XLS or XLSX — the format is sniffed, the mapping below is shared.
    let (raw_headers, records) = read_table(&file_bytes)?;
    // The mapping is keyed on lowercase names, which is what this path has always done.
    let headers = raw_headers
        .iter()
        .map(|h| h.to_lowercase())
        .collect::<Vec<_>>();

    // Allowed contact fields for mapping
    let allowed_fields: std::collections::HashSet<&str> = [
        "first_name",
        "last_name",
        "email",
        "phone",
        "title",
        "company",
        "notes",
        "address_line1",
        "address_line2",
        "city",
        "state",
        "postal_code",
        "country",
        "gender",
    ]
    .into_iter()
    .collect();

    // Validate: every mapped csv_header must exist in actual CSV headers
    for (csv_hdr, contact_field) in &mapping_map {
        if !headers.contains(csv_hdr) {
            return Err(AppError::BadRequest(format!(
                "CSV header '{}' not found in file. Available headers: {}",
                csv_hdr,
                headers.join(", ")
            )));
        }
        if !allowed_fields.contains(contact_field.as_str()) {
            return Err(AppError::BadRequest(format!(
                "Unknown contact field: '{}'. Allowed: {}",
                contact_field,
                allowed_fields
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    // Build index lookup: csv header index -> contact field
    let header_indices: std::collections::HashMap<String, usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.clone(), i))
        .collect();

    // Usage ceiling (`limit_max_contacts`): a bulk import is still an ADD, so it is gated the same
    // way as POST /api/contacts — refuse to START when the workspace is already at its ceiling
    // (402 with the same body), and stop ADDING rows once the ceiling is reached mid-file, so an
    // import can never overshoot a count-based limit (kanban t_f49e4299).
    let mut contact_usage = crate::features::count_contacts(&app_state.db, account_id).await;
    let contact_ceiling = crate::features::enforce_usage_limit(
        &app_state.db,
        account_id,
        "limit_max_contacts",
        "Contact",
        "contacts",
        contact_usage,
    )
    .await?;

    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<String> = vec![];

    let mut row_num = 0usize;

    for record in records {
        row_num += 1;

        if let Some(max) = contact_ceiling {
            if contact_usage >= max {
                errors.push(format!(
                    "Row {}: contact limit reached ({}/{}) — not imported. Upgrade your plan for more contacts.",
                    row_num, contact_usage, max
                ));
                skipped += 1;
                continue;
            }
        }

        if row_num > MAX_ROWS {
            errors.push(format!("Row {}: exceeded max rows ({})", row_num, MAX_ROWS));
            skipped += 1;
            continue;
        }

        // Map CSV columns to contact fields
        let mut mapped: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (csv_hdr, contact_field) in &mapping_map {
            if let Some(&idx) = header_indices.get(csv_hdr) {
                if let Some(value) = record.get(idx) {
                    mapped
                        .entry(contact_field.clone())
                        .or_insert_with(|| value.to_string());
                }
            }
        }

        // Validate
        if let Err(e) = validate_row(&mapped) {
            errors.push(format!("Row {}: {}", row_num, e));
            skipped += 1;
            continue;
        }

        // Insert
        let first_name = mapped.get("first_name").map(|v| v.trim()).unwrap_or("");
        let last_name = mapped.get("last_name").map(|v| v.trim()).unwrap_or("");
        // `company` is in allowed_fields and every caller maps it, but the INSERT never bound it —
        // the value was accepted and silently dropped (found live 2026-09-22: importing a row with
        // Company set then exporting it produced an empty company column).
        let company = mapped
            .get("company")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let email = mapped
            .get("email")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let phone = mapped
            .get("phone")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let title = mapped
            .get("title")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let notes = mapped
            .get("notes")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let address_line1 = mapped
            .get("address_line1")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let address_line2 = mapped
            .get("address_line2")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let city = mapped
            .get("city")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let state = mapped
            .get("state")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let postal_code = mapped
            .get("postal_code")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let country = mapped
            .get("country")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let gender = mapped
            .get("gender")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());

        let result = sqlx::query(
            r#"INSERT INTO contacts
               (id, tenant_id, email, phone, first_name, last_name, company, title, notes,
                address_line1, address_line2, city, state, postal_code, country, gender, is_active)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,true)"#,
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(email)
        .bind(phone)
        .bind(first_name)
        .bind(last_name)
        .bind(company)
        .bind(title)
        .bind(notes)
        .bind(address_line1)
        .bind(address_line2)
        .bind(city)
        .bind(state)
        .bind(postal_code)
        .bind(country)
        .bind(gender)
        .execute(&app_state.db)
        .await;

        match result {
            Ok(_) => {
                imported += 1;
                // one more row now exists, so the ceiling is re-checked against the real count
                contact_usage += 1;
            }
            Err(e) => {
                errors.push(format!("Row {}: DB error: {}", row_num, e));
                skipped += 1;
            }
        }
    }

    Ok(Json(serde_json::json!({
        "imported": imported,
        "skipped": skipped,
        "errors": errors,
    })))
}

/// POST /api/csv/preview
///
/// Preview a CSV file without importing. Returns headers and first 5 rows.
/// Accepts multipart form with `file` field.
pub async fn preview_csv(
    State(_state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    mut multipart: Multipart,
) -> ApiResult<impl IntoResponse> {
    let mut file_bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {}", e)))?
    {
        if let Some("file") = field.name() {
            file_bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read file: {}", e)))?
                    .to_vec(),
            );
            break;
        }
    }

    let file_bytes =
        file_bytes.ok_or_else(|| AppError::BadRequest("Missing 'file' field".into()))?;

    if file_bytes.len() as u64 > MAX_UPLOAD_BYTES {
        return Err(AppError::BadRequest(format!(
            "File too large: {} bytes (max {})",
            file_bytes.len(),
            MAX_UPLOAD_BYTES
        )));
    }

    // Same reader as the import path, so a preview of an .xlsx shows what the import would actually take.
    // Headers stay AS AUTHORED here: this is the mapping UI and it must show the file's real column names.
    let (headers, records) = read_table(&file_bytes)?;

    let mut sample_rows: Vec<Vec<String>> = Vec::with_capacity(PREVIEW_ROWS);
    for record in records.into_iter().take(PREVIEW_ROWS) {
        sample_rows.push(record.iter().map(|f| f.to_string()).collect());
    }

    Ok(Json(serde_json::json!({
        "headers": headers,
        "sample_rows": sample_rows,
    })))
}

/// POST /api/csv/import/opportunities — import deals from CSV, XLS or XLSX.
///
/// The export has always written `name, contact, company, pipeline, stage, value, probability,
/// expected_close, created_at`, and nothing could read it back: measured 2026-10-02, `/api/csv` was
/// {preview, import/contacts, export/contacts, export/opportunities}. **An export you cannot re-import is
/// a backup you cannot restore**, so the accepted column names are deliberately the export's own and no
/// mapping is required for a round-trip. A `mappings` part is still honoured if supplied.
///
/// `opportunities.pipeline_id` and `stage_id` are NOT NULL, so every row must land in a real pipeline AND
/// a real stage OF THAT PIPELINE — a stage belonging to a different pipeline is not a valid target.
/// Resolution per row: the named pipeline/stage when both exist and agree; else the tenant's first
/// pipeline and its first stage; and a row whose pipeline has no stages is REFUSED with a reason rather
/// than silently dropped or 500'd.
pub async fn import_opportunities(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    mut multipart: Multipart,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let mut file_bytes: Option<Vec<u8>> = None;
    let mut mappings_str: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            file_bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read file: {}", e)))?
                    .to_vec(),
            );
        } else if name == "mappings" {
            mappings_str = Some(field.text().await.unwrap_or_default());
        }
    }
    let file_bytes =
        file_bytes.ok_or_else(|| AppError::BadRequest("Missing 'file' field".into()))?;
    if file_bytes.len() as u64 > MAX_UPLOAD_BYTES {
        return Err(AppError::BadRequest(format!(
            "File too large: {} bytes (max {})",
            file_bytes.len(),
            MAX_UPLOAD_BYTES
        )));
    }

    let (raw_headers, records) = read_table(&file_bytes)?;
    let headers = raw_headers
        .iter()
        .map(|h| h.to_lowercase())
        .collect::<Vec<_>>();
    let header_indices: std::collections::HashMap<String, usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.trim().to_string(), i))
        .collect();

    // "contact" accepts an email OR a full name; both are matched case-insensitively.
    let field_of = |header: &str| -> Option<usize> { header_indices.get(header).copied() };
    let id_name = field_of("name");
    let id_contact = field_of("contact");
    let id_company = field_of("company");
    let id_pipeline = field_of("pipeline");
    let id_stage = field_of("stage");
    let id_value = field_of("value");
    let id_probability = field_of("probability");
    let id_close = field_of("expected_close").or_else(|| field_of("expected_close_date"));
    let _ = &mappings_str; // an explicit map is optional; the export's own names already line up

    // The tenant's first pipeline + its first stage: the fallback that keeps a row importable when the
    // file names a pipeline that has since been renamed or deleted.
    // The fallback must be a pipeline that actually HAS a stage, and the INNER JOIN is what guarantees it:
    // selecting the oldest pipeline and then looking for a stage in it fails for every row when that
    // pipeline happens to be empty — measured live 2026-10-02, where it rejected an entire file whose rows
    // never even reached their own field validation. `is_active` is included for the same reason: a deal
    // belongs in a pipeline the tenant is actually using.
    let default_target = sqlx::query_as::<_, (Uuid, Uuid)>(
        r#"SELECT p.id, s.id
             FROM pipelines p
             JOIN pipeline_stages s ON s.pipeline_id = p.id
            WHERE p.tenant_id = $1 AND p.is_active = true
            ORDER BY p.created_at ASC, s.position ASC NULLS LAST, s.created_at ASC
            LIMIT 1"#,
    )
    .bind(account_id)
    .fetch_optional(&state.db)
    .await?;

    let mut imported = 0i64;
    let mut skipped = 0i64;
    let mut errors: Vec<String> = Vec::new();

    for (n, record) in records.into_iter().enumerate() {
        let row_num = n + 1;
        let get = |idx: Option<usize>| -> String {
            idx.and_then(|i| record.get(i))
                .unwrap_or("")
                .trim()
                .to_string()
        };

        let name = get(id_name);
        if name.is_empty() {
            errors.push(format!("Row {}: 'name' is required", row_num));
            skipped += 1;
            continue;
        }

        // pipeline + stage must both be real, and the stage must belong to that pipeline
        let named_pipeline = get(id_pipeline);
        let named_stage = get(id_stage);
        let mut pipeline_id: Option<Uuid> = None;
        let mut stage_id: Option<Uuid> = None;

        if !named_pipeline.is_empty() {
            pipeline_id = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM pipelines WHERE tenant_id = $1 AND lower(name) = lower($2) LIMIT 1",
            )
            .bind(account_id)
            .bind(named_pipeline.trim())
            .fetch_optional(&state.db)
            .await?;
        }
        if pipeline_id.is_none() {
            pipeline_id = default_target.as_ref().map(|(p, _)| *p);
        }
        // With no pipeline named, the fallback's stage comes with it.
        if named_pipeline.is_empty() {
            stage_id = default_target.as_ref().map(|(_, sid)| *sid);
        }
        if let Some(pid) = pipeline_id {
            if !named_stage.is_empty() {
                stage_id = sqlx::query_scalar::<_, Uuid>(
                    "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 AND lower(name) = lower($2) LIMIT 1",
                )
                .bind(pid)
                .bind(named_stage.trim())
                .fetch_optional(&state.db)
                .await?;
            }
            if stage_id.is_none() {
                stage_id = sqlx::query_scalar::<_, Uuid>(
                    "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position ASC NULLS LAST, created_at ASC LIMIT 1",
                )
                .bind(pid)
                .fetch_optional(&state.db)
                .await?;
            }
        }

        let (Some(pid), Some(sid)) = (pipeline_id, stage_id) else {
            errors.push(format!(
                "Row {}: no pipeline with a stage to import into — create a stage first",
                row_num
            ));
            skipped += 1;
            continue;
        };

        // value: numeric column, bound through an explicit cast (the convention this file already uses)
        let value: f64 = match get(id_value).parse::<f64>() {
            Ok(v) => v,
            Err(_) if get(id_value).is_empty() => 0.0,
            Err(_) => {
                errors.push(format!("Row {}: 'value' is not a number", row_num));
                skipped += 1;
                continue;
            }
        };

        // probability must be a real percentage — refused rather than clamped, because clamping hides a
        // bad export instead of telling the operator about it
        let probability: i32 = match get(id_probability).parse::<i32>() {
            Ok(p) if (0..=100).contains(&p) => p,
            Ok(p) => {
                errors.push(format!(
                    "Row {}: probability {} is outside 0-100",
                    row_num, p
                ));
                skipped += 1;
                continue;
            }
            Err(_) if get(id_probability).is_empty() => 0,
            Err(_) => {
                errors.push(format!("Row {}: 'probability' is not a number", row_num));
                skipped += 1;
                continue;
            }
        };

        let expected_close: Option<chrono::NaiveDate> = {
            let raw = get(id_close);
            if raw.is_empty() {
                None
            } else {
                match chrono::NaiveDate::parse_from_str(
                    raw.split('T').next().unwrap_or(&raw),
                    "%Y-%m-%d",
                ) {
                    Ok(d) => Some(d),
                    Err(_) => {
                        errors.push(format!(
                            "Row {}: 'expected_close' must be YYYY-MM-DD",
                            row_num
                        ));
                        skipped += 1;
                        continue;
                    }
                }
            }
        };

        // contact: an email (exact, case-insensitive) or a full name. Unmatched stays UNLINKED — a deal
        // without a contact is a supported state, and inventing one from a display name would fabricate
        // records (the same reason the deal create path requires an email to auto-create).
        let contact_raw = get(id_contact);
        let contact_id = if contact_raw.is_empty() {
            None
        } else if contact_raw.contains('@') {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM contacts WHERE tenant_id = $1 AND lower(email) = lower($2) AND is_active = true LIMIT 1",
            )
            .bind(account_id)
            .bind(contact_raw.trim())
            .fetch_optional(&state.db)
            .await?
        } else {
            sqlx::query_scalar::<_, Uuid>(
                r#"SELECT id FROM contacts WHERE tenant_id = $1 AND is_active = true
                     AND lower(first_name || ' ' || last_name) = lower($2) LIMIT 1"#,
            )
            .bind(account_id)
            .bind(contact_raw.trim())
            .fetch_optional(&state.db)
            .await?
        };

        let company_raw = get(id_company);
        let company_id = if company_raw.is_empty() {
            None
        } else {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM companies WHERE tenant_id = $1 AND lower(name) = lower($2) LIMIT 1",
            )
            .bind(account_id)
            .bind(company_raw.trim())
            .fetch_optional(&state.db)
            .await?
        };

        let res = sqlx::query(
            r#"INSERT INTO opportunities
                 (id, tenant_id, pipeline_id, stage_id, contact_id, company_id, name,
                  value, probability, expected_close_date, source)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8::numeric,$9,$10,'import')"#,
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(pid)
        .bind(sid)
        .bind(contact_id)
        .bind(company_id)
        .bind(&name)
        .bind(value)
        .bind(probability)
        .bind(expected_close)
        .execute(&state.db)
        .await;

        match res {
            Ok(_) => imported += 1,
            Err(e) => {
                errors.push(format!("Row {}: DB error: {}", row_num, e));
                skipped += 1;
            }
        }
    }

    Ok(Json(serde_json::json!({
        "imported": imported,
        "skipped": skipped,
        "errors": errors,
    })))
}

// ── Export ─────────────────────────────────────────────────────────────────

/// GET /api/csv/export/contacts
///
/// Export active contacts for the tenant as a CSV download.
pub async fn export_contacts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // `contacts_extended` does not exist in this app's database (information_schema has only
    // `contacts`), so this query 500'd on every tenant holding a contact. Read the real table;
    // `tags` is not a column — it is the tag_assignments/tags pair (entity_type='contact'),
    // aggregated the same way src/tags/handlers.rs reads them.
    let rows = sqlx::query_as::<_, ContactExportRow>(
        r#"SELECT c.first_name, c.last_name, c.email, c.phone, c.title, c.company,
                  (SELECT string_agg(t.name, ';' ORDER BY t.name)
                     FROM tag_assignments ta
                     JOIN tags t ON t.id = ta.tag_id
                    WHERE ta.entity_type = 'contact' AND ta.entity_id = c.id
                      AND t.is_active = true) AS tags,
                  c.created_at, c.updated_at
           FROM contacts c
           WHERE c.tenant_id = $1 AND c.is_active = true
           ORDER BY c.created_at DESC"#,
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await?;

    let mut wtr = WriterBuilder::new().from_writer(vec![]);

    // Write header
    wtr.write_record([
        "first_name",
        "last_name",
        "email",
        "phone",
        "title",
        "company",
        "tags",
        "created_at",
        "updated_at",
    ])
    .map_err(|e| AppError::Internal(format!("CSV write error: {}", e)))?;

    for row in &rows {
        wtr.write_record([
            &row.first_name,
            &row.last_name,
            row.email.as_deref().unwrap_or(""),
            row.phone.as_deref().unwrap_or(""),
            row.title.as_deref().unwrap_or(""),
            row.company.as_deref().unwrap_or(""),
            row.tags.as_deref().unwrap_or(""),
            &row.created_at.map(|d| d.to_rfc3339()).unwrap_or_default(),
            &row.updated_at.map(|d| d.to_rfc3339()).unwrap_or_default(),
        ])
        .map_err(|e| AppError::Internal(format!("CSV write error: {}", e)))?;
    }

    let csv_bytes = wtr
        .into_inner()
        .map_err(|e| AppError::Internal(format!("CSV flush error: {}", e)))?;

    let headers = [
        (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
        (
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"contacts_export.csv\"",
        ),
    ];

    Ok((StatusCode::OK, headers, csv_bytes))
}

/// GET /api/csv/export/opportunities
///
/// Export opportunities for the tenant with pipeline/contact names as a CSV download.
pub async fn export_opportunities(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // `opportunities.value` is NUMERIC, and sqlx has no NUMERIC -> f64 decode: without the cast
    // the WHOLE row fails to decode (OppExportRow.value is Option<f64>), so the export comes back
    // empty/erroring while the table holds rows. Same cast src/pipelines/opportunity.rs uses.
    let rows = sqlx::query_as::<_, OppExportRow>(
        r#"SELECT
              o.name,
              COALESCE(c.first_name || ' ' || c.last_name, '') AS contact_name,
              COALESCE(co.name, '') AS company,
              COALESCE(p.name, '') AS pipeline,
              COALESCE(s.name, '') AS stage,
              o.value::float8 AS value,
              o.probability,
              o.expected_close_date,
              o.created_at
           FROM opportunities o
           LEFT JOIN contacts c ON c.id = o.contact_id AND c.is_active = true
           LEFT JOIN companies co ON co.id = o.company_id
           LEFT JOIN pipelines p ON p.id = o.pipeline_id
           LEFT JOIN pipeline_stages s ON s.id = o.stage_id
           WHERE o.tenant_id = $1
           ORDER BY o.created_at DESC"#,
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await?;

    let mut wtr = WriterBuilder::new().from_writer(vec![]);

    wtr.write_record([
        "name",
        "contact",
        "company",
        "pipeline",
        "stage",
        "value",
        "probability",
        "expected_close",
        "created_at",
    ])
    .map_err(|e| AppError::Internal(format!("CSV write error: {}", e)))?;

    for row in &rows {
        let value_str = row.value.map(|v| v.to_string()).unwrap_or_default();
        let prob_str = row.probability.map(|p| p.to_string()).unwrap_or_default();
        let close_str = row
            .expected_close_date
            .map(|d| d.to_string())
            .unwrap_or_default();
        let created_str = row.created_at.map(|d| d.to_rfc3339()).unwrap_or_default();

        wtr.write_record([
            &row.name,
            &row.contact_name,
            &row.company,
            &row.pipeline,
            &row.stage,
            &value_str,
            &prob_str,
            &close_str,
            &created_str,
        ])
        .map_err(|e| AppError::Internal(format!("CSV write error: {}", e)))?;
    }

    let csv_bytes = wtr
        .into_inner()
        .map_err(|e| AppError::Internal(format!("CSV flush error: {}", e)))?;

    let headers = [
        (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
        (
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"opportunities_export.csv\"",
        ),
    ];

    Ok((StatusCode::OK, headers, csv_bytes))
}

// ── SQL row types ──────────────────────────────────────────────────────────

/// Row shape for contacts export query.
#[derive(Debug, sqlx::FromRow)]
struct ContactExportRow {
    first_name: String,
    last_name: String,
    email: Option<String>,
    phone: Option<String>,
    title: Option<String>,
    company: Option<String>,
    tags: Option<String>,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Row shape for opportunities export query.
#[derive(Debug, sqlx::FromRow)]
struct OppExportRow {
    name: String,
    contact_name: String,
    company: String,
    pipeline: String,
    stage: String,
    value: Option<f64>,
    probability: Option<i32>,
    expected_close_date: Option<chrono::NaiveDate>,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ── Router ─────────────────────────────────────────────────────────────────

/// Build the CSV handler router with auth middleware.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/preview", post(preview_csv))
        .route("/import/contacts", post(import_contacts))
        .route("/export/contacts", get(export_contacts))
        .route("/export/opportunities", get(export_opportunities))
        .route("/import/opportunities", post(import_opportunities))
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
        .with_state(state)
}
