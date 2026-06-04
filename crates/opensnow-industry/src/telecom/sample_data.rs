use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    Date32Array, Float64Array, Int32Array, Int64Array, StringArray, TimestampMicrosecondArray,
};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use super::schemas;

/// Swedish MCC = 240. Known MNC codes for Swedish operators.
const OPERATORS: &[(&str, &str)] = &[
    ("Telia", "24001"),
    ("Tele2", "24007"),
    ("Telenor", "24004"),
    ("3 (Hi3G)", "24002"),
    ("Comviq", "24005"),
];

/// Realistic Swedish tower locations: (name, lat, lon, region).
const TOWER_LOCATIONS: &[(&str, f64, f64, &str)] = &[
    ("Stockholm-Central", 59.3293, 18.0686, "Stockholm"),
    ("Stockholm-Kista", 59.4030, 17.9440, "Stockholm"),
    ("Stockholm-Sodermalm", 59.3150, 18.0710, "Stockholm"),
    ("Gothenburg-Hisingen", 57.7210, 11.9410, "Vastra Gotaland"),
    ("Gothenburg-Centrum", 57.7089, 11.9746, "Vastra Gotaland"),
    ("Malmo-Central", 55.6049, 13.0038, "Skane"),
    ("Malmo-Hyllie", 55.5636, 12.9740, "Skane"),
    ("Uppsala-City", 59.8586, 17.6389, "Uppsala"),
    ("Linkoping-City", 58.4108, 15.6214, "Ostergotland"),
    ("Vasteras-City", 59.6099, 16.5448, "Vastmanland"),
    ("Orebro-City", 59.2753, 15.2134, "Orebro"),
    ("Lulea-City", 65.5848, 22.1547, "Norrbotten"),
    ("Umea-City", 63.8258, 20.2630, "Vasterbotten"),
    ("Sundsvall-City", 62.3908, 17.3069, "Vasternorrland"),
    ("Jonkoping-City", 57.7826, 14.1618, "Jonkoping"),
    ("Norrkoping-City", 58.5877, 16.1924, "Ostergotland"),
    ("Karlstad-City", 59.3793, 13.5036, "Varmland"),
    ("Gavle-City", 60.6749, 17.1413, "Gavleborg"),
    ("Boras-City", 57.7210, 12.9401, "Vastra Gotaland"),
    ("Helsingborg-City", 56.0465, 12.6945, "Skane"),
];

const TECHNOLOGIES: &[&str] = &["2G", "3G", "4G", "5G"];
const CALL_STATUSES: &[&str] = &["ANSWERED", "NO_ANSWER", "BUSY", "FAILED"];
const CODECS: &[&str] = &["AMR-NB", "AMR-WB", "EVS", "G.711"];
const SMS_TYPES: &[&str] = &["MT", "MO"];
const DELIVERY_STATUSES: &[&str] = &["DELIVERED", "PENDING", "FAILED", "EXPIRED"];
const RAT_TYPES: &[&str] = &["LTE", "NR", "UMTS", "GERAN"];
const APNS: &[&str] = &[
    "internet.example.net",
    "mms.example.net",
    "corporate.vpn",
    "iot.m2m.example",
];
const PLANS: &[&str] = &[
    "Mobil Lagom",
    "Mobil Mycket",
    "Mobil Obegransad",
    "Foretag S",
    "Foretag M",
    "Foretag L",
];
const SUBSCRIBER_STATUSES: &[&str] = &["ACTIVE", "SUSPENDED", "TERMINATED", "PORTING_OUT"];
const REGIONS: &[&str] = &[
    "Stockholm",
    "Vastra Gotaland",
    "Skane",
    "Uppsala",
    "Ostergotland",
    "Vastmanland",
    "Orebro",
    "Norrbotten",
    "Vasterbotten",
];
const EVENT_TYPES: &[&str] = &[
    "LINK_DOWN",
    "HIGH_CPU",
    "PACKET_LOSS",
    "HANDOVER_FAILURE",
    "CELL_OUTAGE",
    "INTERFERENCE",
    "OVERLOAD",
];
const SEVERITIES: &[&str] = &["CRITICAL", "MAJOR", "MINOR", "WARNING", "INFO"];

const FIRST_NAMES: &[&str] = &[
    "Erik", "Anna", "Lars", "Maria", "Karl", "Eva", "Johan", "Karin", "Anders", "Lena", "Per",
    "Sara", "Olof", "Emma", "Nils", "Ingrid", "Gustav", "Astrid", "Fredrik", "Kristina",
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
    "Pettersson",
    "Jonsson",
    "Lindberg",
    "Magnusson",
    "Lindstrom",
];

fn gen_swedish_msisdn(rng: &mut StdRng) -> String {
    let prefix = if rng.gen_bool(0.5) { "70" } else { "73" };
    let num: u32 = rng.gen_range(1_000_000..9_999_999);
    format!("+46{prefix}{num}")
}

fn gen_imsi(rng: &mut StdRng, mcc_mnc: &str) -> String {
    let suffix: u64 = rng.gen_range(1_000_000_000..9_999_999_999);
    format!("{mcc_mnc}{suffix}")
}

fn gen_iccid(rng: &mut StdRng) -> String {
    let mid: u64 = rng.gen_range(10_000_000_000_000_000..99_999_999_999_999_999);
    format!("8946{mid}0")
}

fn gen_tower_id(index: usize, tech: &str) -> String {
    format!("SE-TWR-{tech}-{index:04}")
}

/// Timestamp as microseconds since epoch.
fn random_timestamp_usec(rng: &mut StdRng, base_epoch_usec: i64, range_hours: i64) -> i64 {
    let offset_usec = rng.gen_range(0..range_hours * 3_600_000_000);
    base_epoch_usec + offset_usec
}

/// Generate all telecom tables as Parquet files at the given path.
///
/// `scale` controls the number of rows:
///   - towers: 20 * scale
///   - subscribers: 1000 * scale
///   - cdr_voice: 10000 * scale
///   - cdr_sms: 5000 * scale
///   - cdr_data: 8000 * scale
///   - network_events: 500 * scale
pub fn generate_telecom_dataset(path: &Path, scale: usize) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    let mut rng = StdRng::seed_from_u64(42);

    // ---- towers ----
    let n_towers = 20 * scale;
    generate_towers(path, n_towers, &mut rng)?;

    // Collect tower IDs for reference
    let tower_ids: Vec<String> = (0..n_towers)
        .map(|i| {
            let loc_idx = i % TOWER_LOCATIONS.len();
            let tech = TECHNOLOGIES[i % TECHNOLOGIES.len()];
            gen_tower_id(
                loc_idx * TECHNOLOGIES.len() + (i % TECHNOLOGIES.len()),
                tech,
            )
        })
        .collect();

    // ---- subscribers ----
    let n_subs = 1000 * scale;
    let msisdns = generate_subscribers(path, n_subs, &mut rng)?;

    // Base epoch: 2025-01-01 00:00:00 UTC in microseconds
    let base_epoch_usec: i64 = 1_735_689_600_000_000;
    let range_hours: i64 = 24 * 90; // 90 days of data

    // ---- cdr_voice ----
    generate_cdr_voice(
        path,
        10_000 * scale,
        &msisdns,
        &tower_ids,
        base_epoch_usec,
        range_hours,
        &mut rng,
    )?;

    // ---- cdr_sms ----
    generate_cdr_sms(
        path,
        5_000 * scale,
        &msisdns,
        &tower_ids,
        base_epoch_usec,
        range_hours,
        &mut rng,
    )?;

    // ---- cdr_data ----
    generate_cdr_data(
        path,
        8_000 * scale,
        &msisdns,
        &tower_ids,
        base_epoch_usec,
        range_hours,
        &mut rng,
    )?;

    // ---- network_events ----
    generate_network_events(
        path,
        500 * scale,
        &tower_ids,
        base_epoch_usec,
        range_hours,
        &mut rng,
    )?;

    tracing::info!(
        "Generated telecom dataset at {} with scale factor {scale}",
        path.display()
    );
    Ok(())
}

fn generate_towers(path: &Path, n: usize, rng: &mut StdRng) -> anyhow::Result<()> {
    let schema = Arc::new(schemas::tower_schema());

    let mut tower_ids = Vec::with_capacity(n);
    let mut names = Vec::with_capacity(n);
    let mut lats = Vec::with_capacity(n);
    let mut lons = Vec::with_capacity(n);
    let mut heights = Vec::with_capacity(n);
    let mut techs = Vec::with_capacity(n);
    let mut regions = Vec::with_capacity(n);
    let mut capacities = Vec::with_capacity(n);
    let mut operators = Vec::with_capacity(n);

    for i in 0..n {
        let loc_idx = i % TOWER_LOCATIONS.len();
        let (loc_name, lat, lon, region) = TOWER_LOCATIONS[loc_idx];
        let tech = TECHNOLOGIES[i % TECHNOLOGIES.len()];
        let tid = gen_tower_id(i, tech);
        let (op_name, _) = OPERATORS[i % OPERATORS.len()];

        tower_ids.push(tid);
        names.push(format!("{loc_name}-{tech}"));
        lats.push(lat + rng.gen_range(-0.01..0.01));
        lons.push(lon + rng.gen_range(-0.01..0.01));
        heights.push(rng.gen_range(15.0..80.0));
        techs.push(tech.to_string());
        regions.push(region.to_string());
        capacities.push(rng.gen_range(100..2000));
        operators.push(op_name.to_string());
    }

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(tower_ids)),
            Arc::new(StringArray::from(names)),
            Arc::new(Float64Array::from(lats)),
            Arc::new(Float64Array::from(lons)),
            Arc::new(Float64Array::from(heights)),
            Arc::new(StringArray::from(techs)),
            Arc::new(StringArray::from(regions)),
            Arc::new(Int32Array::from(capacities)),
            Arc::new(StringArray::from(operators)),
        ],
    )?;

    write_parquet(path, "towers.parquet", &schema, &batch)
}

fn generate_subscribers(path: &Path, n: usize, rng: &mut StdRng) -> anyhow::Result<Vec<String>> {
    let schema = Arc::new(schemas::subscriber_schema());

    let mut msisdns = Vec::with_capacity(n);
    let mut imsis = Vec::with_capacity(n);
    let mut iccids = Vec::with_capacity(n);
    let mut names_vec = Vec::with_capacity(n);
    let mut plans = Vec::with_capacity(n);
    let mut statuses = Vec::with_capacity(n);
    let mut activation_dates = Vec::with_capacity(n);
    let mut sub_regions = Vec::with_capacity(n);
    let mut arpus = Vec::with_capacity(n);

    for _ in 0..n {
        let msisdn = gen_swedish_msisdn(rng);
        let (_, mcc_mnc) = OPERATORS.choose(rng).unwrap();
        let imsi = gen_imsi(rng, mcc_mnc);
        let iccid = gen_iccid(rng);
        let first = FIRST_NAMES.choose(rng).unwrap();
        let last = LAST_NAMES.choose(rng).unwrap();

        msisdns.push(msisdn);
        imsis.push(imsi);
        iccids.push(iccid);
        names_vec.push(format!("{first} {last}"));
        plans.push(PLANS.choose(rng).unwrap().to_string());
        statuses.push(
            if rng.gen_bool(0.9) {
                "ACTIVE"
            } else {
                SUBSCRIBER_STATUSES.choose(rng).unwrap()
            }
            .to_string(),
        );
        // Random date between 2018-01-01 and 2025-01-01 (days since epoch)
        let days = rng.gen_range(17532..20089);
        activation_dates.push(days);
        sub_regions.push(REGIONS.choose(rng).unwrap().to_string());
        let arpu_val: f64 = rng.gen_range(99.0..599.0);
        arpus.push((arpu_val * 100.0).round() / 100.0);
    }

    let return_msisdns = msisdns.clone();

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(msisdns)),
            Arc::new(StringArray::from(imsis)),
            Arc::new(StringArray::from(iccids)),
            Arc::new(StringArray::from(names_vec)),
            Arc::new(StringArray::from(plans)),
            Arc::new(StringArray::from(statuses)),
            Arc::new(Date32Array::from(activation_dates)),
            Arc::new(StringArray::from(sub_regions)),
            Arc::new(Float64Array::from(arpus)),
        ],
    )?;

    write_parquet(path, "subscribers.parquet", &schema, &batch)?;
    Ok(return_msisdns)
}

fn generate_cdr_voice(
    path: &Path,
    n: usize,
    msisdns: &[String],
    tower_ids: &[String],
    base_epoch_usec: i64,
    range_hours: i64,
    rng: &mut StdRng,
) -> anyhow::Result<()> {
    let schema = Arc::new(schemas::cdr_voice_schema());

    let mut callers = Vec::with_capacity(n);
    let mut callees = Vec::with_capacity(n);
    let mut start_times = Vec::with_capacity(n);
    let mut durations = Vec::with_capacity(n);
    let mut towers = Vec::with_capacity(n);
    let mut cells = Vec::with_capacity(n);
    let mut statuses = Vec::with_capacity(n);
    let mut codecs_vec = Vec::with_capacity(n);
    let mut mcc_mncs = Vec::with_capacity(n);

    for _ in 0..n {
        let caller = msisdns.choose(rng).unwrap().clone();
        let callee = msisdns.choose(rng).unwrap().clone();
        let ts = random_timestamp_usec(rng, base_epoch_usec, range_hours);
        // Duration: most calls short (30-180s), some long (up to 3600s)
        let dur = if rng.gen_bool(0.8) {
            rng.gen_range(10..300)
        } else {
            rng.gen_range(300..3600)
        };
        let tower = tower_ids.choose(rng).unwrap().clone();
        let cell = format!("{}-C{}", &tower, rng.gen_range(1..4));
        let status = CALL_STATUSES.choose(rng).unwrap().to_string();
        let codec = CODECS.choose(rng).unwrap().to_string();
        let (_, mnc) = OPERATORS.choose(rng).unwrap();

        callers.push(caller);
        callees.push(callee);
        start_times.push(ts);
        durations.push(dur);
        towers.push(tower);
        cells.push(cell);
        statuses.push(status);
        codecs_vec.push(codec);
        mcc_mncs.push(mnc.to_string());
    }

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(callers)),
            Arc::new(StringArray::from(callees)),
            Arc::new(TimestampMicrosecondArray::from(start_times).with_timezone("UTC")),
            Arc::new(Int32Array::from(durations)),
            Arc::new(StringArray::from(towers)),
            Arc::new(StringArray::from(cells)),
            Arc::new(StringArray::from(statuses)),
            Arc::new(StringArray::from(codecs_vec)),
            Arc::new(StringArray::from(mcc_mncs)),
        ],
    )?;

    write_parquet(path, "cdr_voice.parquet", &schema, &batch)
}

fn generate_cdr_sms(
    path: &Path,
    n: usize,
    msisdns: &[String],
    tower_ids: &[String],
    base_epoch_usec: i64,
    range_hours: i64,
    rng: &mut StdRng,
) -> anyhow::Result<()> {
    let schema = Arc::new(schemas::cdr_sms_schema());

    let mut senders = Vec::with_capacity(n);
    let mut receivers = Vec::with_capacity(n);
    let mut timestamps = Vec::with_capacity(n);
    let mut msg_types = Vec::with_capacity(n);
    let mut towers = Vec::with_capacity(n);
    let mut del_statuses = Vec::with_capacity(n);

    for _ in 0..n {
        senders.push(msisdns.choose(rng).unwrap().clone());
        receivers.push(msisdns.choose(rng).unwrap().clone());
        timestamps.push(random_timestamp_usec(rng, base_epoch_usec, range_hours));
        msg_types.push(SMS_TYPES.choose(rng).unwrap().to_string());
        towers.push(tower_ids.choose(rng).unwrap().clone());
        del_statuses.push(DELIVERY_STATUSES.choose(rng).unwrap().to_string());
    }

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(senders)),
            Arc::new(StringArray::from(receivers)),
            Arc::new(TimestampMicrosecondArray::from(timestamps).with_timezone("UTC")),
            Arc::new(StringArray::from(msg_types)),
            Arc::new(StringArray::from(towers)),
            Arc::new(StringArray::from(del_statuses)),
        ],
    )?;

    write_parquet(path, "cdr_sms.parquet", &schema, &batch)
}

fn generate_cdr_data(
    path: &Path,
    n: usize,
    msisdns: &[String],
    tower_ids: &[String],
    base_epoch_usec: i64,
    range_hours: i64,
    rng: &mut StdRng,
) -> anyhow::Result<()> {
    let schema = Arc::new(schemas::cdr_data_schema());

    let mut msisdn_col = Vec::with_capacity(n);
    let mut start_times = Vec::with_capacity(n);
    let mut end_times = Vec::with_capacity(n);
    let mut bytes_ups = Vec::with_capacity(n);
    let mut bytes_downs = Vec::with_capacity(n);
    let mut apns = Vec::with_capacity(n);
    let mut rats = Vec::with_capacity(n);
    let mut towers = Vec::with_capacity(n);

    for _ in 0..n {
        let start = random_timestamp_usec(rng, base_epoch_usec, range_hours);
        let session_len_usec = rng.gen_range(60_000_000i64..7_200_000_000); // 1 min to 2 hrs

        msisdn_col.push(msisdns.choose(rng).unwrap().clone());
        start_times.push(start);
        end_times.push(start + session_len_usec);
        bytes_ups.push(rng.gen_range(1024i64..100_000_000));
        bytes_downs.push(rng.gen_range(10240i64..2_000_000_000));
        apns.push(APNS.choose(rng).unwrap().to_string());
        rats.push(RAT_TYPES.choose(rng).unwrap().to_string());
        towers.push(tower_ids.choose(rng).unwrap().clone());
    }

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(msisdn_col)),
            Arc::new(TimestampMicrosecondArray::from(start_times).with_timezone("UTC")),
            Arc::new(TimestampMicrosecondArray::from(end_times).with_timezone("UTC")),
            Arc::new(Int64Array::from(bytes_ups)),
            Arc::new(Int64Array::from(bytes_downs)),
            Arc::new(StringArray::from(apns)),
            Arc::new(StringArray::from(rats)),
            Arc::new(StringArray::from(towers)),
        ],
    )?;

    write_parquet(path, "cdr_data.parquet", &schema, &batch)
}

fn generate_network_events(
    path: &Path,
    n: usize,
    tower_ids: &[String],
    base_epoch_usec: i64,
    range_hours: i64,
    rng: &mut StdRng,
) -> anyhow::Result<()> {
    let schema = Arc::new(schemas::network_event_schema());

    let mut event_ids = Vec::with_capacity(n);
    let mut timestamps = Vec::with_capacity(n);
    let mut towers = Vec::with_capacity(n);
    let mut event_types = Vec::with_capacity(n);
    let mut severities = Vec::with_capacity(n);
    let mut descriptions = Vec::with_capacity(n);
    let mut affected = Vec::with_capacity(n);

    for i in 0..n {
        let ev_type = EVENT_TYPES.choose(rng).unwrap();
        let severity = SEVERITIES.choose(rng).unwrap();
        let tower = tower_ids.choose(rng).unwrap().clone();

        event_ids.push(format!("EVT-{:08}", i + 1));
        timestamps.push(random_timestamp_usec(rng, base_epoch_usec, range_hours));
        let desc = match *ev_type {
            "LINK_DOWN" => format!("Backhaul link failure on {tower}"),
            "HIGH_CPU" => format!("CPU utilization >90% on {tower}"),
            "PACKET_LOSS" => format!("Packet loss >5% detected on {tower}"),
            "HANDOVER_FAILURE" => format!("Handover failure rate >10% on {tower}"),
            "CELL_OUTAGE" => format!("Complete cell outage on {tower}"),
            "INTERFERENCE" => format!("Co-channel interference detected on {tower}"),
            "OVERLOAD" => format!("RRC connection overload on {tower}"),
            _ => format!("Unknown event on {tower}"),
        };
        towers.push(tower);
        event_types.push(ev_type.to_string());
        severities.push(severity.to_string());
        descriptions.push(desc);
        affected.push(rng.gen_range(0..500));
    }

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(event_ids)),
            Arc::new(TimestampMicrosecondArray::from(timestamps).with_timezone("UTC")),
            Arc::new(StringArray::from(towers)),
            Arc::new(StringArray::from(event_types)),
            Arc::new(StringArray::from(severities)),
            Arc::new(StringArray::from(descriptions)),
            Arc::new(Int32Array::from(affected)),
        ],
    )?;

    write_parquet(path, "network_events.parquet", &schema, &batch)
}

fn write_parquet(
    dir: &Path,
    filename: &str,
    schema: &Arc<arrow::datatypes::Schema>,
    batch: &RecordBatch,
) -> anyhow::Result<()> {
    let file_path = dir.join(filename);
    let file = std::fs::File::create(&file_path)?;
    let mut writer = ArrowWriter::try_new(file, schema.clone(), None)?;
    writer.write(batch)?;
    writer.close()?;
    tracing::debug!("Wrote {}", file_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_telecom_dataset_scale_1() {
        let tmp = TempDir::new().unwrap();
        generate_telecom_dataset(tmp.path(), 1).unwrap();

        // Verify all files exist
        assert!(tmp.path().join("towers.parquet").exists());
        assert!(tmp.path().join("subscribers.parquet").exists());
        assert!(tmp.path().join("cdr_voice.parquet").exists());
        assert!(tmp.path().join("cdr_sms.parquet").exists());
        assert!(tmp.path().join("cdr_data.parquet").exists());
        assert!(tmp.path().join("network_events.parquet").exists());
    }

    #[test]
    fn test_parquet_readable() {
        let tmp = TempDir::new().unwrap();
        generate_telecom_dataset(tmp.path(), 1).unwrap();

        // Read back towers file and verify row count
        let file = std::fs::File::open(tmp.path().join("towers.parquet")).unwrap();
        let reader =
            parquet::arrow::arrow_reader::ParquetRecordBatchReader::try_new(file, 1024).unwrap();
        let batches: Vec<_> = reader.into_iter().map(|b| b.unwrap()).collect();
        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 20); // scale=1 -> 20 towers
    }

    #[test]
    fn test_swedish_msisdn_format() {
        let mut rng = StdRng::seed_from_u64(1);
        let msisdn = gen_swedish_msisdn(&mut rng);
        assert!(msisdn.starts_with("+46"));
        assert_eq!(msisdn.len(), 12); // +46 (3) + 2 prefix + 7 digits = 12
    }

    #[test]
    fn test_imsi_format() {
        let mut rng = StdRng::seed_from_u64(1);
        let imsi = gen_imsi(&mut rng, "24007");
        assert!(imsi.starts_with("24007"));
        assert_eq!(imsi.len(), 15);
    }
}
