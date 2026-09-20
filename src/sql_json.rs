//! JSON row serialisation for read paths.
//!
//! The CRM read paths decode rows with `query_scalar::<_, serde_json::Value>`,
//! which needs a statement that returns ONE json/jsonb column. Decoding several
//! bare SELECT columns into a one-value tuple is impossible for sqlx: it only
//! deserialises column 0 into `serde_json::Value`, and only when that column is
//! json/jsonb. Everywhere else the query compiles fine but fails at runtime with
//! `error occurred while decoding column 0: mismatched types` (HTTP 400 for every
//! tenant), and where column 0 IS json the remaining columns are silently
//! dropped and the 1-tuple serialises as a nested `[[{...}]]` array.
//!
//! Letting PostgreSQL build the object avoids both failure modes.

/// Wrap a SELECT so PostgreSQL serialises every row as ONE flat json object.
///
/// ```ignore
/// let rows = sqlx::query_scalar::<_, serde_json::Value>(
///     &row_json("SELECT id, name FROM tags WHERE tenant_id = $1"))
///     .bind(tenant_id)
///     .fetch_all(db).await?;   // Vec<Value>, one object per row
/// ```
///
/// Do NOT wrap a SELECT that already returns a single json column — that would
/// double-wrap it into `{"coalesce":[...]}`.
pub fn row_json(sql: &str) -> String {
    format!("SELECT row_to_json(t) FROM ({}) t", sql)
}

/// Wrap an `INSERT ... RETURNING ...` so PostgreSQL serialises the returned row
/// as ONE flat json object.
///
/// A data-modifying statement cannot sit in a `FROM` subquery
/// (`SELECT row_to_json(t) FROM (INSERT ...) t` is a syntax error), but a
/// data-modifying CTE can, so the INSERT is executed there and de-tupled here.
pub fn row_json_dml(sql: &str) -> String {
    format!("WITH ins AS ({}) SELECT row_to_json(ins) FROM ins", sql)
}
