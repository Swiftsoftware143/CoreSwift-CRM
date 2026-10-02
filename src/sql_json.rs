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
/// A COMPILE-TIME macro, not a `format!` (kanban t_a4cb4ba7): the wrapper is assembled by `concat!`
/// from the CALLER's literal query, so the statement text a request runs is a constant and no query
/// is BUILT at run time — gate rule 5d / class 14. The stronger property that comes with it: a
/// caller can no longer pass a run-time string (that is now a compile error), which is exactly the
/// distinction the rule exists to enforce. The bytes are identical to what the `format!` produced.
///
/// ```ignore
/// let rows = sqlx::query_scalar::<_, serde_json::Value>(
///     row_json!("SELECT id, name FROM tags WHERE tenant_id = $1"))
///     .bind(tenant_id)
///     .fetch_all(db).await?;   // Vec<Value>, one object per row
/// ```
///
/// Do NOT wrap a SELECT that already returns a single json column — that would
/// double-wrap it into `{"coalesce":[...]}`.
macro_rules! row_json {
    ($sql:expr $(,)?) => {
        concat!("SELECT row_to_json(t) FROM (", $sql, ") t")
    };
}
pub(crate) use row_json;

/// Wrap an `INSERT ... RETURNING ...` so PostgreSQL serialises the returned row
/// as ONE flat json object. Same compile-time discipline as `row_json!`.
///
/// A data-modifying statement cannot sit in a `FROM` subquery
/// (`SELECT row_to_json(t) FROM (INSERT ...) t` is a syntax error), but a
/// data-modifying CTE can, so the INSERT is executed there and de-tupled here.
macro_rules! row_json_dml {
    ($sql:expr $(,)?) => {
        concat!("WITH ins AS (", $sql, ") SELECT row_to_json(ins) FROM ins")
    };
}
pub(crate) use row_json_dml;
