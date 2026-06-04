use arrow::array::{Array, BooleanArray, Float64Array, StringArray};
use arrow::datatypes::DataType;
use datafusion::logical_expr::{ColumnarValue, ScalarUDF, Volatility};
use datafusion::prelude::*;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// IBAN validation (ISO 13616, mod-97 check)
// ---------------------------------------------------------------------------

/// Validate an IBAN using the MOD-97 algorithm (ISO 7064).
/// Returns true when the IBAN passes the check-digit verification.
pub fn validate_iban_value(iban: &str) -> bool {
    let cleaned: String = iban.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.len() < 5 {
        return false;
    }
    // All characters must be alphanumeric
    if !cleaned.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    // Move first 4 chars to end
    let rearranged = format!("{}{}", &cleaned[4..], &cleaned[..4]);
    // Convert letters to digits (A=10 .. Z=35)
    let mut numeric = String::new();
    for ch in rearranged.chars() {
        if ch.is_ascii_digit() {
            numeric.push(ch);
        } else {
            let val = ch.to_ascii_uppercase() as u32 - 'A' as u32 + 10;
            numeric.push_str(&val.to_string());
        }
    }
    // Compute mod 97 on the large number (iterative to avoid big-int)
    let remainder = numeric.chars().fold(0u64, |acc, ch| {
        (acc * 10 + ch.to_digit(10).unwrap() as u64) % 97
    });
    remainder == 1
}

/// Parse IBAN into components: country, check_digits, bank_code, account_number.
/// Returns (country, check_digits, bank_code, account_number).
pub fn parse_iban_value(iban: &str) -> (String, String, String, String) {
    let cleaned: String = iban.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.len() < 5 {
        return (String::new(), String::new(), String::new(), String::new());
    }
    let country = cleaned[0..2].to_string();
    let check_digits = cleaned[2..4].to_string();
    // Bank code is typically the next 3-4 chars; for Swedish IBANs it's 3 digits
    let bank_code_len = match country.as_str() {
        "SE" => 3,
        "DE" => 8,
        "GB" => 4,
        "FR" => 5,
        _ => 4,
    };
    let bank_end = (4 + bank_code_len).min(cleaned.len());
    let bank_code = cleaned[4..bank_end].to_string();
    let account_number = if bank_end < cleaned.len() {
        cleaned[bank_end..].to_string()
    } else {
        String::new()
    };
    (country, check_digits, bank_code, account_number)
}

// ---------------------------------------------------------------------------
// SWIFT / BIC validation
// ---------------------------------------------------------------------------

/// Validate a SWIFT/BIC code (8 or 11 characters).
pub fn validate_swift_value(bic: &str) -> bool {
    let len = bic.len();
    if len != 8 && len != 11 {
        return false;
    }
    // First 4: bank code (letters only)
    if !bic[0..4].chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    // Next 2: country code (letters only)
    if !bic[4..6].chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    // Next 2: location code (alphanumeric)
    if !bic[6..8].chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    // Optional 3: branch code (alphanumeric)
    if len == 11 && !bic[8..11].chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    true
}

/// Parse a SWIFT/BIC into components: bank, country, location, branch.
pub fn parse_swift_value(bic: &str) -> (String, String, String, String) {
    if !validate_swift_value(bic) {
        return (String::new(), String::new(), String::new(), String::new());
    }
    let bank = bic[0..4].to_string();
    let country = bic[4..6].to_string();
    let location = bic[6..8].to_string();
    let branch = if bic.len() == 11 {
        bic[8..11].to_string()
    } else {
        "XXX".to_string()
    };
    (bank, country, location, branch)
}

// ---------------------------------------------------------------------------
// Luhn algorithm for card number validation
// ---------------------------------------------------------------------------

/// Validate a number string using the Luhn algorithm.
pub fn luhn_check_value(number: &str) -> bool {
    let digits: Vec<u32> = number
        .chars()
        .filter(|c| c.is_ascii_digit())
        .map(|c| c.to_digit(10).unwrap())
        .collect();
    if digits.len() < 2 {
        return false;
    }
    let mut sum = 0u32;
    for (idx, &d) in digits.iter().rev().enumerate() {
        if idx % 2 == 1 {
            let mut val = d * 2;
            if val > 9 {
                val -= 9;
            }
            sum += val;
        } else {
            sum += d;
        }
    }
    sum.is_multiple_of(10)
}

// ---------------------------------------------------------------------------
// Financial helpers
// ---------------------------------------------------------------------------

/// Convert an amount from one currency to another using a given rate.
pub fn fx_convert_value(amount: f64, _from: &str, _to: &str, rate: f64) -> f64 {
    amount * rate
}

/// Compute compound interest: principal * (1 + rate)^periods.
pub fn compound_interest_value(principal: f64, rate: f64, periods: f64) -> f64 {
    principal * (1.0_f64 + rate).powf(periods)
}

/// Map a numeric risk score to a category string.
pub fn risk_score_category_value(score: f64) -> &'static str {
    match score {
        s if s < 25.0 => "low",
        s if s < 50.0 => "medium",
        s if s < 75.0 => "high",
        _ => "critical",
    }
}

// ---------------------------------------------------------------------------
// DataFusion UDF wrappers
// ---------------------------------------------------------------------------

fn make_validate_iban_udf() -> ScalarUDF {
    datafusion::logical_expr::create_udf(
        "validate_iban",
        vec![DataType::Utf8],
        DataType::Boolean,
        Volatility::Immutable,
        Arc::new(|args: &[ColumnarValue]| {
            let arr = match &args[0] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let strings = arr.as_any().downcast_ref::<StringArray>().unwrap();
            let result: BooleanArray = strings
                .iter()
                .map(|opt| opt.map(validate_iban_value))
                .collect();
            Ok(ColumnarValue::Array(Arc::new(result)))
        }),
    )
}

fn make_parse_iban_udf() -> ScalarUDF {
    // Returns "country|check|bank|account" as Utf8 pipe-delimited string.
    datafusion::logical_expr::create_udf(
        "parse_iban",
        vec![DataType::Utf8],
        DataType::Utf8,
        Volatility::Immutable,
        Arc::new(|args: &[ColumnarValue]| {
            let arr = match &args[0] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let strings = arr.as_any().downcast_ref::<StringArray>().unwrap();
            let result: StringArray = strings
                .iter()
                .map(|opt| {
                    opt.map(|v| {
                        let (country, check, bank, acct) = parse_iban_value(v);
                        format!("{}|{}|{}|{}", country, check, bank, acct)
                    })
                })
                .collect();
            Ok(ColumnarValue::Array(Arc::new(result)))
        }),
    )
}

fn make_validate_swift_udf() -> ScalarUDF {
    datafusion::logical_expr::create_udf(
        "validate_swift",
        vec![DataType::Utf8],
        DataType::Boolean,
        Volatility::Immutable,
        Arc::new(|args: &[ColumnarValue]| {
            let arr = match &args[0] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let strings = arr.as_any().downcast_ref::<StringArray>().unwrap();
            let result: BooleanArray = strings
                .iter()
                .map(|opt| opt.map(validate_swift_value))
                .collect();
            Ok(ColumnarValue::Array(Arc::new(result)))
        }),
    )
}

fn make_parse_swift_udf() -> ScalarUDF {
    datafusion::logical_expr::create_udf(
        "parse_swift",
        vec![DataType::Utf8],
        DataType::Utf8,
        Volatility::Immutable,
        Arc::new(|args: &[ColumnarValue]| {
            let arr = match &args[0] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let strings = arr.as_any().downcast_ref::<StringArray>().unwrap();
            let result: StringArray = strings
                .iter()
                .map(|opt| {
                    opt.map(|v| {
                        let (bank, country, loc, branch) = parse_swift_value(v);
                        format!("{}|{}|{}|{}", bank, country, loc, branch)
                    })
                })
                .collect();
            Ok(ColumnarValue::Array(Arc::new(result)))
        }),
    )
}

fn make_luhn_check_udf() -> ScalarUDF {
    datafusion::logical_expr::create_udf(
        "luhn_check",
        vec![DataType::Utf8],
        DataType::Boolean,
        Volatility::Immutable,
        Arc::new(|args: &[ColumnarValue]| {
            let arr = match &args[0] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let strings = arr.as_any().downcast_ref::<StringArray>().unwrap();
            let result: BooleanArray = strings
                .iter()
                .map(|opt| opt.map(luhn_check_value))
                .collect();
            Ok(ColumnarValue::Array(Arc::new(result)))
        }),
    )
}

fn make_fx_convert_udf() -> ScalarUDF {
    datafusion::logical_expr::create_udf(
        "fx_convert",
        vec![
            DataType::Float64,
            DataType::Utf8,
            DataType::Utf8,
            DataType::Float64,
        ],
        DataType::Float64,
        Volatility::Immutable,
        Arc::new(|args: &[ColumnarValue]| {
            let amounts = match &args[0] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let from_arr = match &args[1] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let to_arr = match &args[2] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let rates = match &args[3] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let amounts_arr = amounts.as_any().downcast_ref::<Float64Array>().unwrap();
            let from_currencies = from_arr.as_any().downcast_ref::<StringArray>().unwrap();
            let to_currencies = to_arr.as_any().downcast_ref::<StringArray>().unwrap();
            let rates_arr = rates.as_any().downcast_ref::<Float64Array>().unwrap();
            let len = amounts_arr.len();
            let result: Float64Array = (0..len)
                .map(|i| {
                    if amounts_arr.is_null(i) || rates_arr.is_null(i) {
                        None
                    } else {
                        let from_c = from_currencies.value(i);
                        let to_c = to_currencies.value(i);
                        Some(fx_convert_value(
                            amounts_arr.value(i),
                            from_c,
                            to_c,
                            rates_arr.value(i),
                        ))
                    }
                })
                .collect();
            Ok(ColumnarValue::Array(Arc::new(result)))
        }),
    )
}

fn make_compound_interest_udf() -> ScalarUDF {
    datafusion::logical_expr::create_udf(
        "compound_interest",
        vec![DataType::Float64, DataType::Float64, DataType::Float64],
        DataType::Float64,
        Volatility::Immutable,
        Arc::new(|args: &[ColumnarValue]| {
            let principals = match &args[0] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let rates = match &args[1] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let periods = match &args[2] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let principals_arr = principals.as_any().downcast_ref::<Float64Array>().unwrap();
            let rates_arr = rates.as_any().downcast_ref::<Float64Array>().unwrap();
            let periods_arr = periods.as_any().downcast_ref::<Float64Array>().unwrap();
            let len = principals_arr.len();
            let result: Float64Array = (0..len)
                .map(|i| {
                    if principals_arr.is_null(i) || rates_arr.is_null(i) || periods_arr.is_null(i) {
                        None
                    } else {
                        Some(compound_interest_value(
                            principals_arr.value(i),
                            rates_arr.value(i),
                            periods_arr.value(i),
                        ))
                    }
                })
                .collect();
            Ok(ColumnarValue::Array(Arc::new(result)))
        }),
    )
}

fn make_risk_score_category_udf() -> ScalarUDF {
    datafusion::logical_expr::create_udf(
        "risk_score_category",
        vec![DataType::Float64],
        DataType::Utf8,
        Volatility::Immutable,
        Arc::new(|args: &[ColumnarValue]| {
            let scores = match &args[0] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array()?,
            };
            let scores_arr = scores.as_any().downcast_ref::<Float64Array>().unwrap();
            let result: StringArray = scores_arr
                .iter()
                .map(|opt| opt.map(|v| risk_score_category_value(v).to_string()))
                .collect();
            Ok(ColumnarValue::Array(Arc::new(result)))
        }),
    )
}

/// Register all banking UDFs with the given DataFusion SessionContext.
pub fn register_banking_udfs(ctx: &SessionContext) {
    ctx.register_udf(make_validate_iban_udf());
    ctx.register_udf(make_parse_iban_udf());
    ctx.register_udf(make_validate_swift_udf());
    ctx.register_udf(make_parse_swift_udf());
    ctx.register_udf(make_luhn_check_udf());
    ctx.register_udf(make_fx_convert_udf());
    ctx.register_udf(make_compound_interest_udf());
    ctx.register_udf(make_risk_score_category_udf());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- IBAN tests --

    #[test]
    fn test_validate_iban_valid_gb() {
        assert!(validate_iban_value("GB29NWBK60161331926819"));
    }

    #[test]
    fn test_validate_iban_valid_de() {
        assert!(validate_iban_value("DE89370400440532013000"));
    }

    #[test]
    fn test_validate_iban_valid_se() {
        assert!(validate_iban_value("SE4550000000058398257466"));
    }

    #[test]
    fn test_validate_iban_invalid() {
        assert!(!validate_iban_value("GB29NWBK60161331926818")); // wrong check
        assert!(!validate_iban_value("INVALID"));
        assert!(!validate_iban_value(""));
        assert!(!validate_iban_value("AB12"));
    }

    #[test]
    fn test_validate_iban_with_spaces() {
        assert!(validate_iban_value("GB29 NWBK 6016 1331 9268 19"));
    }

    #[test]
    fn test_parse_iban_se() {
        let (country, check, bank, acct) = parse_iban_value("SE4550000000058398257466");
        assert_eq!(country, "SE");
        assert_eq!(check, "45");
        assert_eq!(bank, "500");
        assert_eq!(acct, "00000058398257466");
    }

    #[test]
    fn test_parse_iban_gb() {
        let (country, check, bank, acct) = parse_iban_value("GB29NWBK60161331926819");
        assert_eq!(country, "GB");
        assert_eq!(check, "29");
        assert_eq!(bank, "NWBK");
        assert_eq!(acct, "60161331926819");
    }

    // -- SWIFT/BIC tests --

    #[test]
    fn test_validate_swift_valid_8() {
        assert!(validate_swift_value("SWEDSESS"));
    }

    #[test]
    fn test_validate_swift_valid_11() {
        assert!(validate_swift_value("SWEDSESSXXX"));
    }

    #[test]
    fn test_validate_swift_invalid() {
        assert!(!validate_swift_value("SWED"));
        assert!(!validate_swift_value("12345678"));
        assert!(!validate_swift_value("SWED1234567")); // numbers in bank code
        assert!(!validate_swift_value(""));
    }

    #[test]
    fn test_parse_swift() {
        let (bank, country, loc, branch) = parse_swift_value("SWEDSESSXXX");
        assert_eq!(bank, "SWED");
        assert_eq!(country, "SE");
        assert_eq!(loc, "SS");
        assert_eq!(branch, "XXX");
    }

    #[test]
    fn test_parse_swift_8char() {
        let (bank, country, loc, branch) = parse_swift_value("SWEDSESS");
        assert_eq!(bank, "SWED");
        assert_eq!(country, "SE");
        assert_eq!(loc, "SS");
        assert_eq!(branch, "XXX"); // default
    }

    // -- Luhn tests --

    #[test]
    fn test_luhn_valid_visa() {
        assert!(luhn_check_value("4532015112830366"));
    }

    #[test]
    fn test_luhn_valid_mastercard() {
        assert!(luhn_check_value("5425233430109903"));
    }

    #[test]
    fn test_luhn_invalid() {
        assert!(!luhn_check_value("4532015112830367"));
        assert!(!luhn_check_value("1234567890123456"));
        assert!(!luhn_check_value("1"));
    }

    // -- Financial helper tests --

    #[test]
    fn test_fx_convert() {
        let result = fx_convert_value(100.0, "SEK", "EUR", 0.089);
        assert!((result - 8.9).abs() < 0.001);
    }

    #[test]
    fn test_compound_interest() {
        let result = compound_interest_value(1000.0, 0.05, 10.0);
        assert!((result - 1628.894).abs() < 0.01);
    }

    #[test]
    fn test_risk_score_category() {
        assert_eq!(risk_score_category_value(10.0), "low");
        assert_eq!(risk_score_category_value(24.9), "low");
        assert_eq!(risk_score_category_value(25.0), "medium");
        assert_eq!(risk_score_category_value(49.9), "medium");
        assert_eq!(risk_score_category_value(50.0), "high");
        assert_eq!(risk_score_category_value(74.9), "high");
        assert_eq!(risk_score_category_value(75.0), "critical");
        assert_eq!(risk_score_category_value(100.0), "critical");
    }

    // -- UDF registration test --

    #[tokio::test]
    async fn test_register_banking_udfs() {
        let ctx = SessionContext::new();
        register_banking_udfs(&ctx);
        let result = ctx
            .sql("SELECT validate_iban('GB29NWBK60161331926819') AS valid")
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        assert_eq!(result.len(), 1);
        let batch = &result[0];
        let col = batch
            .column(0)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        assert!(col.value(0));
    }
}
