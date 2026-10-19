//! The query shape saved views and alerts share (R-P2-17): what rows to
//! match. It is stored as JSON, but never reaches SQL as text: every query
//! is turned back into search's query pairs and validated by
//! [`query::parse_filters`], the parser of the search endpoint itself.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::query::{self, LogFilters, MAX_WINDOW};
use crate::error::AppError;

pub const VIEW_RANGES: [&str; 6] = ["15m", "1h", "6h", "24h", "7d", "custom"];
pub const MAX_VIEW_COLUMNS: usize = 10;
/// Most bytes a query may take once serialized (the validators bound every
/// part already; this is the explicit ceiling of what is stored).
pub const MAX_QUERY_BYTES: usize = 16 * 1024;

fn default_range() -> String {
    "1h".to_string()
}

/// What rows a view or an alert matches. Validated exactly like search:
/// [`LogQuery::to_filters`] turns it into query pairs and runs
/// [`query::parse_filters`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogQuery {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub q: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_fields: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_ids: Vec<Uuid>,
    /// The explorer's filter objects: `{"path","op","value"?}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<serde_json::Value>,
    /// Decimal strings (a UInt64 does not fit a JavaScript number).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pattern_hashes: Vec<String>,
}

impl LogQuery {
    pub fn pairs(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        if !self.q.is_empty() {
            pairs.push(("q".to_string(), self.q.clone()));
        }
        if self.in_fields {
            pairs.push(("in_fields".to_string(), "true".to_string()));
        }
        pairs.extend(self.levels.iter().map(|v| ("level".to_string(), v.clone())));
        pairs.extend(
            self.source_ids
                .iter()
                .map(|v| ("source_id".to_string(), v.to_string())),
        );
        pairs.extend(
            self.filters
                .iter()
                .map(|v| ("filter".to_string(), v.to_string())),
        );
        pairs.extend(
            self.pattern_hashes
                .iter()
                .map(|v| ("pattern_hash".to_string(), v.clone())),
        );
        pairs
    }

    pub fn to_filters(&self) -> Result<LogFilters, String> {
        query::parse_filters(&self.pairs())
    }
}

/// A saved explorer state: a [`LogQuery`] plus the time range and columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewQuery {
    #[serde(default = "default_range")]
    pub range: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Utc>>,
    // Not `#[serde(flatten)]` over LogQuery: serde does not support
    // `deny_unknown_fields` together with `flatten`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub q: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_fields: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pattern_hashes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
}

impl ViewQuery {
    pub fn log_query(&self) -> LogQuery {
        LogQuery {
            q: self.q.clone(),
            in_fields: self.in_fields,
            levels: self.levels.clone(),
            source_ids: self.source_ids.clone(),
            filters: self.filters.clone(),
            pattern_hashes: self.pattern_hashes.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !VIEW_RANGES.contains(&self.range.as_str()) {
            return Err("range must be one of 15m, 1h, 6h, 24h, 7d, custom".to_string());
        }
        if self.range == "custom" {
            let (Some(from), Some(to)) = (self.from, self.to) else {
                return Err("range custom needs from and to".to_string());
            };
            if from >= to {
                return Err("from must be before to".to_string());
            }
            if to - from > MAX_WINDOW {
                return Err("the time window may span at most 31 days".to_string());
            }
        } else if self.from.is_some() || self.to.is_some() {
            return Err("from and to are only used with range custom".to_string());
        }
        if self.columns.len() > MAX_VIEW_COLUMNS {
            return Err(format!("at most {MAX_VIEW_COLUMNS} columns"));
        }
        for column in &self.columns {
            query::parse_path(column).map_err(|e| format!("columns: {e}"))?;
        }
        self.log_query().to_filters()?;
        match serde_json::to_vec(self) {
            Ok(bytes) if bytes.len() <= MAX_QUERY_BYTES => Ok(()),
            _ => Err(format!("query must be at most {MAX_QUERY_BYTES} bytes")),
        }
    }
}

/// 400 unless every id is a source of the project.
pub async fn check_sources(pool: &PgPool, project_id: Uuid, ids: &[Uuid]) -> Result<(), AppError> {
    let mut distinct = ids.to_vec();
    distinct.sort();
    distinct.dedup();
    if distinct.is_empty() {
        return Ok(());
    }
    let found: i64 =
        sqlx::query_scalar("select count(*) from sources where project_id = $1 and id = any($2)")
            .bind(project_id)
            .bind(&distinct)
            .fetch_one(pool)
            .await?;
    if found != distinct.len() as i64 {
        return Err(AppError::Validation(
            "source_ids: every source must belong to this project".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn view(value: serde_json::Value) -> Result<ViewQuery, String> {
        let view: ViewQuery = serde_json::from_value(value).map_err(|e| e.to_string())?;
        view.validate().map(|()| view)
    }

    #[test]
    fn a_view_validates_like_search() {
        let ok = view(json!({
            "range": "6h", "q": "disk", "in_fields": true, "levels": ["error"],
            "filters": [{"path":"http.status","op":"gte","value":"500"}],
            "pattern_hashes": ["123"], "columns": ["user.name"]
        }))
        .unwrap();
        let filters = ok.log_query().to_filters().unwrap();
        assert_eq!(filters.levels, ["error"]);
        assert_eq!(filters.pattern_hashes, [123]);
        assert!(filters.in_fields);
        assert_eq!(view(json!({})).unwrap().range, "1h");
        for bad in [
            json!({"range": "2h"}),
            json!({"range": "custom"}),
            json!({"range": "custom", "from": "2026-10-02T00:00:00Z", "to": "2026-10-01T00:00:00Z"}),
            json!({"range": "custom", "from": "2026-01-01T00:00:00Z", "to": "2026-03-01T00:00:00Z"}),
            json!({"range": "1h", "from": "2026-10-01T00:00:00Z"}),
            json!({"levels": ["loud"]}),
            json!({"filters": [{"path":"a","op":"gt","value":"x"}]}),
            json!({"filters": ["not an object"]}),
            json!({"pattern_hashes": ["-1"]}),
            json!({"columns": ["a..b"]}),
            json!({"columns": (0..11).map(|n| format!("c{n}")).collect::<Vec<_>>()}),
            json!({"q": "x".repeat(1025)}),
            json!({"surprise": 1}),
        ] {
            assert!(view(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_custom_range_within_the_window_is_valid() {
        let ok = view(
            json!({"range": "custom", "from": "2026-10-01T00:00:00Z", "to": "2026-10-02T00:00:00Z"}),
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    #[test]
    fn queries_round_trip_without_defaults() {
        let value = json!({"range":"15m","q":"a","levels":["warn"],"columns":["x"]});
        let parsed: ViewQuery = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
        assert_eq!(
            serde_json::to_value(LogQuery::default()).unwrap(),
            json!({})
        );
    }

    #[test]
    fn hostile_text_stays_a_filter_value() {
        let q = view(json!({
            "q": "'; drop table logs; --",
            "filters": [{"path":"a","op":"eq","value":"\"; x"}]
        }))
        .unwrap();
        assert!(q.log_query().to_filters().is_ok());
    }
}
