use anyhow::Result;
use arrow::array::{Date32Array, Float64Array, Int32Array, StringArray, TimestampMillisecondArray};
use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use rand::Rng;
use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;

use super::schemas;

/// Merchant categories for realistic transaction data.
const MERCHANT_CATEGORIES: &[&str] = &[
    "grocery",
    "restaurant",
    "fuel",
    "clothing",
    "electronics",
    "healthcare",
    "travel",
    "entertainment",
    "utilities",
    "insurance",
    "education",
    "home_improvement",
];

const CHANNELS: &[&str] = &["online", "atm", "pos", "branch"];
const TXN_TYPES: &[&str] = &["debit", "credit", "transfer", "payment"];
const CURRENCIES: &[&str] = &["SEK", "EUR", "USD"];
const ACCOUNT_TYPES: &[&str] = &["checking", "savings", "loan", "credit"];
const CARD_TYPES: &[&str] = &["debit", "credit", "prepaid"];
const CARD_NETWORKS: &[&str] = &["visa", "mastercard", "amex"];
const SEGMENTS: &[&str] = &["retail", "private", "corporate"];
const COUNTRIES: &[&str] = &["SE", "NO", "DK", "FI", "DE", "GB"];
const AML_RULES: &[&str] = &[
    "large_cash_deposit",
    "rapid_transfers",
    "structuring",
    "round_amount_pattern",
    "high_risk_jurisdiction",
    "unusual_volume",
];
const AML_STATUSES: &[&str] = &["open", "investigating", "closed", "escalated"];
const LOAN_STATUSES: &[&str] = &["active", "paid_off", "defaulted", "restructured"];

const FIRST_NAMES: &[&str] = &[
    "Erik", "Anna", "Lars", "Maria", "Karl", "Eva", "Johan", "Sara", "Anders", "Karin", "Magnus",
    "Linda", "Olof", "Emma", "Sven", "Lena", "Fredrik", "Ingrid", "Gustaf", "Astrid",
];
const LAST_NAMES: &[&str] = &[
    "Andersson",
    "Johansson",
    "Karlsson",
    "Nilsson",
    "Eriksson",
    "Larsson",
    "Olsson",
    "Persson",
    "Svensson",
    "Gustafsson",
    "Lindberg",
    "Bergstrom",
    "Nordin",
    "Holm",
    "Lund",
    "Nystrom",
    "Sandberg",
    "Forsberg",
];

/// Helper: generate a random value in the given range using the Rng trait.
/// This avoids calling `.gen()` directly, which is a reserved keyword in Rust 2024.
fn random_range(rng: &mut impl Rng, range: std::ops::Range<u64>) -> u64 {
    rng.gen_range(range)
}

fn random_f64(rng: &mut impl Rng) -> f64 {
    rng.r#gen()
}

fn random_usize_range(rng: &mut impl Rng, range: std::ops::Range<usize>) -> usize {
    rng.gen_range(range)
}

fn random_i32_range(rng: &mut impl Rng, range: std::ops::Range<i32>) -> i32 {
    rng.gen_range(range)
}

fn random_i64_range(rng: &mut impl Rng, range: std::ops::Range<i64>) -> i64 {
    rng.gen_range(range)
}

fn random_f64_range(rng: &mut impl Rng, range: std::ops::Range<f64>) -> f64 {
    rng.gen_range(range)
}

fn random_u32_range(rng: &mut impl Rng, range: std::ops::Range<u32>) -> u32 {
    rng.gen_range(range)
}

fn pick<'a>(rng: &mut impl Rng, items: &'a [&str]) -> &'a str {
    items[random_usize_range(rng, 0..items.len())]
}

/// Generate a valid Swedish IBAN (SE + 2 check digits + 20 digits).
fn generate_swedish_iban(rng: &mut impl Rng) -> String {
    let bank_code: u32 = random_u32_range(rng, 100..999);
    let account_part: u64 = random_range(rng, 10_000_000_000_000_000..99_999_999_999_999_999);

    // Build the BBAN (20 digits)
    let bban = format!("{:03}{:017}", bank_code, account_part);

    // Compute IBAN check digits:
    // Rearrange: BBAN + "SE00", convert to numeric, mod 97
    let rearranged = format!("{}SE00", bban);
    let mut numeric = String::new();
    for ch in rearranged.chars() {
        if ch.is_ascii_digit() {
            numeric.push(ch);
        } else {
            let val = ch.to_ascii_uppercase() as u32 - 'A' as u32 + 10;
            numeric.push_str(&val.to_string());
        }
    }
    let remainder = numeric.chars().fold(0u64, |acc, ch| {
        (acc * 10 + ch.to_digit(10).unwrap() as u64) % 97
    });
    let check_digits = 98 - remainder;

    format!("SE{:02}{}", check_digits, bban)
}

/// Generate a Luhn-valid card number with the given prefix and length.
#[cfg(test)]
fn generate_card_number(rng: &mut impl Rng, prefix: &str, length: usize) -> String {
    let mut digits: Vec<u32> = prefix.chars().map(|c| c.to_digit(10).unwrap()).collect();
    // Fill random digits except the last (check digit)
    while digits.len() < length - 1 {
        digits.push(random_u32_range(rng, 0..10));
    }
    // Append a placeholder check digit
    digits.push(0);
    // Compute Luhn sum over all digits (with check digit = 0)
    // In Luhn, we double every second digit from the right.
    let n = digits.len();
    let mut sum = 0u32;
    for (idx, &d) in digits.iter().rev().enumerate() {
        if idx % 2 == 1 {
            // Double this digit
            let mut val = d * 2;
            if val > 9 {
                val -= 9;
            }
            sum += val;
        } else {
            sum += d;
        }
    }
    // Adjust the check digit so total mod 10 == 0
    let check = (10 - (sum % 10)) % 10;
    digits[n - 1] = check;
    digits.iter().map(|d| d.to_string()).collect()
}

/// Generate the complete banking dataset as Parquet files.
///
/// `scale` controls the number of records:
/// - customers: 100 * scale
/// - accounts: 200 * scale
/// - transactions: 5000 * scale
/// - cards: 150 * scale
/// - aml_alerts: 20 * scale
/// - loans: 50 * scale
pub fn generate_banking_dataset(path: &Path, scale: usize) -> Result<()> {
    fs::create_dir_all(path)?;
    let mut rng = rand::thread_rng();

    let num_customers = 100 * scale;
    let num_accounts = 200 * scale;
    let num_transactions = 5000 * scale;
    let num_cards = 150 * scale;
    let num_aml_alerts = 20 * scale;
    let num_loans = 50 * scale;

    // -- Customers --
    let customer_ids: Vec<String> = (0..num_customers)
        .map(|i| format!("CUST-{:06}", i))
        .collect();
    let names: Vec<String> = (0..num_customers)
        .map(|_| {
            format!(
                "{} {}",
                pick(&mut rng, FIRST_NAMES),
                pick(&mut rng, LAST_NAMES),
            )
        })
        .collect();
    // date_of_birth: days since epoch, range 1950-2000
    let dob: Vec<i32> = (0..num_customers)
        .map(|_| random_i32_range(&mut rng, -7305..10957))
        .collect();
    let kyc_statuses: Vec<String> = (0..num_customers)
        .map(|_| {
            let r = random_f64(&mut rng);
            if r < 0.8 {
                "verified"
            } else if r < 0.95 {
                "pending"
            } else {
                "expired"
            }
            .to_string()
        })
        .collect();
    let kyc_dates: Vec<i32> = (0..num_customers)
        .map(|_| random_i32_range(&mut rng, 18000..19500))
        .collect();
    let risk_scores: Vec<f64> = (0..num_customers)
        .map(|_| (random_f64(&mut rng) * 1000.0).round() / 10.0)
        .collect();
    let segments: Vec<String> = (0..num_customers)
        .map(|_| pick(&mut rng, SEGMENTS).to_string())
        .collect();
    let countries: Vec<String> = (0..num_customers)
        .map(|_| pick(&mut rng, COUNTRIES).to_string())
        .collect();
    let reg_dates: Vec<i32> = (0..num_customers)
        .map(|_| random_i32_range(&mut rng, 16000..19500))
        .collect();

    write_parquet(
        &path.join("customers.parquet"),
        &schemas::customer_schema(),
        vec![
            Arc::new(StringArray::from(customer_ids.clone())),
            Arc::new(StringArray::from(names)),
            Arc::new(Date32Array::from(dob)),
            Arc::new(StringArray::from(kyc_statuses)),
            Arc::new(Date32Array::from(kyc_dates)),
            Arc::new(Float64Array::from(risk_scores)),
            Arc::new(StringArray::from(segments)),
            Arc::new(StringArray::from(countries)),
            Arc::new(Date32Array::from(reg_dates)),
        ],
    )?;

    // -- Accounts --
    let account_ids: Vec<String> = (0..num_accounts).map(|i| format!("ACC-{:08}", i)).collect();
    let acct_customer_ids: Vec<String> = (0..num_accounts)
        .map(|_| customer_ids[random_usize_range(&mut rng, 0..num_customers)].clone())
        .collect();
    let acct_types: Vec<String> = (0..num_accounts)
        .map(|_| pick(&mut rng, ACCOUNT_TYPES).to_string())
        .collect();
    let acct_currencies: Vec<String> = (0..num_accounts)
        .map(|_| {
            let r = random_f64(&mut rng);
            if r < 0.7 {
                "SEK"
            } else if r < 0.9 {
                "EUR"
            } else {
                "USD"
            }
            .to_string()
        })
        .collect();
    let balances: Vec<f64> = (0..num_accounts)
        .map(|i| match acct_types[i].as_str() {
            "checking" => (random_f64(&mut rng) * 200000.0 * 100.0).round() / 100.0,
            "savings" => (random_f64(&mut rng) * 1000000.0 * 100.0).round() / 100.0,
            "loan" => -(random_f64(&mut rng) * 2000000.0 * 100.0).round() / 100.0,
            "credit" => -(random_f64(&mut rng) * 100000.0 * 100.0).round() / 100.0,
            _ => 0.0,
        })
        .collect();
    let opened_dates: Vec<i32> = (0..num_accounts)
        .map(|_| random_i32_range(&mut rng, 16000..19500))
        .collect();
    let acct_statuses: Vec<String> = (0..num_accounts)
        .map(|_| {
            let r = random_f64(&mut rng);
            if r < 0.9 {
                "active"
            } else if r < 0.95 {
                "frozen"
            } else {
                "closed"
            }
            .to_string()
        })
        .collect();
    let branch_ids: Vec<String> = (0..num_accounts)
        .map(|_| format!("BR-{:03}", random_i32_range(&mut rng, 1..50)))
        .collect();
    let ibans: Vec<String> = (0..num_accounts)
        .map(|_| generate_swedish_iban(&mut rng))
        .collect();

    write_parquet(
        &path.join("accounts.parquet"),
        &schemas::account_schema(),
        vec![
            Arc::new(StringArray::from(account_ids.clone())),
            Arc::new(StringArray::from(acct_customer_ids)),
            Arc::new(StringArray::from(acct_types)),
            Arc::new(StringArray::from(acct_currencies)),
            Arc::new(Float64Array::from(balances)),
            Arc::new(Date32Array::from(opened_dates)),
            Arc::new(StringArray::from(acct_statuses)),
            Arc::new(StringArray::from(branch_ids)),
            Arc::new(StringArray::from(ibans)),
        ],
    )?;

    // -- Transactions --
    let base_ts: i64 = 1_672_531_200_000; // 2023-01-01 00:00:00 UTC in ms
    let year_ms: i64 = 365 * 24 * 3600 * 1000;

    let txn_ids: Vec<String> = (0..num_transactions)
        .map(|i| format!("TXN-{:012}", i))
        .collect();
    let txn_from: Vec<String> = (0..num_transactions)
        .map(|_| account_ids[random_usize_range(&mut rng, 0..num_accounts)].clone())
        .collect();
    let txn_types: Vec<String> = (0..num_transactions)
        .map(|_| pick(&mut rng, TXN_TYPES).to_string())
        .collect();
    let txn_to: Vec<Option<String>> = (0..num_transactions)
        .map(|i| {
            if txn_types[i] == "transfer" || txn_types[i] == "payment" {
                Some(account_ids[random_usize_range(&mut rng, 0..num_accounts)].clone())
            } else {
                None
            }
        })
        .collect();
    let txn_amounts: Vec<f64> = (0..num_transactions)
        .map(|_| {
            let pattern = random_f64(&mut rng);
            if pattern < 0.02 {
                // Salary deposit
                (random_f64_range(&mut rng, 25000.0..80000.0) * 100.0).round() / 100.0
            } else if pattern < 0.05 {
                // Large transfer (potential AML flag)
                (random_f64_range(&mut rng, 50000.0..500000.0) * 100.0).round() / 100.0
            } else if pattern < 0.40 {
                // Daily card usage
                (random_f64_range(&mut rng, 10.0..500.0) * 100.0).round() / 100.0
            } else if pattern < 0.70 {
                // Medium purchases
                (random_f64_range(&mut rng, 500.0..5000.0) * 100.0).round() / 100.0
            } else {
                // Bill payments / subscriptions
                (random_f64_range(&mut rng, 100.0..3000.0) * 100.0).round() / 100.0
            }
        })
        .collect();
    let txn_currencies: Vec<String> = (0..num_transactions)
        .map(|_| pick(&mut rng, CURRENCIES).to_string())
        .collect();
    let txn_timestamps: Vec<i64> = {
        let mut ts: Vec<i64> = (0..num_transactions)
            .map(|_| base_ts + random_i64_range(&mut rng, 0..year_ms))
            .collect();
        ts.sort();
        ts
    };
    let merchant_cats: Vec<Option<String>> = (0..num_transactions)
        .map(|i| {
            if txn_types[i] == "debit" || txn_types[i] == "payment" {
                Some(pick(&mut rng, MERCHANT_CATEGORIES).to_string())
            } else {
                None
            }
        })
        .collect();
    let txn_statuses: Vec<String> = (0..num_transactions)
        .map(|_| {
            let r = random_f64(&mut rng);
            if r < 0.95 {
                "completed"
            } else if r < 0.98 {
                "pending"
            } else {
                "failed"
            }
            .to_string()
        })
        .collect();
    let txn_channels: Vec<String> = (0..num_transactions)
        .map(|_| pick(&mut rng, CHANNELS).to_string())
        .collect();

    write_parquet(
        &path.join("transactions.parquet"),
        &schemas::transaction_schema(),
        vec![
            Arc::new(StringArray::from(txn_ids)),
            Arc::new(StringArray::from(txn_from)),
            Arc::new(StringArray::from(txn_to)),
            Arc::new(Float64Array::from(txn_amounts)),
            Arc::new(StringArray::from(txn_currencies)),
            Arc::new(TimestampMillisecondArray::from(txn_timestamps).with_timezone("UTC")),
            Arc::new(StringArray::from(txn_types.clone())),
            Arc::new(StringArray::from(merchant_cats)),
            Arc::new(StringArray::from(txn_statuses)),
            Arc::new(StringArray::from(txn_channels)),
        ],
    )?;

    // -- Cards --
    let card_ids: Vec<String> = (0..num_cards).map(|i| format!("CARD-{:08}", i)).collect();
    let card_account_ids: Vec<String> = (0..num_cards)
        .map(|_| account_ids[random_usize_range(&mut rng, 0..num_accounts)].clone())
        .collect();
    let card_types_vec: Vec<String> = (0..num_cards)
        .map(|_| pick(&mut rng, CARD_TYPES).to_string())
        .collect();
    let card_networks_vec: Vec<String> = (0..num_cards)
        .map(|_| pick(&mut rng, CARD_NETWORKS).to_string())
        .collect();
    let expiry_dates: Vec<i32> = (0..num_cards)
        .map(|_| random_i32_range(&mut rng, 19700..21000))
        .collect();
    let card_statuses: Vec<String> = (0..num_cards)
        .map(|_| {
            let r = random_f64(&mut rng);
            if r < 0.85 {
                "active"
            } else if r < 0.92 {
                "blocked"
            } else {
                "expired"
            }
            .to_string()
        })
        .collect();
    let daily_limits: Vec<f64> = (0..num_cards)
        .map(|i| match card_types_vec[i].as_str() {
            "credit" => random_f64_range(&mut rng, 20000.0..100000.0),
            "debit" => random_f64_range(&mut rng, 5000.0..50000.0),
            "prepaid" => random_f64_range(&mut rng, 1000.0..10000.0),
            _ => 10000.0,
        })
        .collect();

    write_parquet(
        &path.join("cards.parquet"),
        &schemas::card_schema(),
        vec![
            Arc::new(StringArray::from(card_ids)),
            Arc::new(StringArray::from(card_account_ids)),
            Arc::new(StringArray::from(card_types_vec)),
            Arc::new(StringArray::from(card_networks_vec)),
            Arc::new(Date32Array::from(expiry_dates)),
            Arc::new(StringArray::from(card_statuses)),
            Arc::new(Float64Array::from(daily_limits)),
        ],
    )?;

    // -- AML Alerts --
    let alert_ids: Vec<String> = (0..num_aml_alerts)
        .map(|i| format!("AML-{:06}", i))
        .collect();
    let alert_customer_ids: Vec<String> = (0..num_aml_alerts)
        .map(|_| customer_ids[random_usize_range(&mut rng, 0..num_customers)].clone())
        .collect();
    let alert_timestamps: Vec<i64> = (0..num_aml_alerts)
        .map(|_| base_ts + random_i64_range(&mut rng, 0..year_ms))
        .collect();
    let alert_rules: Vec<String> = (0..num_aml_alerts)
        .map(|_| pick(&mut rng, AML_RULES).to_string())
        .collect();
    let alert_risk_scores: Vec<f64> = (0..num_aml_alerts)
        .map(|_| (random_f64_range(&mut rng, 50.0..100.0) * 10.0).round() / 10.0)
        .collect();
    let alert_amounts: Vec<Option<f64>> = (0..num_aml_alerts)
        .map(|_| Some((random_f64_range(&mut rng, 50000.0..1000000.0) * 100.0).round() / 100.0))
        .collect();
    let alert_descriptions: Vec<Option<String>> = (0..num_aml_alerts)
        .map(|i| {
            Some(match alert_rules[i].as_str() {
                "large_cash_deposit" => "Cash deposit exceeding threshold at branch".to_string(),
                "rapid_transfers" => "Multiple outgoing transfers within short window".to_string(),
                "structuring" => "Multiple deposits just below reporting threshold".to_string(),
                "round_amount_pattern" => {
                    "Series of round-amount transactions detected".to_string()
                }
                "high_risk_jurisdiction" => "Transfers to/from high-risk jurisdiction".to_string(),
                "unusual_volume" => "Transaction volume significantly above baseline".to_string(),
                _ => "Suspicious activity detected".to_string(),
            })
        })
        .collect();
    let alert_statuses: Vec<String> = (0..num_aml_alerts)
        .map(|_| pick(&mut rng, AML_STATUSES).to_string())
        .collect();

    write_parquet(
        &path.join("aml_alerts.parquet"),
        &schemas::aml_alert_schema(),
        vec![
            Arc::new(StringArray::from(alert_ids)),
            Arc::new(StringArray::from(alert_customer_ids)),
            Arc::new(TimestampMillisecondArray::from(alert_timestamps).with_timezone("UTC")),
            Arc::new(StringArray::from(alert_rules)),
            Arc::new(Float64Array::from(alert_risk_scores)),
            Arc::new(Float64Array::from(alert_amounts)),
            Arc::new(StringArray::from(alert_descriptions)),
            Arc::new(StringArray::from(alert_statuses)),
        ],
    )?;

    // -- Loans --
    let loan_ids: Vec<String> = (0..num_loans).map(|i| format!("LOAN-{:06}", i)).collect();
    let loan_customer_ids: Vec<String> = (0..num_loans)
        .map(|_| customer_ids[random_usize_range(&mut rng, 0..num_customers)].clone())
        .collect();
    let principals: Vec<f64> = (0..num_loans)
        .map(|_| (random_f64_range(&mut rng, 50000.0..5000000.0) * 100.0).round() / 100.0)
        .collect();
    let interest_rates: Vec<f64> = (0..num_loans)
        .map(|_| (random_f64_range(&mut rng, 0.01..0.15) * 10000.0).round() / 10000.0)
        .collect();
    let terms: Vec<i32> = (0..num_loans)
        .map(|_| {
            let options = [12, 24, 36, 60, 120, 180, 240, 360];
            options[random_usize_range(&mut rng, 0..options.len())]
        })
        .collect();
    let monthly_payments: Vec<f64> = (0..num_loans)
        .map(|i| {
            let p = principals[i];
            let r = interest_rates[i] / 12.0_f64;
            let n = terms[i] as f64;
            if r > 0.0_f64 {
                ((p * r * (1.0_f64 + r).powf(n)) / ((1.0_f64 + r).powf(n) - 1.0_f64) * 100.0_f64)
                    .round()
                    / 100.0_f64
            } else {
                (p / n * 100.0_f64).round() / 100.0_f64
            }
        })
        .collect();
    let outstanding: Vec<f64> = (0..num_loans)
        .map(|i| {
            let fraction = random_f64(&mut rng);
            (principals[i] * fraction * 100.0_f64).round() / 100.0_f64
        })
        .collect();
    let loan_statuses_vec: Vec<String> = (0..num_loans)
        .map(|_| pick(&mut rng, LOAN_STATUSES).to_string())
        .collect();
    let disbursement_dates: Vec<i32> = (0..num_loans)
        .map(|_| random_i32_range(&mut rng, 17000..19500))
        .collect();

    write_parquet(
        &path.join("loans.parquet"),
        &schemas::loan_schema(),
        vec![
            Arc::new(StringArray::from(loan_ids)),
            Arc::new(StringArray::from(loan_customer_ids)),
            Arc::new(Float64Array::from(principals)),
            Arc::new(Float64Array::from(interest_rates)),
            Arc::new(Int32Array::from(terms)),
            Arc::new(Float64Array::from(monthly_payments)),
            Arc::new(Float64Array::from(outstanding)),
            Arc::new(StringArray::from(loan_statuses_vec)),
            Arc::new(Date32Array::from(disbursement_dates)),
        ],
    )?;

    tracing::info!(
        path = %path.display(),
        customers = num_customers,
        accounts = num_accounts,
        transactions = num_transactions,
        cards = num_cards,
        aml_alerts = num_aml_alerts,
        loans = num_loans,
        "Banking dataset generated"
    );

    Ok(())
}

fn write_parquet(
    path: &Path,
    schema: &Schema,
    columns: Vec<Arc<dyn arrow::array::Array>>,
) -> Result<()> {
    let schema_ref = Arc::new(schema.clone());
    let batch = RecordBatch::try_new(schema_ref.clone(), columns)?;
    let file = File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema_ref, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::banking::udfs::validate_iban_value;

    #[test]
    fn test_generate_swedish_iban() {
        let mut rng = rand::thread_rng();
        for _ in 0..100 {
            let iban = generate_swedish_iban(&mut rng);
            assert!(
                iban.starts_with("SE"),
                "IBAN should start with SE: {}",
                iban
            );
            assert_eq!(iban.len(), 24, "Swedish IBAN should be 24 chars: {}", iban);
            assert!(
                validate_iban_value(&iban),
                "Generated IBAN should be valid: {}",
                iban
            );
        }
    }

    #[test]
    fn test_generate_card_number_luhn() {
        use crate::banking::udfs::luhn_check_value;
        let mut rng = rand::thread_rng();
        for _ in 0..50 {
            let card = generate_card_number(&mut rng, "4532", 16);
            assert_eq!(card.len(), 16);
            assert!(
                luhn_check_value(&card),
                "Generated card should pass Luhn: {}",
                card
            );
        }
    }

    #[test]
    fn test_generate_banking_dataset() {
        let dir = tempfile::tempdir().unwrap();
        generate_banking_dataset(dir.path(), 1).unwrap();
        assert!(dir.path().join("customers.parquet").exists());
        assert!(dir.path().join("accounts.parquet").exists());
        assert!(dir.path().join("transactions.parquet").exists());
        assert!(dir.path().join("cards.parquet").exists());
        assert!(dir.path().join("aml_alerts.parquet").exists());
        assert!(dir.path().join("loans.parquet").exists());
    }
}
