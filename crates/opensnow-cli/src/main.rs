#![allow(
    clippy::arc_with_non_send_sync,
    clippy::items_after_test_module,
    clippy::print_literal
)]

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use arrow::array::RecordBatch;
use arrow::array::{Float64Array, Int64Array, StringArray, TimestampSecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::util::pretty::pretty_format_batches;
use clap::{Parser, Subcommand};
use opensnow_core::cli::OpenSnowCliReport;
use opensnow_core::{OpenSnowConfig, OpenSnowEngine};
use opensnow_server::OpenSnowServer;
use opensnow_server::sql_guardrails::validate_demo_sql;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "opensnow",
    about = "OpenSnow - Open-source analytics data warehouse",
    version
)]
struct Cli {
    /// Path to config file
    #[arg(long, global = true)]
    config: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new OpenSnow project
    Init {
        /// Load sample data
        #[arg(long)]
        with_sample_data: bool,

        /// Industry template: telecom, banking, or both
        #[arg(long, default_value = "telecom")]
        industry: String,
    },

    /// Start the OpenSnow server
    Start {
        /// HTTP API port
        #[arg(long)]
        http_port: Option<u16>,

        /// PostgreSQL wire protocol port
        #[arg(long)]
        pg_port: Option<u16>,

        /// Enable PostgreSQL wire protocol listener (disabled by default for public demo safety)
        #[arg(long)]
        enable_pgwire: bool,

        /// Server role: standalone (default), coordinator, or worker
        #[arg(long, default_value = "standalone")]
        role: String,

        /// Coordinator gRPC address (for worker mode)
        #[arg(long, default_value = "http://localhost:9100")]
        coordinator: String,

        /// gRPC port for inter-node communication
        #[arg(long, default_value = "9100")]
        grpc_port: u16,
    },

    /// Execute a SQL query directly (no server needed)
    #[command(name = "local")]
    Local {
        /// SQL query to execute
        sql: String,
    },

    /// Interactive SQL shell
    Shell {
        /// Execute a single query and exit
        #[arg(short, long)]
        command: Option<String>,
    },

    /// Show recent queries from the catalog
    Queries {
        /// Limit
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// Reset ephemeral catalog runtime state, preserving registered tables and sample data files
    ResetRuntimeState,

    /// Analyze query history and propose schema optimizations
    OptimizeSchema {
        /// Maximum number of recent queries to analyze
        #[arg(long, default_value = "200")]
        limit: usize,
    },

    /// Reconcile warehouses against a worker pool (operator dry-run)
    OperatorPlan {
        /// Current worker replicas per warehouse, e.g. "default=2,etl=0"
        #[arg(long, default_value = "")]
        current: String,
    },

    /// Apply operator reconcile plan to Kubernetes cluster via kubectl
    OperatorApply {
        /// Kubernetes namespace for worker StatefulSets
        #[arg(long, default_value = "opensnow")]
        namespace: String,

        /// Print the plan without calling kubectl
        #[arg(long)]
        dry_run: bool,
    },

    /// Show server status
    Status {
        /// Server host
        #[arg(long, default_value = "localhost")]
        host: String,

        /// HTTP port
        #[arg(long, default_value = "8080")]
        port: u16,
    },

    /// Bootstrap an enterprise account/org/workspace in the local catalog
    AccountRegister {
        /// Customer-visible account name
        #[arg(long)]
        account_name: String,

        /// Initial owner email; persisted as the ACCOUNTOWNER membership
        #[arg(long)]
        owner_email: String,
    },

    /// OpenSnow command-line contract and readiness utilities
    Cli {
        #[command(subcommand)]
        command: CliCommands,
    },

    /// Create an account-scoped workspace in the local catalog
    AccountWorkspaceCreate {
        /// Target account id/slug
        #[arg(long)]
        account_id: String,

        /// New workspace name
        #[arg(long)]
        name: String,

        /// Acting account id/slug. Defaults to --account-id for same-account admin UX.
        #[arg(long)]
        actor_account_id: Option<String>,
    },
}

#[derive(Subcommand)]
enum CliCommands {
    /// Print the stable OpenSnow CLI and agent-facing contract
    Contract {
        /// Output format: text or json
        #[arg(long, default_value = "text", value_parser = ["text", "json"])]
        format: String,
    },

    /// Check current CLI config against enterprise self-service readiness expectations
    Doctor {
        /// Output format: text or json
        #[arg(long, default_value = "text", value_parser = ["text", "json"])]
        format: String,
    },
}

fn load_config(cli_config: &Option<String>) -> OpenSnowConfig {
    if let Some(path) = cli_config {
        OpenSnowConfig::load_from(path).unwrap_or_else(|e| {
            eprintln!("Failed to load config from {}: {}", path, e);
            std::process::exit(1);
        })
    } else {
        OpenSnowConfig::load()
    }
}

fn create_engine(config: &OpenSnowConfig) -> Result<OpenSnowEngine> {
    OpenSnowEngine::try_from_config_and_catalog(config.storage.clone(), &config.catalog.path)
        .map_err(|e| anyhow::anyhow!("failed to initialize OpenSnow storage/catalog state: {e:#}"))
}

fn print_cli_report(config: &OpenSnowConfig, format: &str) -> Result<()> {
    let report = OpenSnowCliReport::from_config(config);
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => print!("{}", report.render_text()),
        other => anyhow::bail!("unsupported cli output format: {other}"),
    }
    Ok(())
}

fn generate_sample_data(warehouse_path: &str) -> Result<()> {
    let data_dir = format!("{warehouse_path}/opensnow/public");
    std::fs::create_dir_all(&data_dir)?;

    // CDRs (Call Detail Records)
    generate_cdrs(&format!("{data_dir}/cdrs.parquet"))?;
    // Subscribers
    generate_subscribers(&format!("{data_dir}/subscribers.parquet"))?;
    // Cell towers
    generate_towers(&format!("{data_dir}/towers.parquet"))?;

    println!("Sample data written to {data_dir}/");
    Ok(())
}

fn generate_cdrs(path: &str) -> Result<()> {
    let n = 10_000;
    let mut ids = Vec::with_capacity(n);
    let mut callers = Vec::with_capacity(n);
    let mut callees = Vec::with_capacity(n);
    let mut durations = Vec::with_capacity(n);
    let mut timestamps = Vec::with_capacity(n);
    let mut tower_ids = Vec::with_capacity(n);
    let mut call_types = Vec::with_capacity(n);

    let base_ts: i64 = 1_740_000_000; // ~2025-02-20

    for i in 0..n {
        ids.push(i as i64 + 1);
        callers.push(format!("+4670{:07}", (i * 7 + 13) % 10_000_000));
        callees.push(format!("+4673{:07}", (i * 11 + 37) % 10_000_000));
        durations.push(((i * 17 + 5) % 3600) as f64);
        timestamps.push(base_ts + (i as i64 * 87));
        tower_ids.push((i % 500) as i64 + 1);
        call_types.push(if i % 3 == 0 {
            "voice"
        } else if i % 3 == 1 {
            "sms"
        } else {
            "data"
        });
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("cdr_id", DataType::Int64, false),
        Field::new("caller", DataType::Utf8, false),
        Field::new("callee", DataType::Utf8, false),
        Field::new("duration_seconds", DataType::Float64, false),
        Field::new(
            "timestamp",
            DataType::Timestamp(TimeUnit::Second, None),
            false,
        ),
        Field::new("tower_id", DataType::Int64, false),
        Field::new("call_type", DataType::Utf8, false),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(callers)),
            Arc::new(StringArray::from(callees)),
            Arc::new(Float64Array::from(durations)),
            Arc::new(TimestampSecondArray::from(timestamps)),
            Arc::new(Int64Array::from(tower_ids)),
            Arc::new(StringArray::from(call_types)),
        ],
    )?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build();
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    println!("  CDRs:        10,000 rows -> {path}");
    Ok(())
}

fn generate_subscribers(path: &str) -> Result<()> {
    let n = 5_000;
    let mut ids = Vec::with_capacity(n);
    let mut phones = Vec::with_capacity(n);
    let mut names = Vec::with_capacity(n);
    let mut plans = Vec::with_capacity(n);
    let mut regions = Vec::with_capacity(n);
    let mut arpus = Vec::with_capacity(n);

    let plan_names = ["Basic", "Standard", "Premium", "Enterprise"];
    let region_names = ["Stockholm", "Gothenburg", "Malmo", "Uppsala", "Linkoping"];
    let first_names = [
        "Erik", "Anna", "Lars", "Maria", "Anders", "Sara", "Johan", "Eva",
    ];
    let last_names = [
        "Svensson",
        "Johansson",
        "Karlsson",
        "Nilsson",
        "Eriksson",
        "Larsson",
    ];

    for i in 0..n {
        ids.push(i as i64 + 1);
        phones.push(format!("+4670{:07}", (i * 7 + 13) % 10_000_000));
        names.push(format!(
            "{} {}",
            first_names[i % first_names.len()],
            last_names[i % last_names.len()]
        ));
        plans.push(plan_names[i % plan_names.len()]);
        regions.push(region_names[i % region_names.len()]);
        arpus.push(((i * 13 + 100) % 500) as f64 + 99.0);
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("subscriber_id", DataType::Int64, false),
        Field::new("phone", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("plan", DataType::Utf8, false),
        Field::new("region", DataType::Utf8, false),
        Field::new("monthly_arpu", DataType::Float64, false),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(phones)),
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(
                plans.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                regions.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(arpus)),
        ],
    )?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build();
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    println!("  Subscribers: 5,000 rows  -> {path}");
    Ok(())
}

fn generate_towers(path: &str) -> Result<()> {
    let n = 500;
    let mut ids = Vec::with_capacity(n);
    let mut names = Vec::with_capacity(n);
    let mut lats = Vec::with_capacity(n);
    let mut lons = Vec::with_capacity(n);
    let mut regions = Vec::with_capacity(n);
    let mut capacities = Vec::with_capacity(n);

    let region_names = ["Stockholm", "Gothenburg", "Malmo", "Uppsala", "Linkoping"];

    for i in 0..n {
        ids.push(i as i64 + 1);
        names.push(format!("TOWER-{:04}", i + 1));
        lats.push(55.6 + (i as f64 * 0.02) % 4.0);
        lons.push(12.0 + (i as f64 * 0.03) % 6.0);
        regions.push(region_names[i % region_names.len()]);
        capacities.push(((i * 37 + 100) % 10000) as i64 + 1000);
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("tower_id", DataType::Int64, false),
        Field::new("tower_name", DataType::Utf8, false),
        Field::new("latitude", DataType::Float64, false),
        Field::new("longitude", DataType::Float64, false),
        Field::new("region", DataType::Utf8, false),
        Field::new("max_capacity", DataType::Int64, false),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(names)),
            Arc::new(Float64Array::from(lats)),
            Arc::new(Float64Array::from(lons)),
            Arc::new(StringArray::from(
                regions.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(capacities)),
        ],
    )?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build();
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    println!("  Towers:      500 rows    -> {path}");
    Ok(())
}

fn generate_banking_sample_data(warehouse_path: &str) -> Result<()> {
    let data_dir = format!("{warehouse_path}/opensnow/public");
    std::fs::create_dir_all(&data_dir)?;

    generate_transactions(&format!("{data_dir}/transactions.parquet"))?;
    generate_accounts(&format!("{data_dir}/accounts.parquet"))?;
    generate_customers(&format!("{data_dir}/customers.parquet"))?;
    println!("Banking sample data written to {data_dir}/");
    Ok(())
}

fn generate_transactions(path: &str) -> Result<()> {
    let n = 50_000;
    let mut ids = Vec::with_capacity(n);
    let mut acc_froms = Vec::with_capacity(n);
    let mut acc_tos = Vec::with_capacity(n);
    let mut amounts = Vec::with_capacity(n);
    let mut currencies = Vec::with_capacity(n);
    let mut timestamps = Vec::with_capacity(n);
    let mut txn_types = Vec::with_capacity(n);
    let mut categories = Vec::with_capacity(n);
    let mut statuses = Vec::with_capacity(n);
    let mut channels = Vec::with_capacity(n);

    let base_ts: i64 = 1_740_000_000;
    let currency_list = ["SEK", "EUR", "USD"];
    let type_list = ["debit", "credit", "transfer", "payment"];
    let category_list = [
        "groceries",
        "restaurant",
        "transport",
        "salary",
        "utilities",
        "entertainment",
        "healthcare",
        "rent",
    ];
    let channel_list = ["online", "atm", "pos", "branch", "mobile"];

    for i in 0..n {
        ids.push(format!("TXN-{:08}", i + 1));
        acc_froms.push(format!("SE{:020}", (i * 7 + 1000) % 99999999));
        acc_tos.push(format!("SE{:020}", (i * 13 + 5000) % 99999999));
        amounts.push(((i * 17 + 50) % 50000) as f64 / 100.0 + 10.0);
        currencies.push(currency_list[i % currency_list.len()]);
        timestamps.push(base_ts + (i as i64 * 35));
        txn_types.push(type_list[i % type_list.len()]);
        categories.push(category_list[i % category_list.len()]);
        statuses.push(if i % 50 == 0 { "failed" } else { "completed" });
        channels.push(channel_list[i % channel_list.len()]);
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("txn_id", DataType::Utf8, false),
        Field::new("account_from", DataType::Utf8, false),
        Field::new("account_to", DataType::Utf8, false),
        Field::new("amount", DataType::Float64, false),
        Field::new("currency", DataType::Utf8, false),
        Field::new(
            "timestamp",
            DataType::Timestamp(TimeUnit::Second, None),
            false,
        ),
        Field::new("txn_type", DataType::Utf8, false),
        Field::new("merchant_category", DataType::Utf8, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("channel", DataType::Utf8, false),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(StringArray::from(acc_froms)),
            Arc::new(StringArray::from(acc_tos)),
            Arc::new(Float64Array::from(amounts)),
            Arc::new(StringArray::from(
                currencies.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
            Arc::new(TimestampSecondArray::from(timestamps)),
            Arc::new(StringArray::from(
                txn_types.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                categories.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                statuses.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                channels.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
        ],
    )?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build();
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    println!("  Transactions: 50,000 rows -> {path}");
    Ok(())
}

fn generate_accounts(path: &str) -> Result<()> {
    let n = 10_000;
    let mut ids = Vec::with_capacity(n);
    let mut customer_ids = Vec::with_capacity(n);
    let mut acc_types = Vec::with_capacity(n);
    let mut currencies = Vec::with_capacity(n);
    let mut balances = Vec::with_capacity(n);
    let mut ibans = Vec::with_capacity(n);
    let mut statuses = Vec::with_capacity(n);

    let type_list = ["checking", "savings", "loan", "credit"];
    let currency_list = ["SEK", "EUR", "USD"];

    for i in 0..n {
        ids.push(format!("ACC-{:06}", i + 1));
        customer_ids.push(format!("CUST-{:06}", (i % 5000) + 1));
        acc_types.push(type_list[i % type_list.len()]);
        currencies.push(currency_list[i % currency_list.len()]);
        balances.push(((i * 31 + 1000) % 1_000_000) as f64 + 500.0);
        ibans.push(format!("SE{:02}{:020}", (i * 3 + 35) % 99, i + 1000000));
        statuses.push(if i % 100 == 0 { "closed" } else { "active" });
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("account_id", DataType::Utf8, false),
        Field::new("customer_id", DataType::Utf8, false),
        Field::new("account_type", DataType::Utf8, false),
        Field::new("currency", DataType::Utf8, false),
        Field::new("balance", DataType::Float64, false),
        Field::new("iban", DataType::Utf8, false),
        Field::new("status", DataType::Utf8, false),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(StringArray::from(customer_ids)),
            Arc::new(StringArray::from(
                acc_types.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                currencies.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(balances)),
            Arc::new(StringArray::from(ibans)),
            Arc::new(StringArray::from(
                statuses.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
        ],
    )?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build();
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    println!("  Accounts:     10,000 rows -> {path}");
    Ok(())
}

fn generate_customers(path: &str) -> Result<()> {
    let n = 5_000;
    let mut ids = Vec::with_capacity(n);
    let mut names = Vec::with_capacity(n);
    let mut kyc_statuses = Vec::with_capacity(n);
    let mut risk_scores = Vec::with_capacity(n);
    let mut segments = Vec::with_capacity(n);
    let mut countries = Vec::with_capacity(n);

    let first_names = [
        "Erik", "Anna", "Lars", "Maria", "Anders", "Sara", "Johan", "Eva", "Oscar", "Linnea",
    ];
    let last_names = [
        "Svensson",
        "Johansson",
        "Karlsson",
        "Nilsson",
        "Eriksson",
        "Larsson",
        "Berg",
        "Lund",
    ];
    let segment_list = ["retail", "private", "corporate"];
    let country_list = ["SE", "NO", "DK", "FI", "DE"];

    for i in 0..n {
        ids.push(format!("CUST-{:06}", i + 1));
        names.push(format!(
            "{} {}",
            first_names[i % first_names.len()],
            last_names[i % last_names.len()]
        ));
        kyc_statuses.push(if i % 20 == 0 { "pending" } else { "verified" });
        risk_scores.push(((i * 7 + 10) % 100) as i64);
        segments.push(segment_list[i % segment_list.len()]);
        countries.push(country_list[i % country_list.len()]);
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("customer_id", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("kyc_status", DataType::Utf8, false),
        Field::new("risk_score", DataType::Int64, false),
        Field::new("segment", DataType::Utf8, false),
        Field::new("country", DataType::Utf8, false),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(
                kyc_statuses
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(risk_scores)),
            Arc::new(StringArray::from(
                segments.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                countries.into_iter().map(String::from).collect::<Vec<_>>(),
            )),
        ],
    )?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build();
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    println!("  Customers:    5,000 rows  -> {path}");
    Ok(())
}

fn truncate_sql(sql: &str, max_len: usize) -> String {
    if sql.len() <= max_len {
        sql.to_string()
    } else {
        let mut s = sql[..max_len].to_string();
        s.push_str("...");
        s
    }
}

fn extract_table_name(sql: &str) -> Option<String> {
    // Very simple heuristic: look for " from <table>" (case-insensitive) and
    // return the next token until whitespace/comma/semicolon.
    let lower = sql.to_lowercase();
    let needle = " from ";
    let idx = lower.find(needle)?;
    let after = &lower[idx + needle.len()..];
    let after_trimmed = after.trim_start();

    let mut end = after_trimmed.len();
    for (i, ch) in after_trimmed.char_indices() {
        if ch.is_whitespace() || ch == ',' || ch == ';' {
            end = i;
            break;
        }
    }

    if end == 0 {
        None
    } else {
        Some(after_trimmed[..end].to_string())
    }
}

async fn register_warehouse_tables(engine: &OpenSnowEngine) -> Result<()> {
    let warehouse = engine.warehouse_path();
    let table_dir = format!("{warehouse}/opensnow/public");

    if !std::path::Path::new(&table_dir).exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(&table_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "parquet") {
            let name = path.file_stem().unwrap().to_str().unwrap();
            engine
                .register_parquet(name, path.to_str().unwrap())
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn parses_enterprise_account_register_command() {
        let cli = Cli::try_parse_from([
            "opensnow",
            "account-register",
            "--account-name",
            "Acme Corp",
            "--owner-email",
            "owner@acme.test",
        ])
        .expect("account-register command should parse");

        match cli.command {
            Commands::AccountRegister {
                account_name,
                owner_email,
            } => {
                assert_eq!(account_name, "Acme Corp");
                assert_eq!(owner_email, "owner@acme.test");
            }
            _ => panic!("unexpected command parsed"),
        }
    }

    #[test]
    fn parses_opensnow_cli_contract_json_command() {
        let cli = Cli::try_parse_from(["opensnow", "cli", "contract", "--format", "json"])
            .expect("cli contract command should parse");

        match cli.command {
            Commands::Cli { command } => match command {
                CliCommands::Contract { format } => assert_eq!(format, "json"),
                _ => panic!("unexpected cli command parsed"),
            },
            _ => panic!("unexpected command parsed"),
        }
    }

    #[test]
    fn parses_enterprise_account_workspace_create_command() {
        let cli = Cli::try_parse_from([
            "opensnow",
            "account-workspace-create",
            "--account-id",
            "acme-corp",
            "--name",
            "analytics",
            "--actor-account-id",
            "acme-corp",
        ])
        .expect("account-workspace-create command should parse");

        match cli.command {
            Commands::AccountWorkspaceCreate {
                account_id,
                name,
                actor_account_id,
            } => {
                assert_eq!(account_id, "acme-corp");
                assert_eq!(name, "analytics");
                assert_eq!(actor_account_id.as_deref(), Some("acme-corp"));
            }
            _ => panic!("unexpected command parsed"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let is_cli_contract_command = matches!(&cli.command, Commands::Cli { .. });

    // Initialise tracing + OpenTelemetry. `OPENSNOW_OTEL_DISABLED=1` falls
    // back to a plain `fmt` subscriber for environments where the OTel
    // exporter is undesirable (e.g. unit tests piped through cargo). Contract
    // output skips telemetry entirely so `--format json` remains clean JSON.
    let otel_disabled = is_cli_contract_command
        || std::env::var("OPENSNOW_OTEL_DISABLED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    let _telemetry_guard = if is_cli_contract_command {
        None
    } else if otel_disabled {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .try_init()
            .ok();
        None
    } else {
        match opensnow_server::telemetry::init("opensnow") {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("telemetry init failed ({e}); falling back to fmt subscriber");
                tracing_subscriber::fmt()
                    .with_env_filter(
                        EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| EnvFilter::new("info")),
                    )
                    .try_init()
                    .ok();
                None
            }
        }
    };

    let config = load_config(&cli.config);

    match cli.command {
        Commands::Init {
            with_sample_data,
            industry,
        } => {
            let warehouse = &config.storage.warehouse_path;
            std::fs::create_dir_all(warehouse)?;

            // Create default config file
            let config_content = r#"[server]
http_port = 8080
pg_port = 5433
pg_enabled = false
# Loopback by default. To expose OpenSnow, set host = "0.0.0.0" AND enable auth
# (OPENSNOW_JWT_SECRET) or set OPENSNOW_ALLOW_PUBLIC=1 to accept an
# unauthenticated public listener.
host = "127.0.0.1"

[storage]
warehouse_path = "~/.opensnow/warehouse"
# Uncomment for S3/MinIO:
# s3_endpoint = "http://localhost:9000"
# s3_allow_insecure_http = true
# s3_bucket = "opensnow"
# s3_access_key = "OPEN/SNOW/DEMO/ONLY"
# s3_secret_key = "OPEN/SNOW/DEMO/ONLY/STORAGE"

[catalog]
path = "~/.opensnow/catalog.db"
"#;

            if !std::path::Path::new("opensnow.toml").exists() {
                std::fs::write("opensnow.toml", config_content)?;
                println!("Created opensnow.toml");
            }

            if with_sample_data {
                let industries: Vec<&str> = if industry == "both" {
                    vec!["telecom", "banking"]
                } else {
                    vec![industry.as_str()]
                };

                for ind in &industries {
                    match *ind {
                        "telecom" => {
                            println!("Generating sample telecom data...");
                            generate_sample_data(warehouse)?;
                        }
                        "banking" => {
                            println!("Generating sample banking data...");
                            generate_banking_sample_data(warehouse)?;
                        }
                        _ => {
                            eprintln!("Unknown industry: {}. Use: telecom, banking, or both", ind);
                        }
                    }
                }
                println!("\nDone! Start querying:");
                println!("  opensnow start");
                if industries.contains(&"telecom") {
                    println!(
                        "  opensnow shell -c \"SELECT call_type, COUNT(*) FROM cdrs GROUP BY 1\""
                    );
                }
                if industries.contains(&"banking") {
                    println!(
                        "  opensnow shell -c \"SELECT txn_type, SUM(amount) FROM transactions GROUP BY 1\""
                    );
                }
            } else {
                println!("Initialized OpenSnow at {warehouse}");
                println!("Run with --with-sample-data --industry=telecom|banking|both");
            }
        }

        Commands::Start {
            http_port,
            pg_port,
            enable_pgwire,
            role,
            coordinator,
            grpc_port,
        } => {
            let http = http_port.unwrap_or(config.server.http_port);
            let pg = pg_port.unwrap_or(config.server.pg_port);
            let pg_enabled = enable_pgwire || config.server.pg_enabled;
            let host = config.server.host.clone();

            let engine = create_engine(&config)?;
            register_warehouse_tables(&engine).await?;

            match role.as_str() {
                "coordinator" => {
                    use opensnow_distributed::coordinator::Coordinator;
                    let coord = Arc::new(Coordinator::new(engine, grpc_port));

                    // Start gRPC for worker registration in background
                    let coord_grpc = coord.clone();
                    tokio::spawn(async move {
                        if let Err(e) = coord_grpc.start_grpc().await {
                            tracing::error!("Coordinator gRPC error: {}", e);
                        }
                    });

                    // Start HTTP + PG server using the same engine
                    let engine_inner = create_engine(&config)?;
                    register_warehouse_tables(&engine_inner).await?;
                    let server = OpenSnowServer::new_with_options(
                        engine_inner,
                        host.clone(),
                        http,
                        pg,
                        pg_enabled,
                    );
                    server.run().await?;
                }
                "worker" => {
                    use opensnow_distributed::worker::Worker;
                    let hostname = hostname::get()
                        .map(|h| h.to_string_lossy().to_string())
                        .unwrap_or_else(|_| "localhost".to_string());
                    let worker_engine = Arc::new(create_engine(&config)?);

                    let worker = Worker::new(
                        worker_engine,
                        coordinator.clone(),
                        hostname,
                        pg,
                        grpc_port,
                        "default".to_string(),
                    );

                    worker.start().await?;
                    info!("Worker running. Press Ctrl+C to stop.");
                    tokio::signal::ctrl_c().await?;
                }
                _ => {
                    // Standalone mode (default) — no distributed coordination
                    let engine = create_engine(&config)?;
                    register_warehouse_tables(&engine).await?;
                    let server =
                        OpenSnowServer::new_with_options(engine, host, http, pg, pg_enabled);
                    server.run().await?;
                }
            }
        }

        Commands::Local { sql } => {
            let engine = create_engine(&config)?;
            register_warehouse_tables(&engine).await?;
            let sql = validate_demo_sql(&sql).map_err(|message| anyhow::anyhow!(message))?;
            let batches = engine.execute_sql(&sql).await?;
            if !batches.is_empty() {
                println!("{}", pretty_format_batches(&batches)?);
            }
        }

        Commands::Shell { command } => {
            let engine = create_engine(&config)?;
            register_warehouse_tables(&engine).await?;

            if let Some(sql) = command {
                let sql = validate_demo_sql(&sql).map_err(|message| anyhow::anyhow!(message))?;
                let batches = engine.execute_sql(&sql).await?;
                if !batches.is_empty() {
                    println!("{}", pretty_format_batches(&batches)?);
                }
            } else {
                println!("OpenSnow Shell v{}", env!("CARGO_PKG_VERSION"));
                println!("Type SQL queries, or 'quit' to exit.\n");

                let stdin = std::io::stdin();
                let mut buf = String::new();

                loop {
                    eprint!("opensnow> ");
                    buf.clear();
                    if stdin.read_line(&mut buf)? == 0 {
                        break;
                    }
                    let line = buf.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if line.eq_ignore_ascii_case("quit") || line.eq_ignore_ascii_case("exit") {
                        break;
                    }

                    let line = match validate_demo_sql(line) {
                        Ok(sql) => sql,
                        Err(message) => {
                            eprintln!("Error: {message}\n");
                            continue;
                        }
                    };

                    match engine.execute_sql(&line).await {
                        Ok(batches) => {
                            if !batches.is_empty() {
                                match pretty_format_batches(&batches) {
                                    Ok(table) => println!("{table}"),
                                    Err(e) => eprintln!("Format error: {e}"),
                                }
                                let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
                                println!("({rows} rows)\n");
                            } else {
                                println!("OK\n");
                            }
                        }
                        Err(e) => eprintln!("Error: {e}\n"),
                    }
                }
            }
        }

        Commands::Queries { limit } => {
            let engine = create_engine(&config)?;
            let records = engine.catalog().recent_queries(limit)?;

            if records.is_empty() {
                println!("No queries recorded yet.");
            } else {
                println!(
                    "{:<20} {:<10} {:<8} {:>8}  {}",
                    "submitted_at", "warehouse", "status", "dur_ms", "sql",
                );
                for r in records {
                    let sql = truncate_sql(&r.sql, 80);
                    println!(
                        "{:<20} {:<10} {:<8} {:>8}  {}",
                        r.submitted_at, r.warehouse, r.status, r.duration_ms, sql,
                    );
                }
            }
        }

        Commands::ResetRuntimeState => {
            let engine = create_engine(&config)?;
            engine.catalog().reset_runtime_state()?;
            println!(
                "Reset catalog runtime state at {} (query history and materialized view cache metadata cleared; registered tables and sample data files preserved)",
                config.catalog.path
            );
        }

        Commands::OptimizeSchema { limit } => {
            let engine = create_engine(&config)?;
            let records = engine.catalog().recent_queries(limit)?;
            let total_queries = records.len();

            let mut table_counts: HashMap<String, usize> = HashMap::new();
            for rec in &records {
                if let Some(table) = extract_table_name(&rec.sql) {
                    *table_counts.entry(table).or_insert(0) += 1;
                }
            }

            let mut table_freq: Vec<(String, usize)> = table_counts.into_iter().collect();
            table_freq.sort_by_key(|item| std::cmp::Reverse(item.1));

            println!("# Schema Optimization Plan (beta)\n");
            println!("Analyzed {} recent queries.\n", total_queries);

            println!("## Top tables by query frequency");
            if table_freq.is_empty() {
                println!("(No table references detected in recent queries.)\n");
            } else {
                for (table, count) in table_freq.iter().take(10) {
                    println!("- {} ({} queries)", table, count);
                }
                println!();
            }

            println!("## Suggestions (manual for now)");
            println!(
                "- Consider creating star schemas around the top fact-like tables (e.g. CDRs, transactions)."
            );
            println!(
                "- Look for opportunities to materialize marts for the highest-frequency aggregates."
            );
            println!(
                "- Evaluate partitioning and clustering on timestamp / customer dimensions for heavy tables."
            );
            println!("\n(Automatic suggestions will be implemented in a later pass.)");
        }

        Commands::OperatorPlan { current } => {
            use opensnow_distributed::operator::{
                PlanAction, build_reconcile_plan, load_warehouse_specs,
            };

            let engine = create_engine(&config)?;
            let catalog = engine.catalog();
            let specs = load_warehouse_specs(catalog)?;

            let mut current_map = std::collections::HashMap::new();
            if !current.trim().is_empty() {
                for kv in current.split(',') {
                    kv.split_once('=')
                        .and_then(|(name, val)| {
                            val.trim()
                                .parse::<i32>()
                                .ok()
                                .map(|n| (name.trim().to_string(), n))
                        })
                        .map(|(name, n)| current_map.insert(name, n));
                }
            }

            let plan = build_reconcile_plan(&specs, &current_map);

            println!("# Warehouse Reconcile Plan (dry-run)\n");
            for action in plan.actions {
                match action {
                    PlanAction::Scale {
                        warehouse,
                        from,
                        to,
                    } => {
                        println!("- SCALE {:<12} {} -> {}", warehouse, from, to);
                    }
                    PlanAction::Noop {
                        warehouse,
                        replicas,
                    } => {
                        println!("- NOOP  {:<12} (replicas = {})", warehouse, replicas);
                    }
                }
            }
        }

        Commands::OperatorApply { namespace, dry_run } => {
            use opensnow_distributed::k8s::{ApplyOutcome, KubeController};
            use opensnow_distributed::operator::{
                PlanAction, build_reconcile_plan, load_warehouse_specs,
            };

            let engine = create_engine(&config)?;
            let catalog = engine.catalog();
            let specs = load_warehouse_specs(catalog)?;

            if dry_run {
                // Dry-run: print the plan without calling kubectl
                let current_map = std::collections::HashMap::new();
                let plan = build_reconcile_plan(&specs, &current_map);
                println!(
                    "# Warehouse Reconcile Plan (dry-run, namespace={})\n",
                    namespace
                );
                for action in plan.actions {
                    match action {
                        PlanAction::Scale {
                            warehouse,
                            from,
                            to,
                        } => {
                            println!("  SCALE {:<12} {} -> {}", warehouse, from, to);
                        }
                        PlanAction::Noop {
                            warehouse,
                            replicas,
                        } => {
                            println!("  NOOP  {:<12} (replicas = {})", warehouse, replicas);
                        }
                    }
                }
            } else {
                let controller = KubeController::new(&namespace);
                let current_map = controller.observe_replicas(&specs);
                let plan = build_reconcile_plan(&specs, &current_map);

                println!("# Applying reconcile plan (namespace={})\n", namespace);
                let outcomes = controller.apply_plan(&plan);
                let mut has_failures = false;
                for outcome in outcomes {
                    match outcome {
                        ApplyOutcome::Scaled {
                            warehouse,
                            from,
                            to,
                        } => {
                            println!("  SCALED {:<12} {} -> {}", warehouse, from, to);
                        }
                        ApplyOutcome::Noop {
                            warehouse,
                            replicas,
                        } => {
                            println!("  NOOP   {:<12} (replicas = {})", warehouse, replicas);
                        }
                        ApplyOutcome::Failed { warehouse, error } => {
                            eprintln!("  FAILED {:<12} {}", warehouse, error);
                            has_failures = true;
                        }
                    }
                }
                if has_failures {
                    std::process::exit(1);
                }
            }
        }

        Commands::Status { host, port } => {
            let addr = format!("{host}:{port}");
            println!("Checking {addr}...");
            match std::net::TcpStream::connect_timeout(
                &addr.parse()?,
                std::time::Duration::from_secs(2),
            ) {
                Ok(_) => println!("OpenSnow server is reachable at {addr}"),
                Err(e) => eprintln!("Server unreachable at {addr}: {e}"),
            }
        }

        Commands::AccountRegister {
            account_name,
            owner_email,
        } => {
            let engine = create_engine(&config)?;
            let bootstrap = engine
                .catalog()
                .register_account(&account_name, &owner_email)?;
            println!("Registered enterprise account");
            println!("  account_id:       {}", bootstrap.account.id);
            println!("  organization_id:  {}", bootstrap.organization.id);
            println!("  workspace_id:     {}", bootstrap.workspace.id);
            println!("  owner_email:      {}", bootstrap.owner_membership.email);
            println!("  owner_role:       {}", bootstrap.owner_membership.role);
            println!("  service_identity: {}", bootstrap.service_identity.id);
        }

        Commands::Cli { command } => match command {
            CliCommands::Contract { format } | CliCommands::Doctor { format } => {
                print_cli_report(&config, &format)?;
            }
        },

        Commands::AccountWorkspaceCreate {
            account_id,
            name,
            actor_account_id,
        } => {
            let engine = create_engine(&config)?;
            let actor_account_id = actor_account_id.as_deref().unwrap_or(&account_id);
            let workspace = engine.catalog().create_workspace_for_account(
                actor_account_id,
                &account_id,
                &name,
            )?;
            println!("Created enterprise account workspace");
            println!("  account_id:      {}", workspace.account_id);
            println!("  organization_id: {}", workspace.organization_id);
            println!("  workspace_id:    {}", workspace.id);
            println!("  workspace_name:  {}", workspace.name);
        }
    }

    Ok(())
}
