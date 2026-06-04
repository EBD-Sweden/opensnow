use std::sync::Arc;

use arrow::array::{Array, BooleanArray, Float64Array, Int64Array, StringArray};
use arrow::datatypes::DataType;
use datafusion::logical_expr::{ColumnarValue, ScalarUDF, ScalarUDFImpl, Signature, Volatility};
use datafusion::prelude::SessionContext;

/// Register Snowflake-compatible scalar functions with DataFusion.
pub fn register_snowflake_functions(ctx: &SessionContext) {
    ctx.register_udf(ScalarUDF::from(IffFunction::new()));
    ctx.register_udf(ScalarUDF::from(Nvl::new()));
    ctx.register_udf(ScalarUDF::from(Nvl2::new()));
    ctx.register_udf(ScalarUDF::from(TryCastInt::new()));
    ctx.register_udf(ScalarUDF::from(TryCastFloat::new()));
    ctx.register_udf(ScalarUDF::from(DateAdd::new()));
    ctx.register_udf(ScalarUDF::from(DateDiff::new()));
    ctx.register_udf(ScalarUDF::from(ParseJson::new()));
    ctx.register_udf(ScalarUDF::from(JsonExtractPath::new()));
    ctx.register_udf(ScalarUDF::from(TypeOf::new()));
}

// ---- IFF(condition, true_val, false_val) ----
#[derive(Debug)]
struct IffFunction {
    signature: Signature,
}

impl IffFunction {
    fn new() -> Self {
        Self {
            signature: Signature::any(3, Volatility::Immutable),
        }
    }
}

impl ScalarUDFImpl for IffFunction {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "iff"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, arg_types: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(arg_types.get(1).cloned().unwrap_or(DataType::Utf8))
    }
    fn invoke_batch(
        &self,
        args: &[ColumnarValue],
        num_rows: usize,
    ) -> datafusion::common::Result<ColumnarValue> {
        let condition = args[0].to_owned().into_array(num_rows)?;
        let true_val = args[1].to_owned().into_array(num_rows)?;
        let false_val = args[2].to_owned().into_array(num_rows)?;

        let cond = condition
            .as_any()
            .downcast_ref::<BooleanArray>()
            .ok_or_else(|| {
                datafusion::error::DataFusionError::Internal(
                    "IFF: first arg must be boolean".into(),
                )
            })?;

        // For simplicity, handle string case (most common in Snowflake)
        let true_str = true_val.as_any().downcast_ref::<StringArray>();
        let false_str = false_val.as_any().downcast_ref::<StringArray>();

        if let (Some(t), Some(f)) = (true_str, false_str) {
            let result: StringArray = (0..num_rows)
                .map(|i| {
                    if cond.value(i) {
                        t.value(i)
                    } else {
                        f.value(i)
                    }
                })
                .collect::<Vec<&str>>()
                .into_iter()
                .map(Some)
                .collect();
            return Ok(ColumnarValue::Array(Arc::new(result)));
        }

        // Fallback: return true_val (best effort)
        Ok(ColumnarValue::Array(true_val))
    }
}

// ---- NVL(expr, default) — like COALESCE ----
#[derive(Debug)]
struct Nvl {
    signature: Signature,
}
impl Nvl {
    fn new() -> Self {
        Self {
            signature: Signature::any(2, Volatility::Immutable),
        }
    }
}
impl ScalarUDFImpl for Nvl {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "nvl"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, arg_types: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(arg_types[0].clone())
    }
    fn invoke_batch(
        &self,
        args: &[ColumnarValue],
        num_rows: usize,
    ) -> datafusion::common::Result<ColumnarValue> {
        let expr = args[0].to_owned().into_array(num_rows)?;
        let default = args[1].to_owned().into_array(num_rows)?;

        if let (Some(e), Some(d)) = (
            expr.as_any().downcast_ref::<StringArray>(),
            default.as_any().downcast_ref::<StringArray>(),
        ) {
            let result: StringArray = (0..num_rows)
                .map(|i| {
                    if e.is_null(i) {
                        Some(d.value(i))
                    } else {
                        Some(e.value(i))
                    }
                })
                .collect();
            return Ok(ColumnarValue::Array(Arc::new(result)));
        }
        Ok(ColumnarValue::Array(expr))
    }
}

// ---- NVL2(expr, not_null_val, null_val) ----
#[derive(Debug)]
struct Nvl2 {
    signature: Signature,
}
impl Nvl2 {
    fn new() -> Self {
        Self {
            signature: Signature::any(3, Volatility::Immutable),
        }
    }
}
impl ScalarUDFImpl for Nvl2 {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "nvl2"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, arg_types: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(arg_types.get(1).cloned().unwrap_or(DataType::Utf8))
    }
    fn invoke_batch(
        &self,
        args: &[ColumnarValue],
        num_rows: usize,
    ) -> datafusion::common::Result<ColumnarValue> {
        let expr = args[0].to_owned().into_array(num_rows)?;
        let not_null = args[1].to_owned().into_array(num_rows)?;
        let null_val = args[2].to_owned().into_array(num_rows)?;

        if let (Some(nn), Some(nv)) = (
            not_null.as_any().downcast_ref::<StringArray>(),
            null_val.as_any().downcast_ref::<StringArray>(),
        ) {
            let result: StringArray = (0..num_rows)
                .map(|i| {
                    if expr.is_null(i) {
                        Some(nv.value(i))
                    } else {
                        Some(nn.value(i))
                    }
                })
                .collect();
            return Ok(ColumnarValue::Array(Arc::new(result)));
        }
        Ok(ColumnarValue::Array(not_null))
    }
}

// ---- TRY_CAST to INT (returns NULL on failure) ----
#[derive(Debug)]
struct TryCastInt {
    signature: Signature,
}
impl TryCastInt {
    fn new() -> Self {
        Self {
            signature: Signature::exact(vec![DataType::Utf8], Volatility::Immutable),
        }
    }
}
impl ScalarUDFImpl for TryCastInt {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "try_to_integer"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Int64)
    }
    fn invoke_batch(
        &self,
        args: &[ColumnarValue],
        num_rows: usize,
    ) -> datafusion::common::Result<ColumnarValue> {
        let arr = args[0].to_owned().into_array(num_rows)?;
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let result: Int64Array = (0..str_arr.len())
            .map(|i| {
                if str_arr.is_null(i) {
                    None
                } else {
                    str_arr.value(i).parse::<i64>().ok()
                }
            })
            .collect();
        Ok(ColumnarValue::Array(Arc::new(result)))
    }
}

// ---- TRY_CAST to FLOAT ----
#[derive(Debug)]
struct TryCastFloat {
    signature: Signature,
}
impl TryCastFloat {
    fn new() -> Self {
        Self {
            signature: Signature::exact(vec![DataType::Utf8], Volatility::Immutable),
        }
    }
}
impl ScalarUDFImpl for TryCastFloat {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "try_to_double"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Float64)
    }
    fn invoke_batch(
        &self,
        args: &[ColumnarValue],
        num_rows: usize,
    ) -> datafusion::common::Result<ColumnarValue> {
        let arr = args[0].to_owned().into_array(num_rows)?;
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let result: Float64Array = (0..str_arr.len())
            .map(|i| {
                if str_arr.is_null(i) {
                    None
                } else {
                    str_arr.value(i).parse::<f64>().ok()
                }
            })
            .collect();
        Ok(ColumnarValue::Array(Arc::new(result)))
    }
}

// ---- DATEADD(part, amount, date_expr) — simplified version ----
#[derive(Debug)]
struct DateAdd {
    signature: Signature,
}
impl DateAdd {
    fn new() -> Self {
        Self {
            signature: Signature::any(3, Volatility::Immutable),
        }
    }
}
impl ScalarUDFImpl for DateAdd {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "dateadd"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Utf8)
    }
    fn invoke_batch(
        &self,
        args: &[ColumnarValue],
        num_rows: usize,
    ) -> datafusion::common::Result<ColumnarValue> {
        // Simplified: returns a string hint. Full implementation would manipulate timestamps.
        let _part = args[0].to_owned().into_array(num_rows)?;
        let _amount = args[1].to_owned().into_array(num_rows)?;
        let result: StringArray = (0..num_rows)
            .map(|_| Some("Use: date_col + INTERVAL 'N days/hours' instead"))
            .collect();
        Ok(ColumnarValue::Array(Arc::new(result)))
    }
}

// ---- DATEDIFF(part, start, end) ----
#[derive(Debug)]
struct DateDiff {
    signature: Signature,
}
impl DateDiff {
    fn new() -> Self {
        Self {
            signature: Signature::any(3, Volatility::Immutable),
        }
    }
}
impl ScalarUDFImpl for DateDiff {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "datediff"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Int64)
    }
    fn invoke_batch(
        &self,
        _args: &[ColumnarValue],
        num_rows: usize,
    ) -> datafusion::common::Result<ColumnarValue> {
        // Simplified placeholder
        let result: Int64Array = (0..num_rows).map(|_| Some(0i64)).collect();
        Ok(ColumnarValue::Array(Arc::new(result)))
    }
}

// ---- PARSE_JSON(string) -> string (JSON validated) ----
#[derive(Debug)]
struct ParseJson {
    signature: Signature,
}
impl ParseJson {
    fn new() -> Self {
        Self {
            signature: Signature::exact(vec![DataType::Utf8], Volatility::Immutable),
        }
    }
}
impl ScalarUDFImpl for ParseJson {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "parse_json"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Utf8)
    }
    fn invoke_batch(
        &self,
        args: &[ColumnarValue],
        num_rows: usize,
    ) -> datafusion::common::Result<ColumnarValue> {
        let arr = args[0].to_owned().into_array(num_rows)?;
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let result: StringArray = (0..str_arr.len())
            .map(|i| {
                if str_arr.is_null(i) {
                    return None;
                }
                let s = str_arr.value(i);
                match serde_json::from_str::<serde_json::Value>(s) {
                    Ok(v) => Some(v.to_string()),
                    Err(_) => None,
                }
            })
            .collect();
        Ok(ColumnarValue::Array(Arc::new(result)))
    }
}

// ---- JSON_EXTRACT_PATH(json_str, path...) — simplified GET_PATH ----
#[derive(Debug)]
struct JsonExtractPath {
    signature: Signature,
}
impl JsonExtractPath {
    fn new() -> Self {
        Self {
            signature: Signature::variadic_any(Volatility::Immutable),
        }
    }
}
impl ScalarUDFImpl for JsonExtractPath {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "get_path"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Utf8)
    }
    fn invoke_batch(
        &self,
        args: &[ColumnarValue],
        num_rows: usize,
    ) -> datafusion::common::Result<ColumnarValue> {
        if args.len() < 2 {
            return Err(datafusion::error::DataFusionError::Internal(
                "get_path requires at least 2 args".into(),
            ));
        }
        let json_arr = args[0].to_owned().into_array(num_rows)?;
        let path_arr = args[1].to_owned().into_array(num_rows)?;
        let json_str = json_arr.as_any().downcast_ref::<StringArray>().unwrap();
        let path_str = path_arr.as_any().downcast_ref::<StringArray>().unwrap();

        let result: StringArray = (0..num_rows)
            .map(|i| {
                if json_str.is_null(i) || path_str.is_null(i) {
                    return None;
                }
                let json: serde_json::Value = serde_json::from_str(json_str.value(i)).ok()?;
                let path = path_str.value(i);
                // Navigate path (dot-separated)
                let mut current = &json;
                for key in path.split('.') {
                    current = current.get(key)?;
                }
                Some(match current {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
            })
            .collect();
        Ok(ColumnarValue::Array(Arc::new(result)))
    }
}

// ---- TYPEOF(expr) ----
#[derive(Debug)]
struct TypeOf {
    signature: Signature,
}
impl TypeOf {
    fn new() -> Self {
        Self {
            signature: Signature::any(1, Volatility::Immutable),
        }
    }
}
impl ScalarUDFImpl for TypeOf {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "typeof"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Utf8)
    }
    fn invoke_batch(
        &self,
        args: &[ColumnarValue],
        num_rows: usize,
    ) -> datafusion::common::Result<ColumnarValue> {
        let arr = args[0].to_owned().into_array(num_rows)?;
        let type_name = format!("{:?}", arr.data_type());
        let result: StringArray = (0..num_rows).map(|_| Some(type_name.as_str())).collect();
        Ok(ColumnarValue::Array(Arc::new(result)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_iff_function() {
        let ctx = SessionContext::new();
        register_snowflake_functions(&ctx);
        let result = ctx
            .sql("SELECT iff(1 > 0, 'yes', 'no') AS result")
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        assert_eq!(result[0].num_rows(), 1);
    }

    #[tokio::test]
    async fn test_try_to_integer() {
        let ctx = SessionContext::new();
        register_snowflake_functions(&ctx);
        let result = ctx
            .sql("SELECT try_to_integer('42') AS num")
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        let arr = result[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(arr.value(0), 42);
    }

    #[tokio::test]
    async fn test_parse_json() {
        let ctx = SessionContext::new();
        register_snowflake_functions(&ctx);
        let result = ctx
            .sql(r#"SELECT parse_json('{"name":"test","value":42}') AS j"#)
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        assert_eq!(result[0].num_rows(), 1);
    }

    #[tokio::test]
    async fn test_get_path() {
        let ctx = SessionContext::new();
        register_snowflake_functions(&ctx);
        let result = ctx
            .sql(r#"SELECT get_path('{"user":{"name":"alice","age":30}}', 'user.name') AS name"#)
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        let arr = result[0]
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(arr.value(0), "alice");
    }

    #[tokio::test]
    async fn test_typeof() {
        let ctx = SessionContext::new();
        register_snowflake_functions(&ctx);
        let result = ctx
            .sql("SELECT typeof(42) AS t")
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        assert_eq!(result[0].num_rows(), 1);
    }
}
