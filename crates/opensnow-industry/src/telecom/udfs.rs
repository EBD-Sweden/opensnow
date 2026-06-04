use std::any::Any;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int32Array, StringArray, StructArray,
};
use arrow::datatypes::{DataType, Field, Fields};
use datafusion::common::Result as DFResult;
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature, TypeSignature,
    Volatility,
};
use datafusion::prelude::SessionContext;

// ---------------------------------------------------------------------------
// parse_msisdn
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ParseMsisdnUdf {
    signature: Signature,
}

impl ParseMsisdnUdf {
    fn new() -> Self {
        Self {
            signature: Signature::new(
                TypeSignature::Exact(vec![DataType::Utf8]),
                Volatility::Immutable,
            ),
        }
    }
}

impl ScalarUDFImpl for ParseMsisdnUdf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "parse_msisdn"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _args: &[DataType]) -> DFResult<DataType> {
        Ok(DataType::Struct(Fields::from(vec![
            Field::new("country_code", DataType::Utf8, true),
            Field::new("ndc", DataType::Utf8, true),
            Field::new("subscriber_number", DataType::Utf8, true),
        ])))
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> DFResult<ColumnarValue> {
        let args = &args.args;
        let phone_array = match &args[0] {
            ColumnarValue::Array(a) => a.clone(),
            ColumnarValue::Scalar(s) => s.to_array_of_size(1)?,
        };
        let phones = phone_array
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("expected Utf8 array");

        let len = phones.len();
        let mut cc_builder = Vec::with_capacity(len);
        let mut ndc_builder = Vec::with_capacity(len);
        let mut sn_builder = Vec::with_capacity(len);

        for i in 0..len {
            if phones.is_null(i) {
                cc_builder.push(None);
                ndc_builder.push(None);
                sn_builder.push(None);
                continue;
            }
            let raw = phones.value(i);
            let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();

            // Try to parse common structures: +CC NDC SN
            // Swedish numbers: 46 + 7x xxx xxxx (mobile) or 46 + 8 xxx xxxx (Stockholm)
            if digits.len() >= 10 {
                // Assume first 2 digits are country code for numbers >= 12 digits,
                // otherwise try common patterns
                let (cc, rest) = if digits.starts_with("46") && digits.len() >= 11 {
                    ("46".to_string(), &digits[2..])
                } else if digits.starts_with("1") && digits.len() == 11 {
                    ("1".to_string(), &digits[1..])
                } else if digits.starts_with("44") && digits.len() >= 12 {
                    ("44".to_string(), &digits[2..])
                } else if digits.len() > 10 {
                    // Generic: first 1-3 digits as CC
                    let cc_len = (digits.len() - 10).min(3);
                    (digits[..cc_len].to_string(), &digits[cc_len..])
                } else {
                    // 10 digits, no CC
                    (String::new(), digits.as_str())
                };

                // NDC is typically first 2-3 digits of the remaining number
                let ndc_len = if rest.len() > 7 { rest.len() - 7 } else { 0 };
                let ndc = &rest[..ndc_len];
                let sn = &rest[ndc_len..];

                cc_builder.push(Some(cc));
                ndc_builder.push(Some(ndc.to_string()));
                sn_builder.push(Some(sn.to_string()));
            } else {
                cc_builder.push(Some(String::new()));
                ndc_builder.push(Some(String::new()));
                sn_builder.push(Some(digits));
            }
        }

        let cc_array: ArrayRef = Arc::new(StringArray::from(cc_builder));
        let ndc_array: ArrayRef = Arc::new(StringArray::from(ndc_builder));
        let sn_array: ArrayRef = Arc::new(StringArray::from(sn_builder));

        let struct_array = StructArray::from(vec![
            (
                Arc::new(Field::new("country_code", DataType::Utf8, true)),
                cc_array,
            ),
            (Arc::new(Field::new("ndc", DataType::Utf8, true)), ndc_array),
            (
                Arc::new(Field::new("subscriber_number", DataType::Utf8, true)),
                sn_array,
            ),
        ]);

        Ok(ColumnarValue::Array(Arc::new(struct_array)))
    }
}

// ---------------------------------------------------------------------------
// validate_imsi
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ValidateImsiUdf {
    signature: Signature,
}

impl ValidateImsiUdf {
    fn new() -> Self {
        Self {
            signature: Signature::new(
                TypeSignature::Exact(vec![DataType::Utf8]),
                Volatility::Immutable,
            ),
        }
    }
}

impl ScalarUDFImpl for ValidateImsiUdf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "validate_imsi"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _args: &[DataType]) -> DFResult<DataType> {
        Ok(DataType::Boolean)
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> DFResult<ColumnarValue> {
        let args = &args.args;
        let arr = match &args[0] {
            ColumnarValue::Array(a) => a.clone(),
            ColumnarValue::Scalar(s) => s.to_array_of_size(1)?,
        };
        let strings = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let mut results = Vec::with_capacity(strings.len());

        for i in 0..strings.len() {
            if strings.is_null(i) {
                results.push(None);
                continue;
            }
            let imsi = strings.value(i);
            // IMSI: 15 decimal digits, starts with valid MCC (3 digits)
            let valid = imsi.len() == 15 && imsi.chars().all(|c| c.is_ascii_digit());
            results.push(Some(valid));
        }

        Ok(ColumnarValue::Array(Arc::new(BooleanArray::from(results))))
    }
}

// ---------------------------------------------------------------------------
// validate_imei  (Luhn check)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ValidateImeiUdf {
    signature: Signature,
}

impl ValidateImeiUdf {
    fn new() -> Self {
        Self {
            signature: Signature::new(
                TypeSignature::Exact(vec![DataType::Utf8]),
                Volatility::Immutable,
            ),
        }
    }
}

fn luhn_check(digits: &str) -> bool {
    if digits.len() != 15 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let sum: u32 = digits
        .chars()
        .rev()
        .enumerate()
        .map(|(i, c)| {
            let mut d = c.to_digit(10).unwrap();
            if i % 2 == 1 {
                d *= 2;
                if d > 9 {
                    d -= 9;
                }
            }
            d
        })
        .sum();
    sum.is_multiple_of(10)
}

impl ScalarUDFImpl for ValidateImeiUdf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "validate_imei"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _args: &[DataType]) -> DFResult<DataType> {
        Ok(DataType::Boolean)
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> DFResult<ColumnarValue> {
        let args = &args.args;
        let arr = match &args[0] {
            ColumnarValue::Array(a) => a.clone(),
            ColumnarValue::Scalar(s) => s.to_array_of_size(1)?,
        };
        let strings = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let mut results = Vec::with_capacity(strings.len());

        for i in 0..strings.len() {
            if strings.is_null(i) {
                results.push(None);
                continue;
            }
            results.push(Some(luhn_check(strings.value(i))));
        }

        Ok(ColumnarValue::Array(Arc::new(BooleanArray::from(results))))
    }
}

// ---------------------------------------------------------------------------
// format_e164
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct FormatE164Udf {
    signature: Signature,
}

impl FormatE164Udf {
    fn new() -> Self {
        Self {
            signature: Signature::new(
                TypeSignature::Exact(vec![DataType::Utf8, DataType::Utf8]),
                Volatility::Immutable,
            ),
        }
    }
}

impl ScalarUDFImpl for FormatE164Udf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "format_e164"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _args: &[DataType]) -> DFResult<DataType> {
        Ok(DataType::Utf8)
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> DFResult<ColumnarValue> {
        let args = &args.args;
        let phone_arr = match &args[0] {
            ColumnarValue::Array(a) => a.clone(),
            ColumnarValue::Scalar(s) => s.to_array_of_size(1)?,
        };
        let country_arr = match &args[1] {
            ColumnarValue::Array(a) => a.clone(),
            ColumnarValue::Scalar(s) => s.to_array_of_size(phone_arr.len())?,
        };
        let phones = phone_arr.as_any().downcast_ref::<StringArray>().unwrap();
        let countries = country_arr.as_any().downcast_ref::<StringArray>().unwrap();
        let mut results: Vec<Option<String>> = Vec::with_capacity(phones.len());

        for i in 0..phones.len() {
            if phones.is_null(i) || countries.is_null(i) {
                results.push(None);
                continue;
            }
            let phone = phones.value(i);
            let country = countries.value(i).to_uppercase();
            let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();

            let cc = match country.as_str() {
                "SE" => "46",
                "US" | "CA" => "1",
                "GB" => "44",
                "DE" => "49",
                "NO" => "47",
                "DK" => "45",
                "FI" => "358",
                "FR" => "33",
                _ => "",
            };

            if cc.is_empty() || digits.starts_with(cc) {
                results.push(Some(format!("+{digits}")));
            } else if let Some(stripped) = digits.strip_prefix('0') {
                results.push(Some(format!("+{cc}{stripped}")));
            } else {
                results.push(Some(format!("+{cc}{digits}")));
            }
        }

        Ok(ColumnarValue::Array(Arc::new(StringArray::from(results))))
    }
}

// ---------------------------------------------------------------------------
// tower_distance  (Haversine)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct TowerDistanceUdf {
    signature: Signature,
}

impl TowerDistanceUdf {
    fn new() -> Self {
        Self {
            signature: Signature::new(
                TypeSignature::Exact(vec![
                    DataType::Float64,
                    DataType::Float64,
                    DataType::Float64,
                    DataType::Float64,
                ]),
                Volatility::Immutable,
            ),
        }
    }
}

fn haversine(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0; // Earth radius in km
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let lat1_r = lat1.to_radians();
    let lat2_r = lat2.to_radians();

    let a = (d_lat / 2.0).sin().powi(2) + lat1_r.cos() * lat2_r.cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    R * c
}

impl ScalarUDFImpl for TowerDistanceUdf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "tower_distance"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _args: &[DataType]) -> DFResult<DataType> {
        Ok(DataType::Float64)
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> DFResult<ColumnarValue> {
        let args = &args.args;
        let to_f64_array = |idx: usize| -> Arc<dyn arrow::array::Array> {
            match &args[idx] {
                ColumnarValue::Array(a) => a.clone(),
                ColumnarValue::Scalar(s) => s.to_array_of_size(1).unwrap(),
            }
        };

        let lat1_arr = to_f64_array(0);
        let lon1_arr = to_f64_array(1);
        let lat2_arr = to_f64_array(2);
        let lon2_arr = to_f64_array(3);

        let lat1 = lat1_arr.as_any().downcast_ref::<Float64Array>().unwrap();
        let lon1 = lon1_arr.as_any().downcast_ref::<Float64Array>().unwrap();
        let lat2 = lat2_arr.as_any().downcast_ref::<Float64Array>().unwrap();
        let lon2 = lon2_arr.as_any().downcast_ref::<Float64Array>().unwrap();

        let len = lat1.len();
        let mut results = Vec::with_capacity(len);

        for i in 0..len {
            if lat1.is_null(i) || lon1.is_null(i) || lat2.is_null(i) || lon2.is_null(i) {
                results.push(None);
            } else {
                results.push(Some(haversine(
                    lat1.value(i),
                    lon1.value(i),
                    lat2.value(i),
                    lon2.value(i),
                )));
            }
        }

        Ok(ColumnarValue::Array(Arc::new(Float64Array::from(results))))
    }
}

// ---------------------------------------------------------------------------
// call_cost
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct CallCostUdf {
    signature: Signature,
}

impl CallCostUdf {
    fn new() -> Self {
        Self {
            signature: Signature::new(
                TypeSignature::Exact(vec![DataType::Int32, DataType::Float64]),
                Volatility::Immutable,
            ),
        }
    }
}

impl ScalarUDFImpl for CallCostUdf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "call_cost"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _args: &[DataType]) -> DFResult<DataType> {
        Ok(DataType::Float64)
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> DFResult<ColumnarValue> {
        let args = &args.args;
        let dur_arr = match &args[0] {
            ColumnarValue::Array(a) => a.clone(),
            ColumnarValue::Scalar(s) => s.to_array_of_size(1)?,
        };
        let rate_arr = match &args[1] {
            ColumnarValue::Array(a) => a.clone(),
            ColumnarValue::Scalar(s) => s.to_array_of_size(dur_arr.len())?,
        };

        let durations = dur_arr.as_any().downcast_ref::<Int32Array>().unwrap();
        let rates = rate_arr.as_any().downcast_ref::<Float64Array>().unwrap();
        let mut results = Vec::with_capacity(durations.len());

        for i in 0..durations.len() {
            if durations.is_null(i) || rates.is_null(i) {
                results.push(None);
            } else {
                let minutes = (durations.value(i) as f64) / 60.0;
                let cost = (minutes * rates.value(i) * 100.0).ceil() / 100.0;
                results.push(Some(cost));
            }
        }

        Ok(ColumnarValue::Array(Arc::new(Float64Array::from(results))))
    }
}

// ---------------------------------------------------------------------------
// data_usage_gb
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct DataUsageGbUdf {
    signature: Signature,
}

impl DataUsageGbUdf {
    fn new() -> Self {
        Self {
            signature: Signature::new(
                TypeSignature::Exact(vec![DataType::Int64]),
                Volatility::Immutable,
            ),
        }
    }
}

impl ScalarUDFImpl for DataUsageGbUdf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "data_usage_gb"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _args: &[DataType]) -> DFResult<DataType> {
        Ok(DataType::Float64)
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> DFResult<ColumnarValue> {
        let args = &args.args;
        let bytes_arr = match &args[0] {
            ColumnarValue::Array(a) => a.clone(),
            ColumnarValue::Scalar(s) => s.to_array_of_size(1)?,
        };

        let bytes = bytes_arr
            .as_any()
            .downcast_ref::<arrow::array::Int64Array>()
            .unwrap();
        let mut results = Vec::with_capacity(bytes.len());

        for i in 0..bytes.len() {
            if bytes.is_null(i) {
                results.push(None);
            } else {
                let gb = ((bytes.value(i) as f64 / 1_073_741_824.0) * 100.0).round() / 100.0;
                results.push(Some(gb));
            }
        }

        Ok(ColumnarValue::Array(Arc::new(Float64Array::from(results))))
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register all telecom UDFs with the given DataFusion `SessionContext`.
pub fn register_telecom_udfs(ctx: &SessionContext) {
    ctx.register_udf(ScalarUDF::from(ParseMsisdnUdf::new()));
    ctx.register_udf(ScalarUDF::from(ValidateImsiUdf::new()));
    ctx.register_udf(ScalarUDF::from(ValidateImeiUdf::new()));
    ctx.register_udf(ScalarUDF::from(FormatE164Udf::new()));
    ctx.register_udf(ScalarUDF::from(TowerDistanceUdf::new()));
    ctx.register_udf(ScalarUDF::from(CallCostUdf::new()));
    ctx.register_udf(ScalarUDF::from(DataUsageGbUdf::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_luhn_check_valid() {
        // Known valid IMEI (check digit computed via Luhn)
        assert!(luhn_check("490154203237518"));
    }

    #[test]
    fn test_luhn_check_invalid() {
        assert!(!luhn_check("490154203237519"));
        assert!(!luhn_check("12345")); // too short
        assert!(!luhn_check("abcdefghijklmno")); // non-digits
    }

    #[test]
    fn test_haversine_known_distance() {
        // Stockholm (59.3293, 18.0686) to Gothenburg (57.7089, 11.9746)
        let d = haversine(59.3293, 18.0686, 57.7089, 11.9746);
        assert!((d - 398.0).abs() < 10.0, "Expected ~398 km, got {d}");
    }

    #[test]
    fn test_haversine_same_point() {
        let d = haversine(59.0, 18.0, 59.0, 18.0);
        assert!(d.abs() < 0.001);
    }

    #[tokio::test]
    async fn test_register_udfs() {
        let ctx = SessionContext::new();
        register_telecom_udfs(&ctx);

        // validate_imsi via SQL
        let df = ctx
            .sql("SELECT validate_imsi('240010123456789')")
            .await
            .unwrap();
        let batches = df.collect().await.unwrap();
        assert_eq!(batches.len(), 1);
        let col = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        assert!(col.value(0));
    }

    #[tokio::test]
    async fn test_validate_imei_sql() {
        let ctx = SessionContext::new();
        register_telecom_udfs(&ctx);

        let df = ctx
            .sql("SELECT validate_imei('490154203237518')")
            .await
            .unwrap();
        let batches = df.collect().await.unwrap();
        let col = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        assert!(col.value(0));
    }

    #[tokio::test]
    async fn test_format_e164_sql() {
        let ctx = SessionContext::new();
        register_telecom_udfs(&ctx);

        let df = ctx
            .sql("SELECT format_e164('0701234567', 'SE')")
            .await
            .unwrap();
        let batches = df.collect().await.unwrap();
        let col = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(col.value(0), "+46701234567");
    }

    #[tokio::test]
    async fn test_data_usage_gb_sql() {
        let ctx = SessionContext::new();
        register_telecom_udfs(&ctx);

        // 1 GiB in bytes = 1073741824
        let df = ctx
            .sql("SELECT data_usage_gb(CAST(1073741824 AS BIGINT))")
            .await
            .unwrap();
        let batches = df.collect().await.unwrap();
        let col = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert!((col.value(0) - 1.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_tower_distance_sql() {
        let ctx = SessionContext::new();
        register_telecom_udfs(&ctx);

        let df = ctx
            .sql("SELECT tower_distance(59.3293, 18.0686, 57.7089, 11.9746)")
            .await
            .unwrap();
        let batches = df.collect().await.unwrap();
        let col = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let dist = col.value(0);
        assert!((dist - 398.0).abs() < 10.0, "Expected ~398 km, got {dist}");
    }

    #[tokio::test]
    async fn test_call_cost_sql() {
        let ctx = SessionContext::new();
        register_telecom_udfs(&ctx);

        // 120 seconds at 1.50 per minute = 3.00
        let df = ctx
            .sql("SELECT call_cost(CAST(120 AS INT), 1.50)")
            .await
            .unwrap();
        let batches = df.collect().await.unwrap();
        let col = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert!((col.value(0) - 3.0).abs() < 0.01);
    }
}
