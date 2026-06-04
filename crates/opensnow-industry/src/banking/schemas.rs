use arrow::datatypes::{DataType, Field, Schema};

/// Schema for financial transactions between accounts.
pub fn transaction_schema() -> Schema {
    Schema::new(vec![
        Field::new("txn_id", DataType::Utf8, false),
        Field::new("account_from", DataType::Utf8, false),
        Field::new("account_to", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, false),
        Field::new("currency", DataType::Utf8, false),
        Field::new(
            "timestamp",
            DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new("txn_type", DataType::Utf8, false), // debit, credit, transfer, payment
        Field::new("merchant_category", DataType::Utf8, true),
        Field::new("status", DataType::Utf8, false),
        Field::new("channel", DataType::Utf8, false), // online, atm, pos, branch
    ])
}

/// Schema for bank accounts.
pub fn account_schema() -> Schema {
    Schema::new(vec![
        Field::new("account_id", DataType::Utf8, false),
        Field::new("customer_id", DataType::Utf8, false),
        Field::new("account_type", DataType::Utf8, false), // checking, savings, loan, credit
        Field::new("currency", DataType::Utf8, false),
        Field::new("balance", DataType::Float64, false),
        Field::new("opened_date", DataType::Date32, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("branch_id", DataType::Utf8, true),
        Field::new("iban", DataType::Utf8, true),
    ])
}

/// Schema for bank customers.
pub fn customer_schema() -> Schema {
    Schema::new(vec![
        Field::new("customer_id", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("date_of_birth", DataType::Date32, true),
        Field::new("kyc_status", DataType::Utf8, false),
        Field::new("kyc_date", DataType::Date32, true),
        Field::new("risk_score", DataType::Float64, false),
        Field::new("segment", DataType::Utf8, false), // retail, private, corporate
        Field::new("country", DataType::Utf8, false),
        Field::new("registration_date", DataType::Date32, false),
    ])
}

/// Schema for payment cards.
pub fn card_schema() -> Schema {
    Schema::new(vec![
        Field::new("card_id", DataType::Utf8, false),
        Field::new("account_id", DataType::Utf8, false),
        Field::new("card_type", DataType::Utf8, false), // debit, credit, prepaid
        Field::new("card_network", DataType::Utf8, false), // visa, mastercard, amex
        Field::new("expiry_date", DataType::Date32, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("daily_limit", DataType::Float64, false),
    ])
}

/// Schema for anti-money-laundering alerts.
pub fn aml_alert_schema() -> Schema {
    Schema::new(vec![
        Field::new("alert_id", DataType::Utf8, false),
        Field::new("customer_id", DataType::Utf8, false),
        Field::new(
            "timestamp",
            DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new("rule_triggered", DataType::Utf8, false),
        Field::new("risk_score", DataType::Float64, false),
        Field::new("amount", DataType::Float64, true),
        Field::new("description", DataType::Utf8, true),
        Field::new("status", DataType::Utf8, false), // open, investigating, closed, escalated
    ])
}

/// Schema for loans.
pub fn loan_schema() -> Schema {
    Schema::new(vec![
        Field::new("loan_id", DataType::Utf8, false),
        Field::new("customer_id", DataType::Utf8, false),
        Field::new("principal", DataType::Float64, false),
        Field::new("interest_rate", DataType::Float64, false),
        Field::new("term_months", DataType::Int32, false),
        Field::new("monthly_payment", DataType::Float64, false),
        Field::new("outstanding_balance", DataType::Float64, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("disbursement_date", DataType::Date32, false),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_schema_fields() {
        let schema = transaction_schema();
        assert_eq!(schema.fields().len(), 10);
        assert!(schema.field_with_name("txn_id").is_ok());
        assert!(schema.field_with_name("amount").is_ok());
        assert!(schema.field_with_name("channel").is_ok());
    }

    #[test]
    fn test_account_schema_fields() {
        let schema = account_schema();
        assert_eq!(schema.fields().len(), 9);
        assert!(schema.field_with_name("iban").is_ok());
    }

    #[test]
    fn test_customer_schema_fields() {
        let schema = customer_schema();
        assert_eq!(schema.fields().len(), 9);
        assert!(schema.field_with_name("kyc_status").is_ok());
        assert!(schema.field_with_name("segment").is_ok());
    }

    #[test]
    fn test_card_schema_fields() {
        let schema = card_schema();
        assert_eq!(schema.fields().len(), 7);
        assert!(schema.field_with_name("card_network").is_ok());
    }

    #[test]
    fn test_aml_alert_schema_fields() {
        let schema = aml_alert_schema();
        assert_eq!(schema.fields().len(), 8);
        assert!(schema.field_with_name("rule_triggered").is_ok());
    }

    #[test]
    fn test_loan_schema_fields() {
        let schema = loan_schema();
        assert_eq!(schema.fields().len(), 9);
        assert!(schema.field_with_name("outstanding_balance").is_ok());
    }
}
