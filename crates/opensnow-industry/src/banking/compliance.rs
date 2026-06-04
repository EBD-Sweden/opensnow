/// Generate SQL for PSD2 consent validation for a given customer.
///
/// Checks that the customer has active, non-expired consent records
/// and returns consent details including scope and expiration.
pub fn psd2_consent_check_sql(customer_id: &str) -> String {
    format!(
        r#"SELECT
    c.customer_id,
    c.name,
    consent.consent_id,
    consent.scope,
    consent.granted_date,
    consent.expiry_date,
    consent.status,
    CASE
        WHEN consent.status = 'active'
             AND consent.expiry_date >= CURRENT_DATE
        THEN 'VALID'
        WHEN consent.status = 'active'
             AND consent.expiry_date < CURRENT_DATE
        THEN 'EXPIRED'
        WHEN consent.status = 'revoked'
        THEN 'REVOKED'
        ELSE 'MISSING'
    END AS consent_validity
FROM customers c
LEFT JOIN psd2_consents consent
    ON c.customer_id = consent.customer_id
WHERE c.customer_id = '{customer_id}'
ORDER BY consent.granted_date DESC"#
    )
}

/// Generate SQL for AML transaction screening.
///
/// Identifies transactions above the given threshold and aggregates
/// suspicious activity patterns for a given customer.
pub fn aml_screening_sql(customer_id: &str, threshold: f64) -> String {
    format!(
        r#"WITH customer_txns AS (
    SELECT
        t.txn_id,
        t.account_from,
        t.account_to,
        t.amount,
        t.currency,
        t.timestamp,
        t.txn_type,
        t.channel,
        t.status
    FROM transactions t
    JOIN accounts a ON t.account_from = a.account_id
    WHERE a.customer_id = '{customer_id}'
      AND t.status = 'completed'
),
large_txns AS (
    SELECT *
    FROM customer_txns
    WHERE amount >= {threshold}
),
rapid_transfers AS (
    SELECT
        ct1.txn_id AS txn_id_1,
        ct2.txn_id AS txn_id_2,
        ct1.amount AS amount_1,
        ct2.amount AS amount_2,
        ct1.timestamp AS ts_1,
        ct2.timestamp AS ts_2
    FROM customer_txns ct1
    JOIN customer_txns ct2
        ON ct1.txn_id < ct2.txn_id
        AND ct2.timestamp BETWEEN ct1.timestamp
            AND ct1.timestamp + INTERVAL '1' HOUR
    WHERE ct1.txn_type = 'transfer'
      AND ct2.txn_type = 'transfer'
),
daily_aggregates AS (
    SELECT
        CAST(timestamp AS DATE) AS txn_date,
        COUNT(*) AS txn_count,
        SUM(amount) AS daily_total,
        COUNT(DISTINCT channel) AS channels_used
    FROM customer_txns
    GROUP BY CAST(timestamp AS DATE)
)
SELECT
    'large_transactions' AS check_type,
    COUNT(*) AS hit_count,
    SUM(amount) AS total_amount
FROM large_txns
UNION ALL
SELECT
    'rapid_transfers' AS check_type,
    COUNT(*) AS hit_count,
    SUM(amount_1 + amount_2) AS total_amount
FROM rapid_transfers
UNION ALL
SELECT
    'high_volume_days' AS check_type,
    COUNT(*) AS hit_count,
    SUM(daily_total) AS total_amount
FROM daily_aggregates
WHERE txn_count > 20 OR daily_total > {threshold} * 5"#
    )
}

/// Generate SQL for SOX audit trail on a specific table within a date range.
///
/// Creates a comprehensive audit trail showing all changes, access events,
/// and data lineage for compliance with Sarbanes-Oxley requirements.
pub fn sox_audit_trail_sql(table: &str, start: &str, end: &str) -> String {
    format!(
        r#"SELECT
    audit.event_id,
    audit.table_name,
    audit.operation,
    audit.user_id,
    audit.timestamp,
    audit.old_values,
    audit.new_values,
    audit.ip_address,
    audit.session_id,
    u.name AS user_name,
    u.role AS user_role
FROM audit_log audit
LEFT JOIN users u ON audit.user_id = u.user_id
WHERE audit.table_name = '{table}'
  AND audit.timestamp >= TIMESTAMP '{start}'
  AND audit.timestamp <= TIMESTAMP '{end}'
ORDER BY audit.timestamp ASC"#
    )
}

/// Generate SQL for GDPR erasure (right to be forgotten) for a customer.
///
/// Produces a cascade of DELETE statements across all banking tables
/// to fully remove a customer's personal data. Returns the SQL as a
/// series of statements that should be executed in order within a transaction.
pub fn gdpr_erasure_sql(customer_id: &str) -> String {
    format!(
        r#"-- GDPR Right to Erasure - Customer Data Deletion
-- Customer: {customer_id}
-- Execute within a single transaction

-- Step 1: Delete AML alerts (references customer)
DELETE FROM aml_alerts
WHERE customer_id = '{customer_id}';

-- Step 2: Delete cards (references accounts owned by customer)
DELETE FROM cards
WHERE account_id IN (
    SELECT account_id FROM accounts WHERE customer_id = '{customer_id}'
);

-- Step 3: Delete transactions (references accounts owned by customer)
DELETE FROM transactions
WHERE account_from IN (
    SELECT account_id FROM accounts WHERE customer_id = '{customer_id}'
)
OR account_to IN (
    SELECT account_id FROM accounts WHERE customer_id = '{customer_id}'
);

-- Step 4: Delete loans (references customer)
DELETE FROM loans
WHERE customer_id = '{customer_id}';

-- Step 5: Delete PSD2 consents (references customer)
DELETE FROM psd2_consents
WHERE customer_id = '{customer_id}';

-- Step 6: Delete accounts (references customer)
DELETE FROM accounts
WHERE customer_id = '{customer_id}';

-- Step 7: Delete the customer record itself
DELETE FROM customers
WHERE customer_id = '{customer_id}';

-- Step 8: Log the erasure event for compliance audit
INSERT INTO gdpr_erasure_log (customer_id, erasure_timestamp, requested_by)
VALUES ('{customer_id}', CURRENT_TIMESTAMP, 'gdpr_automation');"#
    )
}

/// Generate SQL for Suspicious Activity Report (SAR) filing.
///
/// Identifies customers with transactions exceeding the threshold amount
/// within the specified time window, aggregating evidence for regulatory filing.
pub fn suspicious_activity_report_sql(threshold_amount: f64, window_hours: u32) -> String {
    format!(
        r#"WITH suspicious_txns AS (
    SELECT
        t.txn_id,
        t.account_from,
        t.account_to,
        t.amount,
        t.currency,
        t.timestamp,
        t.txn_type,
        t.channel,
        a.customer_id,
        c.name AS customer_name,
        c.risk_score,
        c.segment,
        c.country
    FROM transactions t
    JOIN accounts a ON t.account_from = a.account_id
    JOIN customers c ON a.customer_id = c.customer_id
    WHERE t.amount >= {threshold_amount}
      AND t.timestamp >= CURRENT_TIMESTAMP - INTERVAL '{window_hours}' HOUR
      AND t.status = 'completed'
),
customer_activity AS (
    SELECT
        customer_id,
        customer_name,
        risk_score,
        segment,
        country,
        COUNT(*) AS suspicious_txn_count,
        SUM(amount) AS total_suspicious_amount,
        MIN(timestamp) AS first_suspicious_txn,
        MAX(timestamp) AS last_suspicious_txn,
        COUNT(DISTINCT channel) AS channels_used,
        COUNT(DISTINCT account_from) AS accounts_used,
        ARRAY_AGG(DISTINCT txn_type) AS txn_types_used
    FROM suspicious_txns
    GROUP BY customer_id, customer_name, risk_score, segment, country
),
existing_alerts AS (
    SELECT
        customer_id,
        COUNT(*) AS prior_alert_count,
        MAX(timestamp) AS last_alert_date
    FROM aml_alerts
    GROUP BY customer_id
)
SELECT
    ca.customer_id,
    ca.customer_name,
    ca.risk_score,
    ca.segment,
    ca.country,
    ca.suspicious_txn_count,
    ca.total_suspicious_amount,
    ca.first_suspicious_txn,
    ca.last_suspicious_txn,
    ca.channels_used,
    ca.accounts_used,
    ca.txn_types_used,
    COALESCE(ea.prior_alert_count, 0) AS prior_alerts,
    ea.last_alert_date,
    CASE
        WHEN ca.risk_score >= 75 AND ca.total_suspicious_amount >= {threshold_amount} * 5
        THEN 'CRITICAL - Immediate filing required'
        WHEN ca.risk_score >= 50 OR ca.suspicious_txn_count >= 5
        THEN 'HIGH - File within 24 hours'
        WHEN ca.suspicious_txn_count >= 3
        THEN 'MEDIUM - Review and escalate'
        ELSE 'LOW - Monitor'
    END AS sar_priority
FROM customer_activity ca
LEFT JOIN existing_alerts ea ON ca.customer_id = ea.customer_id
ORDER BY ca.risk_score DESC, ca.total_suspicious_amount DESC"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_psd2_consent_check_sql() {
        let sql = psd2_consent_check_sql("CUST-000001");
        assert!(sql.contains("CUST-000001"));
        assert!(sql.contains("psd2_consents"));
        assert!(sql.contains("consent_validity"));
        assert!(sql.contains("CURRENT_DATE"));
    }

    #[test]
    fn test_aml_screening_sql() {
        let sql = aml_screening_sql("CUST-000042", 15000.0);
        assert!(sql.contains("CUST-000042"));
        assert!(sql.contains("15000"));
        assert!(sql.contains("large_transactions"));
        assert!(sql.contains("rapid_transfers"));
        assert!(sql.contains("high_volume_days"));
    }

    #[test]
    fn test_sox_audit_trail_sql() {
        let sql = sox_audit_trail_sql("transactions", "2023-01-01 00:00:00", "2023-12-31 23:59:59");
        assert!(sql.contains("transactions"));
        assert!(sql.contains("2023-01-01"));
        assert!(sql.contains("2023-12-31"));
        assert!(sql.contains("audit_log"));
    }

    #[test]
    fn test_gdpr_erasure_sql() {
        let sql = gdpr_erasure_sql("CUST-000099");
        assert!(sql.contains("CUST-000099"));
        // Should delete from all related tables
        assert!(sql.contains("DELETE FROM aml_alerts"));
        assert!(sql.contains("DELETE FROM cards"));
        assert!(sql.contains("DELETE FROM transactions"));
        assert!(sql.contains("DELETE FROM loans"));
        assert!(sql.contains("DELETE FROM accounts"));
        assert!(sql.contains("DELETE FROM customers"));
        assert!(sql.contains("gdpr_erasure_log"));
    }

    #[test]
    fn test_suspicious_activity_report_sql() {
        let sql = suspicious_activity_report_sql(50000.0, 24);
        assert!(sql.contains("50000"));
        assert!(sql.contains("24"));
        assert!(sql.contains("sar_priority"));
        assert!(sql.contains("CRITICAL"));
        assert!(sql.contains("prior_alerts"));
    }
}
