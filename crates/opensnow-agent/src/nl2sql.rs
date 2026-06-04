use serde::Serialize;

/// Natural language to SQL query hints.
/// Provides context that AI agents can use to write better SQL.

#[derive(Debug, Serialize)]
pub struct QueryContext {
    pub tables: Vec<TableContext>,
    pub relationships: Vec<Relationship>,
    pub common_patterns: Vec<QueryPattern>,
}

#[derive(Debug, Serialize)]
pub struct TableContext {
    pub name: String,
    pub industry: String,
    pub description: String,
    pub key_columns: Vec<String>,
    pub common_filters: Vec<String>,
    pub common_aggregations: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Relationship {
    pub from_table: String,
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
    pub join_type: String,
}

#[derive(Debug, Serialize)]
pub struct QueryPattern {
    pub intent: String,
    pub sql_template: String,
    pub description: String,
}

/// Build query context for the agent based on available tables.
pub fn build_query_context(table_names: &[String]) -> QueryContext {
    let mut tables = Vec::new();
    let mut relationships = Vec::new();
    let mut patterns = Vec::new();

    for name in table_names {
        match name.as_str() {
            "cdrs" => {
                tables.push(TableContext {
                    name: "cdrs".into(),
                    industry: "telecom".into(),
                    description:
                        "Call Detail Records — one row per voice call, SMS, or data session".into(),
                    key_columns: vec![
                        "cdr_id".into(),
                        "caller".into(),
                        "callee".into(),
                        "tower_id".into(),
                    ],
                    common_filters: vec!["call_type".into(), "timestamp".into(), "tower_id".into()],
                    common_aggregations: vec![
                        "COUNT(*)".into(),
                        "AVG(duration_seconds)".into(),
                        "SUM(duration_seconds)".into(),
                    ],
                });
                relationships.push(Relationship {
                    from_table: "cdrs".into(),
                    from_column: "tower_id".into(),
                    to_table: "towers".into(),
                    to_column: "tower_id".into(),
                    join_type: "INNER JOIN".into(),
                });
            }
            "subscribers" => {
                tables.push(TableContext {
                    name: "subscribers".into(),
                    industry: "telecom".into(),
                    description: "Mobile subscribers — one row per SIM/phone number".into(),
                    key_columns: vec!["subscriber_id".into(), "phone".into()],
                    common_filters: vec!["region".into(), "plan".into()],
                    common_aggregations: vec!["COUNT(*)".into(), "AVG(monthly_arpu)".into()],
                });
            }
            "towers" => {
                tables.push(TableContext {
                    name: "towers".into(),
                    industry: "telecom".into(),
                    description: "Cell towers / base stations".into(),
                    key_columns: vec!["tower_id".into()],
                    common_filters: vec!["region".into()],
                    common_aggregations: vec!["COUNT(*)".into()],
                });
            }
            "transactions" => {
                tables.push(TableContext {
                    name: "transactions".into(),
                    industry: "banking".into(),
                    description: "Financial transactions — debits, credits, transfers, payments"
                        .into(),
                    key_columns: vec!["txn_id".into(), "account_from".into(), "account_to".into()],
                    common_filters: vec![
                        "txn_type".into(),
                        "timestamp".into(),
                        "channel".into(),
                        "status".into(),
                    ],
                    common_aggregations: vec![
                        "SUM(amount)".into(),
                        "COUNT(*)".into(),
                        "AVG(amount)".into(),
                    ],
                });
                relationships.push(Relationship {
                    from_table: "transactions".into(),
                    from_column: "account_from".into(),
                    to_table: "accounts".into(),
                    to_column: "iban".into(),
                    join_type: "LEFT JOIN".into(),
                });
            }
            "accounts" => {
                tables.push(TableContext {
                    name: "accounts".into(),
                    industry: "banking".into(),
                    description: "Bank accounts — checking, savings, loan, credit".into(),
                    key_columns: vec!["account_id".into(), "customer_id".into(), "iban".into()],
                    common_filters: vec!["account_type".into(), "status".into()],
                    common_aggregations: vec!["SUM(balance)".into(), "COUNT(*)".into()],
                });
                relationships.push(Relationship {
                    from_table: "accounts".into(),
                    from_column: "customer_id".into(),
                    to_table: "customers".into(),
                    to_column: "customer_id".into(),
                    join_type: "INNER JOIN".into(),
                });
            }
            "customers" => {
                tables.push(TableContext {
                    name: "customers".into(),
                    industry: "banking".into(),
                    description: "Bank customers with KYC status and risk scoring".into(),
                    key_columns: vec!["customer_id".into()],
                    common_filters: vec!["segment".into(), "country".into(), "kyc_status".into()],
                    common_aggregations: vec!["COUNT(*)".into(), "AVG(risk_score)".into()],
                });
            }
            _ => {
                tables.push(TableContext {
                    name: name.clone(),
                    industry: "general".into(),
                    description: format!("Table: {}", name),
                    key_columns: vec![],
                    common_filters: vec![],
                    common_aggregations: vec!["COUNT(*)".into()],
                });
            }
        }
    }

    // Common cross-table patterns
    if table_names.iter().any(|n| n == "cdrs") && table_names.iter().any(|n| n == "towers") {
        patterns.push(QueryPattern {
            intent: "Call volume by region".into(),
            sql_template: "SELECT t.region, COUNT(*) AS calls FROM cdrs c JOIN towers t ON c.tower_id = t.tower_id GROUP BY t.region ORDER BY calls DESC".into(),
            description: "Join CDRs to towers to aggregate by geographic region".into(),
        });
    }
    if table_names.iter().any(|n| n == "transactions")
        && table_names.iter().any(|n| n == "customers")
    {
        patterns.push(QueryPattern {
            intent: "Transaction volume by customer segment".into(),
            sql_template: "SELECT c.segment, COUNT(*) AS txns, ROUND(SUM(t.amount),2) AS total FROM transactions t JOIN accounts a ON t.account_from = a.iban JOIN customers c ON a.customer_id = c.customer_id GROUP BY c.segment".into(),
            description: "Three-way join: transactions -> accounts -> customers for segment analysis".into(),
        });
    }

    QueryContext {
        tables,
        relationships,
        common_patterns: patterns,
    }
}
