//! CSV import/export handlers for contacts and opportunities.
//!
//! Provides drag-and-drop CSV upload with column mapping preview,
//! batch import up to 500 records, and streaming CSV export.

use axum::{
    extract::{Multipart, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Extension, Json, Router,
};
use axum::routing::{get, post};
use csv::{ReaderBuilder, WriterBuilder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::auth::models::Claims;
use crate::errors::{AppError, ApiResult};

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
            "mappings" => mappings_str = Some(
                String::from_utf8(data.to_vec())
                    .map_err(|_| AppError::BadRequest("mappings must be valid UTF-8".into()))?,
            ),
            _ => {}
        }
    }

    let file_bytes = file_bytes.ok_or_else(|| AppError::BadRequest("Missing 'file' field".into()))?;
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

    // Parse CSV
    let mut reader = ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(file_bytes.as_slice());

    let headers = reader
        .headers()
        .map_err(|e| AppError::BadRequest(format!("Failed to read CSV headers: {}", e)))?
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
                allowed_fields.iter().cloned().collect::<Vec<_>>().join(", ")
            )));
        }
    }

    // Build index lookup: csv header index -> contact field
    let header_indices: std::collections::HashMap<String, usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.clone(), i))
        .collect();

    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<String> = vec![];

    let mut row_num = 0usize;

    for result in reader.records() {
        row_num += 1;

        if row_num > MAX_ROWS {
            errors.push(format!("Row {}: exceeded max rows ({})", row_num, MAX_ROWS));
            skipped += 1;
            continue;
        }

        let record =
            result.map_err(|e| AppError::BadRequest(format!("CSV parse error at row {}: {}", row_num, e)))?;

        // Map CSV columns to contact fields
        let mut mapped: std::collections::HashMap<String, String> = std::collections::HashMap::new();
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
        let email = mapped.get("email").map(|v| v.trim()).filter(|v| !v.is_empty());
        let phone = mapped.get("phone").map(|v| v.trim()).filter(|v| !v.is_empty());
        let title = mapped.get("title").map(|v| v.trim()).filter(|v| !v.is_empty());
        let notes = mapped.get("notes").map(|v| v.trim()).filter(|v| !v.is_empty());
        let address_line1 = mapped
            .get("address_line1")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let address_line2 = mapped
            .get("address_line2")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let city = mapped.get("city").map(|v| v.trim()).filter(|v| !v.is_empty());
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
               (id, tenant_id, email, phone, first_name, last_name, title, notes,
                address_line1, address_line2, city, state, postal_code, country, gender, is_active)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,true)"#,
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(email)
        .bind(phone)
        .bind(first_name)
        .bind(last_name)
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

    let mut reader = ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(file_bytes.as_slice());

    let headers = reader
        .headers()
        .map_err(|e| AppError::BadRequest(format!("Failed to read CSV headers: {}", e)))?
        .iter()
        .map(|h| h.to_string())
        .collect::<Vec<_>>();

    let mut sample_rows: Vec<Vec<String>> = Vec::with_capacity(PREVIEW_ROWS);
    for result in reader.records().take(PREVIEW_ROWS) {
        let record = result.map_err(|e| AppError::BadRequest(format!("CSV parse error: {}", e)))?;
        sample_rows.push(record.iter().map(|f| f.to_string()).collect());
    }

    Ok(Json(serde_json::json!({
        "headers": headers,
        "sample_rows": sample_rows,
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

    let rows = sqlx::query_as::<_, ContactExportRow>(
        r#"SELECT first_name, last_name, email, phone, title, company, tags,
                  created_at, updated_at
           FROM contacts_extended
           WHERE tenant_id = $1 AND is_active = true
           ORDER BY created_at DESC"#,
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

    let rows = sqlx::query_as::<_, OppExportRow>(
        r#"SELECT
              o.name,
              COALESCE(c.first_name || ' ' || c.last_name, '') AS contact_name,
              COALESCE(co.name, '') AS company,
              COALESCE(p.name, '') AS pipeline,
              COALESCE(s.name, '') AS stage,
              o.value,
              o.probability,
              o.expected_close_date,
              o.created_at
           FROM opportunities o
           LEFT JOIN contacts c ON c.id = o.contact_id AND c.is_active = true
           LEFT JOIN companies co ON co.id = o.company_id
           LEFT JOIN pipelines p ON p.id = o.pipeline_id
           LEFT JOIN pipeline_stages s ON s.id = o.stage_id
           WHERE o.account_id = $1
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
        let created_str = row
            .created_at
            .map(|d| d.to_rfc3339())
            .unwrap_or_default();

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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
        .with_state(state)
}
