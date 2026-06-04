pub mod compliance;
pub mod sample_data;
pub mod schemas;
pub mod udfs;

pub use compliance::{
    aml_screening_sql, gdpr_erasure_sql, psd2_consent_check_sql, sox_audit_trail_sql,
    suspicious_activity_report_sql,
};
pub use sample_data::generate_banking_dataset;
pub use schemas::{
    account_schema, aml_alert_schema, card_schema, customer_schema, loan_schema, transaction_schema,
};
pub use udfs::register_banking_udfs;
