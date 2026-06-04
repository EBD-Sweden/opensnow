use serde::{Deserialize, Serialize};

/// Auto-schema inference and suggestion engine.
/// Agents can describe data in natural language and get schema recommendations.

#[derive(Debug, Serialize, Deserialize)]
pub struct SchemaSuggestion {
    pub table_name: String,
    pub columns: Vec<ColumnSuggestion>,
    pub partition_by: Vec<String>,
    pub cluster_by: Vec<String>,
    pub create_sql: String,
    pub rationale: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ColumnSuggestion {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub description: String,
}

/// Suggest schema based on natural language description and industry context.
pub fn suggest_schema(description: &str, industry: Option<&str>) -> SchemaSuggestion {
    let lower = description.to_lowercase();

    // Detect common patterns and suggest schemas
    if lower.contains("transaction") || lower.contains("payment") || lower.contains("purchase") {
        return suggest_transaction_schema(description, industry);
    }
    if lower.contains("cdr") || lower.contains("call") || lower.contains("telecom") {
        return suggest_cdr_schema(description, industry);
    }
    if lower.contains("user") || lower.contains("customer") || lower.contains("subscriber") {
        return suggest_customer_schema(description, industry);
    }
    if lower.contains("event") || lower.contains("log") || lower.contains("metric") {
        return suggest_event_schema(description, industry);
    }

    // Generic fallback
    suggest_generic_schema(description)
}

fn suggest_transaction_schema(_desc: &str, industry: Option<&str>) -> SchemaSuggestion {
    let is_banking = industry == Some("banking");

    let mut columns = vec![
        col("txn_id", "VARCHAR", false, "Unique transaction identifier"),
        col("timestamp", "TIMESTAMP", false, "Transaction timestamp"),
        col("amount", "DOUBLE", false, "Transaction amount"),
        col("currency", "VARCHAR", false, "ISO 4217 currency code"),
        col(
            "status",
            "VARCHAR",
            false,
            "Transaction status (completed/pending/failed)",
        ),
    ];

    if is_banking {
        columns.extend(vec![
            col("account_from", "VARCHAR", false, "Source account IBAN"),
            col("account_to", "VARCHAR", false, "Destination account IBAN"),
            col(
                "txn_type",
                "VARCHAR",
                false,
                "Transaction type (debit/credit/transfer/payment)",
            ),
            col(
                "merchant_category",
                "VARCHAR",
                true,
                "Merchant category code",
            ),
            col(
                "channel",
                "VARCHAR",
                false,
                "Channel (online/atm/pos/branch/mobile)",
            ),
        ]);
    } else {
        columns.extend(vec![
            col("source_id", "VARCHAR", false, "Source entity ID"),
            col("target_id", "VARCHAR", true, "Target entity ID"),
            col("category", "VARCHAR", true, "Transaction category"),
        ]);
    }

    let table_name = "transactions".to_string();
    let create_sql = format_create_sql(&table_name, &columns);

    SchemaSuggestion {
        table_name,
        columns,
        partition_by: vec!["DATE_TRUNC('day', timestamp)".to_string()],
        cluster_by: vec!["timestamp".to_string()],
        create_sql,
        rationale: "Partitioned by day for time-range queries. Clustered by timestamp for efficient range scans. Amount as DOUBLE for precision. IBAN columns for banking compliance.".to_string(),
    }
}

fn suggest_cdr_schema(_desc: &str, _industry: Option<&str>) -> SchemaSuggestion {
    let columns = vec![
        col("cdr_id", "BIGINT", false, "Unique CDR identifier"),
        col("caller", "VARCHAR", false, "Calling party number (E.164)"),
        col("callee", "VARCHAR", false, "Called party number (E.164)"),
        col("start_time", "TIMESTAMP", false, "Call start timestamp"),
        col(
            "duration_seconds",
            "DOUBLE",
            false,
            "Call duration in seconds",
        ),
        col("call_type", "VARCHAR", false, "Call type (voice/sms/data)"),
        col("tower_id", "BIGINT", false, "Serving cell tower ID"),
        col(
            "mcc_mnc",
            "VARCHAR",
            false,
            "Mobile Country Code + Network Code",
        ),
        col(
            "call_status",
            "VARCHAR",
            false,
            "Call status (answered/missed/busy/failed)",
        ),
    ];

    let table_name = "cdrs".to_string();
    let create_sql = format_create_sql(&table_name, &columns);

    SchemaSuggestion {
        table_name,
        columns,
        partition_by: vec!["DATE_TRUNC('day', start_time)".to_string()],
        cluster_by: vec!["start_time".to_string(), "caller".to_string()],
        create_sql,
        rationale: "Partitioned by day for time-range queries. Clustered by time and caller for subscriber lookups. Bloom filter recommended on caller/callee for point queries.".to_string(),
    }
}

fn suggest_customer_schema(_desc: &str, industry: Option<&str>) -> SchemaSuggestion {
    let mut columns = vec![
        col(
            "customer_id",
            "VARCHAR",
            false,
            "Unique customer identifier",
        ),
        col("name", "VARCHAR", false, "Full name"),
        col("created_at", "TIMESTAMP", false, "Registration date"),
        col(
            "status",
            "VARCHAR",
            false,
            "Account status (active/inactive/suspended)",
        ),
    ];

    match industry {
        Some("banking") => {
            columns.extend(vec![
                col("kyc_status", "VARCHAR", false, "KYC verification status"),
                col(
                    "risk_score",
                    "INTEGER",
                    false,
                    "Risk assessment score (0-100)",
                ),
                col(
                    "segment",
                    "VARCHAR",
                    false,
                    "Customer segment (retail/private/corporate)",
                ),
                col("country", "VARCHAR", false, "ISO 3166-1 country code"),
            ]);
        }
        Some("telecom") => {
            columns.extend(vec![
                col("msisdn", "VARCHAR", false, "Mobile number (E.164)"),
                col(
                    "imsi",
                    "VARCHAR",
                    false,
                    "International Mobile Subscriber Identity",
                ),
                col("plan", "VARCHAR", false, "Subscription plan"),
                col("region", "VARCHAR", false, "Service region"),
                col("monthly_arpu", "DOUBLE", false, "Average Revenue Per User"),
            ]);
        }
        _ => {
            columns.extend(vec![
                col("email", "VARCHAR", true, "Email address"),
                col("segment", "VARCHAR", true, "Customer segment"),
                col("country", "VARCHAR", true, "Country code"),
            ]);
        }
    }

    let table_name = "customers".to_string();
    let create_sql = format_create_sql(&table_name, &columns);

    SchemaSuggestion {
        table_name,
        columns,
        partition_by: vec![],
        cluster_by: vec!["customer_id".to_string()],
        create_sql,
        rationale: "Clustered by customer_id for fast lookups. No time-based partitioning since customer data is typically small.".to_string(),
    }
}

fn suggest_event_schema(_desc: &str, _industry: Option<&str>) -> SchemaSuggestion {
    let columns = vec![
        col("event_id", "VARCHAR", false, "Unique event identifier"),
        col("timestamp", "TIMESTAMP", false, "Event timestamp"),
        col("event_type", "VARCHAR", false, "Event type/category"),
        col("source", "VARCHAR", false, "Event source system"),
        col(
            "severity",
            "VARCHAR",
            true,
            "Severity level (info/warn/error/critical)",
        ),
        col("payload", "VARCHAR", true, "Event payload (JSON)"),
        col("entity_id", "VARCHAR", true, "Related entity ID"),
    ];

    let table_name = "events".to_string();
    let create_sql = format_create_sql(&table_name, &columns);

    SchemaSuggestion {
        table_name,
        columns,
        partition_by: vec!["DATE_TRUNC('hour', timestamp)".to_string()],
        cluster_by: vec!["timestamp".to_string(), "event_type".to_string()],
        create_sql,
        rationale: "Partitioned by hour for high-volume event data. Clustered by time and type for efficient filtering. Payload stored as VARCHAR for semi-structured data (VARIANT type coming soon).".to_string(),
    }
}

fn suggest_generic_schema(desc: &str) -> SchemaSuggestion {
    let columns = vec![
        col("id", "BIGINT", false, "Primary identifier"),
        col(
            "created_at",
            "TIMESTAMP",
            false,
            "Record creation timestamp",
        ),
        col("data", "VARCHAR", true, "Data payload (JSON)"),
    ];

    let table_name = "data_table".to_string();
    let create_sql = format_create_sql(&table_name, &columns);

    SchemaSuggestion {
        table_name,
        columns,
        partition_by: vec![],
        cluster_by: vec!["id".to_string()],
        create_sql,
        rationale: format!(
            "Generic schema for: {}. Refine by providing more details about your data structure, or provide sample data for auto-inference.",
            desc
        ),
    }
}

fn col(name: &str, dt: &str, nullable: bool, desc: &str) -> ColumnSuggestion {
    ColumnSuggestion {
        name: name.to_string(),
        data_type: dt.to_string(),
        nullable,
        description: desc.to_string(),
    }
}

fn format_create_sql(table_name: &str, columns: &[ColumnSuggestion]) -> String {
    let col_defs: Vec<String> = columns
        .iter()
        .map(|c| {
            let null = if c.nullable { "" } else { " NOT NULL" };
            format!("    {} {}{}", c.name, c.data_type, null)
        })
        .collect();
    format!(
        "CREATE TABLE {} (\n{}\n);",
        table_name,
        col_defs.join(",\n")
    )
}

/// Infer schema from sample JSON data.
pub fn infer_schema_from_json(json_str: &str) -> Result<Vec<ColumnSuggestion>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("Invalid JSON: {}", e))?;

    let obj = match &value {
        serde_json::Value::Array(arr) if !arr.is_empty() => {
            arr[0].as_object().ok_or("Expected array of objects")?
        }
        serde_json::Value::Object(obj) => obj,
        _ => return Err("Expected JSON object or array of objects".to_string()),
    };

    let columns: Vec<ColumnSuggestion> = obj
        .iter()
        .map(|(key, val)| {
            let dt = match val {
                serde_json::Value::Number(n) if n.is_i64() => "BIGINT",
                serde_json::Value::Number(n) if n.is_f64() => "DOUBLE",
                serde_json::Value::Bool(_) => "BOOLEAN",
                serde_json::Value::String(s) if looks_like_timestamp(s) => "TIMESTAMP",
                serde_json::Value::String(s) if looks_like_date(s) => "DATE",
                serde_json::Value::String(_) => "VARCHAR",
                serde_json::Value::Null => "VARCHAR",
                serde_json::Value::Array(_) => "VARCHAR",
                serde_json::Value::Object(_) => "VARCHAR",
                _ => "VARCHAR",
            };
            ColumnSuggestion {
                name: key.clone(),
                data_type: dt.to_string(),
                nullable: val.is_null(),
                description: format!("Inferred from sample value: {}", truncate(val, 50)),
            }
        })
        .collect();

    Ok(columns)
}

fn looks_like_timestamp(s: &str) -> bool {
    s.len() >= 19 && (s.contains('T') || s.contains(' ')) && s.contains(':')
}

fn looks_like_date(s: &str) -> bool {
    s.len() == 10 && s.chars().filter(|c| *c == '-').count() == 2
}

fn truncate(val: &serde_json::Value, max: usize) -> String {
    let s = val.to_string();
    if s.len() > max {
        format!("{}...", &s[..max])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_suggest_banking_transaction() {
        let s = suggest_schema("customer purchase transactions", Some("banking"));
        assert_eq!(s.table_name, "transactions");
        assert!(s.columns.iter().any(|c| c.name == "account_from"));
    }

    #[test]
    fn test_suggest_telecom_cdr() {
        let s = suggest_schema("call detail records for voice calls", Some("telecom"));
        assert_eq!(s.table_name, "cdrs");
        assert!(s.columns.iter().any(|c| c.name == "caller"));
    }

    #[test]
    fn test_infer_from_json() {
        let json = r#"{"id": 1, "name": "test", "amount": 99.5, "active": true, "created": "2025-01-01T00:00:00"}"#;
        let cols = infer_schema_from_json(json).unwrap();
        assert_eq!(cols.len(), 5);
        assert!(
            cols.iter()
                .any(|c| c.name == "amount" && c.data_type == "DOUBLE")
        );
        assert!(
            cols.iter()
                .any(|c| c.name == "created" && c.data_type == "TIMESTAMP")
        );
    }

    #[test]
    fn test_infer_from_json_array() {
        let json = r#"[{"event_id": "abc", "ts": "2025-06-01T12:00:00", "count": 5}]"#;
        let cols = infer_schema_from_json(json).unwrap();
        assert_eq!(cols.len(), 3);
        assert!(
            cols.iter()
                .any(|c| c.name == "count" && c.data_type == "BIGINT")
        );
        assert!(
            cols.iter()
                .any(|c| c.name == "ts" && c.data_type == "TIMESTAMP")
        );
    }

    #[test]
    fn test_infer_from_json_date_column() {
        let json = r#"{"id": 1, "report_date": "2025-01-15"}"#;
        let cols = infer_schema_from_json(json).unwrap();
        assert!(
            cols.iter()
                .any(|c| c.name == "report_date" && c.data_type == "DATE")
        );
    }

    #[test]
    fn test_infer_from_invalid_json_returns_error() {
        assert!(infer_schema_from_json("not json at all").is_err());
        assert!(infer_schema_from_json("[]").is_err()); // empty array
        assert!(infer_schema_from_json("42").is_err()); // bare number
    }

    #[test]
    fn test_suggest_customer_telecom() {
        let s = suggest_schema("subscriber data with MSISDN", Some("telecom"));
        assert_eq!(s.table_name, "customers");
        assert!(s.columns.iter().any(|c| c.name == "msisdn"));
    }

    #[test]
    fn test_suggest_event_schema() {
        let s = suggest_schema("application event log with severity", None);
        assert_eq!(s.table_name, "events");
        assert!(s.columns.iter().any(|c| c.name == "event_type"));
        assert!(
            !s.partition_by.is_empty(),
            "event schema should have partition_by"
        );
    }

    #[test]
    fn test_suggest_generic_fallback() {
        let s = suggest_schema("some totally unrecognized description", None);
        assert_eq!(s.table_name, "data_table");
        assert!(s.columns.iter().any(|c| c.name == "id"));
    }

    #[test]
    fn test_create_sql_not_null_columns() {
        let s = suggest_schema("payment transactions", Some("banking"));
        // All required (NOT NULL) columns should appear in CREATE SQL.
        assert!(s.create_sql.contains("NOT NULL"));
    }
}
