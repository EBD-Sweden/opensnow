//! Telecom regulatory and compliance SQL helpers.
//!
//! These functions generate SQL statements for common regulatory operations.
//! They return SQL strings that can be executed against a DataFusion context
//! (or any SQL-compatible engine) that has the telecom tables registered.

/// Generate SQL statements to purge all data for a subscriber (GDPR Art. 17).
///
/// Returns a vector of SQL DELETE/UPDATE statements that remove or anonymize
/// subscriber data across all telecom tables.
pub fn gdpr_delete_subscriber(msisdn: &str) -> Vec<String> {
    let sanitized = sanitize_msisdn(msisdn);
    vec![
        format!("DELETE FROM subscribers WHERE msisdn = '{sanitized}'"),
        format!("DELETE FROM cdr_voice WHERE caller = '{sanitized}' OR callee = '{sanitized}'"),
        format!("DELETE FROM cdr_sms WHERE sender = '{sanitized}' OR receiver = '{sanitized}'"),
        format!("DELETE FROM cdr_data WHERE msisdn = '{sanitized}'"),
        // Audit log entry (should be kept for accountability per GDPR Art. 5(2))
        format!(
            "INSERT INTO gdpr_audit_log (msisdn_hash, action, timestamp) \
             VALUES (SHA256('{sanitized}'), 'DELETE_ALL', NOW())"
        ),
    ]
}

/// Generate SQL for enforcing data retention policies.
///
/// Produces a DELETE statement that removes rows older than `retention_days`
/// from the specified table. Assumes the table has a timestamp-like column
/// (named `start_time`, `timestamp`, or `activation_date`).
pub fn retention_policy_sql(table: &str, retention_days: u32) -> String {
    let sanitized_table = sanitize_identifier(table);
    let ts_col = infer_timestamp_column(&sanitized_table);
    format!(
        "DELETE FROM {sanitized_table} \
         WHERE {ts_col} < NOW() - INTERVAL '{retention_days} days'"
    )
}

/// Generate SQL for lawful intercept (LEA) requests.
///
/// Produces queries that return all communication records for a given MSISDN
/// within the specified time range. This covers voice calls, SMS, and data
/// sessions as required by Swedish LEA (PTS) regulations.
pub fn lawful_intercept_query(msisdn: &str, start: &str, end: &str) -> Vec<String> {
    let sanitized = sanitize_msisdn(msisdn);
    let start_safe = sanitize_timestamp(start);
    let end_safe = sanitize_timestamp(end);

    vec![
        // Voice CDRs where subscriber is caller or callee
        format!(
            "SELECT 'VOICE' AS record_type, caller, callee, start_time, \
             duration_seconds, tower_id, cell_id, call_status, mcc_mnc \
             FROM cdr_voice \
             WHERE (caller = '{sanitized}' OR callee = '{sanitized}') \
             AND start_time BETWEEN '{start_safe}' AND '{end_safe}' \
             ORDER BY start_time"
        ),
        // SMS CDRs
        format!(
            "SELECT 'SMS' AS record_type, sender, receiver, timestamp, \
             message_type, tower_id, delivery_status \
             FROM cdr_sms \
             WHERE (sender = '{sanitized}' OR receiver = '{sanitized}') \
             AND timestamp BETWEEN '{start_safe}' AND '{end_safe}' \
             ORDER BY timestamp"
        ),
        // Data session CDRs
        format!(
            "SELECT 'DATA' AS record_type, msisdn, start_time, end_time, \
             bytes_up, bytes_down, apn, rat_type, tower_id \
             FROM cdr_data \
             WHERE msisdn = '{sanitized}' \
             AND start_time BETWEEN '{start_safe}' AND '{end_safe}' \
             ORDER BY start_time"
        ),
        // Subscriber profile snapshot
        format!(
            "SELECT msisdn, imsi, iccid, name, plan, status, \
             activation_date, region \
             FROM subscribers \
             WHERE msisdn = '{sanitized}'"
        ),
    ]
}

/// Validate that data in a table stays within the declared region.
///
/// Returns a SQL query that identifies rows where the tower's region
/// does not match the subscriber's declared region. Useful for ensuring
/// compliance with data residency requirements.
pub fn data_residency_check(table: &str) -> String {
    let sanitized_table = sanitize_identifier(table);

    match sanitized_table.as_str() {
        "cdr_voice" => "SELECT v.caller, v.tower_id, t.region AS tower_region, \
             s.region AS subscriber_region \
             FROM cdr_voice v \
             JOIN towers t ON v.tower_id = t.tower_id \
             JOIN subscribers s ON v.caller = s.msisdn \
             WHERE t.region != s.region"
            .to_string(),
        "cdr_sms" => "SELECT sm.sender, sm.tower_id, t.region AS tower_region, \
             s.region AS subscriber_region \
             FROM cdr_sms sm \
             JOIN towers t ON sm.tower_id = t.tower_id \
             JOIN subscribers s ON sm.sender = s.msisdn \
             WHERE t.region != s.region"
            .to_string(),
        "cdr_data" => "SELECT d.msisdn, d.tower_id, t.region AS tower_region, \
             s.region AS subscriber_region \
             FROM cdr_data d \
             JOIN towers t ON d.tower_id = t.tower_id \
             JOIN subscribers s ON d.msisdn = s.msisdn \
             WHERE t.region != s.region"
            .to_string(),
        _ => {
            format!(
                "SELECT * FROM {sanitized_table} WHERE 1=0 -- \
                 No residency check defined for table '{sanitized_table}'"
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Sanitization helpers (basic SQL injection prevention)
// ---------------------------------------------------------------------------

fn sanitize_msisdn(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect()
}

fn sanitize_identifier(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

fn sanitize_timestamp(input: &str) -> String {
    input
        .chars()
        .filter(|c| {
            c.is_ascii_digit() || *c == '-' || *c == ':' || *c == ' ' || *c == 'T' || *c == 'Z'
        })
        .collect()
}

fn infer_timestamp_column(table: &str) -> &'static str {
    match table {
        "cdr_voice" | "cdr_data" => "start_time",
        "cdr_sms" | "network_events" => "timestamp",
        "subscribers" => "activation_date",
        _ => "timestamp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gdpr_delete_subscriber() {
        let stmts = gdpr_delete_subscriber("+46701234567");
        assert_eq!(stmts.len(), 5);
        assert!(stmts[0].contains("DELETE FROM subscribers"));
        assert!(stmts[0].contains("+46701234567"));
        assert!(stmts[1].contains("cdr_voice"));
        assert!(stmts[2].contains("cdr_sms"));
        assert!(stmts[3].contains("cdr_data"));
        assert!(stmts[4].contains("gdpr_audit_log"));
    }

    #[test]
    fn test_gdpr_sanitizes_input() {
        let stmts = gdpr_delete_subscriber("+46701234567'; DROP TABLE subscribers;--");
        // Should strip everything except digits and +
        assert!(stmts[0].contains("+46701234567"));
        assert!(!stmts[0].contains("DROP"));
    }

    #[test]
    fn test_retention_policy_sql() {
        let sql = retention_policy_sql("cdr_voice", 90);
        assert!(sql.contains("DELETE FROM cdr_voice"));
        assert!(sql.contains("start_time"));
        assert!(sql.contains("90 days"));
    }

    #[test]
    fn test_retention_policy_sms() {
        let sql = retention_policy_sql("cdr_sms", 30);
        assert!(sql.contains("timestamp"));
        assert!(sql.contains("30 days"));
    }

    #[test]
    fn test_lawful_intercept_query() {
        let queries = lawful_intercept_query(
            "+46701234567",
            "2025-01-01T00:00:00Z",
            "2025-03-31T23:59:59Z",
        );
        assert_eq!(queries.len(), 4);
        assert!(queries[0].contains("cdr_voice"));
        assert!(queries[0].contains("VOICE"));
        assert!(queries[1].contains("cdr_sms"));
        assert!(queries[2].contains("cdr_data"));
        assert!(queries[3].contains("subscribers"));
        // All should contain the time range
        assert!(queries[0].contains("2025-01-01T00:00:00Z"));
        assert!(queries[0].contains("2025-03-31T23:59:59Z"));
    }

    #[test]
    fn test_data_residency_check_voice() {
        let sql = data_residency_check("cdr_voice");
        assert!(sql.contains("JOIN towers"));
        assert!(sql.contains("JOIN subscribers"));
        assert!(sql.contains("tower_region"));
        assert!(sql.contains("subscriber_region"));
    }

    #[test]
    fn test_data_residency_check_data() {
        let sql = data_residency_check("cdr_data");
        assert!(sql.contains("cdr_data d"));
        assert!(sql.contains("t.region != s.region"));
    }

    #[test]
    fn test_data_residency_check_unknown_table() {
        let sql = data_residency_check("unknown_table");
        assert!(sql.contains("WHERE 1=0"));
        assert!(sql.contains("No residency check defined"));
    }

    #[test]
    fn test_sanitize_identifier_strips_injection() {
        let result = sanitize_identifier("cdr_voice; DROP TABLE x");
        assert_eq!(result, "cdr_voiceDROPTABLEx");
    }
}
