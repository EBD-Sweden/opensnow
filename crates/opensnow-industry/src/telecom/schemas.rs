use arrow::datatypes::{DataType, Field, Schema};

/// CDR schema for voice calls.
pub fn cdr_voice_schema() -> Schema {
    Schema::new(vec![
        Field::new("caller", DataType::Utf8, false),
        Field::new("callee", DataType::Utf8, false),
        Field::new(
            "start_time",
            DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("duration_seconds", DataType::Int32, false),
        Field::new("tower_id", DataType::Utf8, false),
        Field::new("cell_id", DataType::Utf8, false),
        Field::new("call_status", DataType::Utf8, false),
        Field::new("codec", DataType::Utf8, true),
        Field::new("mcc_mnc", DataType::Utf8, false),
    ])
}

/// CDR schema for SMS messages.
pub fn cdr_sms_schema() -> Schema {
    Schema::new(vec![
        Field::new("sender", DataType::Utf8, false),
        Field::new("receiver", DataType::Utf8, false),
        Field::new(
            "timestamp",
            DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("message_type", DataType::Utf8, false),
        Field::new("tower_id", DataType::Utf8, false),
        Field::new("delivery_status", DataType::Utf8, false),
    ])
}

/// CDR schema for data sessions.
pub fn cdr_data_schema() -> Schema {
    Schema::new(vec![
        Field::new("msisdn", DataType::Utf8, false),
        Field::new(
            "start_time",
            DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new(
            "end_time",
            DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("bytes_up", DataType::Int64, false),
        Field::new("bytes_down", DataType::Int64, false),
        Field::new("apn", DataType::Utf8, false),
        Field::new("rat_type", DataType::Utf8, false),
        Field::new("tower_id", DataType::Utf8, false),
    ])
}

/// Schema for subscriber master data.
pub fn subscriber_schema() -> Schema {
    Schema::new(vec![
        Field::new("msisdn", DataType::Utf8, false),
        Field::new("imsi", DataType::Utf8, false),
        Field::new("iccid", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("plan", DataType::Utf8, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("activation_date", DataType::Date32, false),
        Field::new("region", DataType::Utf8, false),
        Field::new("arpu", DataType::Float64, false),
    ])
}

/// Schema for cell towers / base stations.
pub fn tower_schema() -> Schema {
    Schema::new(vec![
        Field::new("tower_id", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("lat", DataType::Float64, false),
        Field::new("lon", DataType::Float64, false),
        Field::new("height_m", DataType::Float64, false),
        Field::new("technology", DataType::Utf8, false),
        Field::new("region", DataType::Utf8, false),
        Field::new("capacity", DataType::Int32, false),
        Field::new("operator", DataType::Utf8, false),
    ])
}

/// Schema for network events / alarms.
pub fn network_event_schema() -> Schema {
    Schema::new(vec![
        Field::new("event_id", DataType::Utf8, false),
        Field::new(
            "timestamp",
            DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("tower_id", DataType::Utf8, false),
        Field::new("event_type", DataType::Utf8, false),
        Field::new("severity", DataType::Utf8, false),
        Field::new("description", DataType::Utf8, true),
        Field::new("affected_subscribers", DataType::Int32, false),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cdr_voice_schema_fields() {
        let schema = cdr_voice_schema();
        assert_eq!(schema.fields().len(), 9);
        assert!(schema.field_with_name("caller").is_ok());
        assert!(schema.field_with_name("callee").is_ok());
        assert!(schema.field_with_name("start_time").is_ok());
        assert!(schema.field_with_name("duration_seconds").is_ok());
        assert!(schema.field_with_name("tower_id").is_ok());
        assert!(schema.field_with_name("cell_id").is_ok());
        assert!(schema.field_with_name("call_status").is_ok());
        assert!(schema.field_with_name("codec").is_ok());
        assert!(schema.field_with_name("mcc_mnc").is_ok());
    }

    #[test]
    fn test_cdr_sms_schema_fields() {
        let schema = cdr_sms_schema();
        assert_eq!(schema.fields().len(), 6);
        assert!(schema.field_with_name("sender").is_ok());
        assert!(schema.field_with_name("delivery_status").is_ok());
    }

    #[test]
    fn test_cdr_data_schema_fields() {
        let schema = cdr_data_schema();
        assert_eq!(schema.fields().len(), 8);
        assert!(schema.field_with_name("msisdn").is_ok());
        assert!(schema.field_with_name("bytes_up").is_ok());
        assert!(schema.field_with_name("bytes_down").is_ok());
        assert!(schema.field_with_name("apn").is_ok());
        assert!(schema.field_with_name("rat_type").is_ok());
    }

    #[test]
    fn test_subscriber_schema_fields() {
        let schema = subscriber_schema();
        assert_eq!(schema.fields().len(), 9);
        assert!(schema.field_with_name("imsi").is_ok());
        assert!(schema.field_with_name("iccid").is_ok());
        assert!(schema.field_with_name("arpu").is_ok());
    }

    #[test]
    fn test_tower_schema_fields() {
        let schema = tower_schema();
        assert_eq!(schema.fields().len(), 9);
        assert!(schema.field_with_name("lat").is_ok());
        assert!(schema.field_with_name("lon").is_ok());
        assert!(schema.field_with_name("technology").is_ok());
        assert!(schema.field_with_name("operator").is_ok());
    }

    #[test]
    fn test_network_event_schema_fields() {
        let schema = network_event_schema();
        assert_eq!(schema.fields().len(), 7);
        assert!(schema.field_with_name("event_id").is_ok());
        assert!(schema.field_with_name("severity").is_ok());
        assert!(schema.field_with_name("affected_subscribers").is_ok());
    }
}
