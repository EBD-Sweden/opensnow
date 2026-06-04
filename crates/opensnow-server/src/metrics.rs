use std::sync::LazyLock;
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use prometheus::{
    Counter, CounterVec, Encoder, Gauge, GaugeVec, Histogram, HistogramOpts, Opts, Registry,
    TextEncoder,
};

/// Global metrics registry.
static REGISTRY: LazyLock<Registry> = LazyLock::new(create_metrics_registry);

static QUERIES_TOTAL_SUCCESS: LazyLock<Counter> = LazyLock::new(|| {
    Counter::with_opts(
        Opts::new("opensnow_queries_total", "Total number of queries executed")
            .const_label("status", "success"),
    )
    .expect("metric can be created")
});

static QUERIES_TOTAL_ERROR: LazyLock<Counter> = LazyLock::new(|| {
    Counter::with_opts(
        Opts::new("opensnow_queries_total", "Total number of queries executed")
            .const_label("status", "error"),
    )
    .expect("metric can be created")
});

static QUERY_DURATION_SECONDS: LazyLock<Histogram> = LazyLock::new(|| {
    Histogram::with_opts(
        HistogramOpts::new(
            "opensnow_query_duration_seconds",
            "Query execution duration in seconds",
        )
        .buckets(vec![0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0]),
    )
    .expect("metric can be created")
});

static ROWS_SCANNED_TOTAL: LazyLock<Counter> = LazyLock::new(|| {
    Counter::with_opts(Opts::new(
        "opensnow_rows_scanned_total",
        "Total number of rows scanned",
    ))
    .expect("metric can be created")
});

static BYTES_READ_TOTAL: LazyLock<Counter> = LazyLock::new(|| {
    Counter::with_opts(Opts::new(
        "opensnow_bytes_read_total",
        "Total number of bytes read",
    ))
    .expect("metric can be created")
});

static ACTIVE_CONNECTIONS: LazyLock<Gauge> = LazyLock::new(|| {
    Gauge::with_opts(Opts::new(
        "opensnow_active_connections",
        "Number of active connections",
    ))
    .expect("metric can be created")
});

static ACTIVE_QUERIES: LazyLock<Gauge> = LazyLock::new(|| {
    Gauge::with_opts(Opts::new(
        "opensnow_active_queries",
        "Number of currently executing queries",
    ))
    .expect("metric can be created")
});

static WAREHOUSE_STATUS: LazyLock<GaugeVec> = LazyLock::new(|| {
    GaugeVec::new(
        Opts::new(
            "opensnow_warehouse_status",
            "Status of warehouses (1 = active, 0 = inactive)",
        ),
        &["warehouse", "status"],
    )
    .expect("metric can be created")
});

/// Pending queries per warehouse. This is the primary scaling signal for KEDA
/// (see deploy/keda-scaledobject.yaml). Each in-flight query increments this
/// gauge when it starts and decrements when it finishes.
static WAREHOUSE_PENDING_QUERIES: LazyLock<GaugeVec> = LazyLock::new(|| {
    GaugeVec::new(
        Opts::new(
            "opensnow_warehouse_pending_queries",
            "Number of currently pending queries per warehouse",
        ),
        &["warehouse"],
    )
    .expect("metric can be created")
});

static WAREHOUSE_QUERIES_TOTAL: LazyLock<CounterVec> = LazyLock::new(|| {
    CounterVec::new(
        Opts::new(
            "opensnow_warehouse_queries_total",
            "Total queries routed per warehouse, labeled by query status",
        ),
        &["warehouse", "status"],
    )
    .expect("metric can be created")
});

static WAREHOUSE_CREDITS_ESTIMATE_TOTAL: LazyLock<CounterVec> = LazyLock::new(|| {
    CounterVec::new(
        Opts::new(
            "opensnow_warehouse_credits_estimate_total",
            "Estimated credits consumed per warehouse based on configured size and query wall time",
        ),
        &["warehouse", "size"],
    )
    .expect("metric can be created")
});

static CACHE_HIT_RATIO: LazyLock<Gauge> = LazyLock::new(|| {
    Gauge::with_opts(Opts::new(
        "opensnow_cache_hit_ratio",
        "Ratio of cache hits to total cache lookups",
    ))
    .expect("metric can be created")
});

static TABLE_COUNT: LazyLock<Gauge> = LazyLock::new(|| {
    Gauge::with_opts(Opts::new(
        "opensnow_table_count",
        "Number of tables registered in the catalog",
    ))
    .expect("metric can be created")
});

/// Number of warehouses currently in the running/active state. Increments
/// when a warehouse is resumed and decrements when it suspends.
static ACTIVE_WAREHOUSES: LazyLock<Gauge> = LazyLock::new(|| {
    Gauge::with_opts(Opts::new(
        "opensnow_active_warehouses",
        "Number of warehouses currently active (running)",
    ))
    .expect("metric can be created")
});

/// Create and return the global Prometheus metrics registry with all metrics
/// registered.
pub fn create_metrics_registry() -> Registry {
    let registry = Registry::new();

    registry
        .register(Box::new(QUERIES_TOTAL_SUCCESS.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(QUERIES_TOTAL_ERROR.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(QUERY_DURATION_SECONDS.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(ROWS_SCANNED_TOTAL.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(BYTES_READ_TOTAL.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(ACTIVE_CONNECTIONS.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(ACTIVE_QUERIES.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(WAREHOUSE_STATUS.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(WAREHOUSE_PENDING_QUERIES.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(WAREHOUSE_QUERIES_TOTAL.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(WAREHOUSE_CREDITS_ESTIMATE_TOTAL.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(CACHE_HIT_RATIO.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(TABLE_COUNT.clone()))
        .expect("collector can be registered");
    registry
        .register(Box::new(ACTIVE_WAREHOUSES.clone()))
        .expect("collector can be registered");

    registry
}

/// Axum handler that returns all metrics in Prometheus text exposition format.
pub async fn metrics_handler() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();

    match encoder.encode(&metric_families, &mut buffer) {
        Ok(()) => (
            StatusCode::OK,
            [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
            buffer,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to encode metrics: {e}"),
        )
            .into_response(),
    }
}

/// Record a completed query with its duration, status, and number of rows
/// scanned.
pub fn record_query(duration: Duration, status: &str, rows: u64) {
    QUERY_DURATION_SECONDS.observe(duration.as_secs_f64());
    ROWS_SCANNED_TOTAL.inc_by(rows as f64);

    match status {
        "success" => QUERIES_TOTAL_SUCCESS.inc(),
        "error" => QUERIES_TOTAL_ERROR.inc(),
        _ => QUERIES_TOTAL_ERROR.inc(),
    }
}

/// Increment the active connections gauge.
pub fn inc_connections() {
    ACTIVE_CONNECTIONS.inc();
}

/// Decrement the active connections gauge.
pub fn dec_connections() {
    ACTIVE_CONNECTIONS.dec();
}

/// Increment the active queries gauge.
pub fn inc_active_queries() {
    ACTIVE_QUERIES.inc();
}

/// Decrement the active queries gauge.
pub fn dec_active_queries() {
    ACTIVE_QUERIES.dec();
}

/// Record bytes read.
pub fn record_bytes_read(bytes: u64) {
    BYTES_READ_TOTAL.inc_by(bytes as f64);
}

/// Set the warehouse status gauge.
pub fn set_warehouse_status(warehouse: &str, status: &str, value: f64) {
    WAREHOUSE_STATUS
        .with_label_values(&[warehouse, status])
        .set(value);
}

/// Increment pending-query gauge for a given warehouse.
pub fn inc_warehouse_pending(warehouse: &str) {
    WAREHOUSE_PENDING_QUERIES
        .with_label_values(&[warehouse])
        .inc();
}

/// Decrement pending-query gauge for a given warehouse.
pub fn dec_warehouse_pending(warehouse: &str) {
    WAREHOUSE_PENDING_QUERIES
        .with_label_values(&[warehouse])
        .dec();
}

pub fn record_warehouse_usage(warehouse: &str, size: &str, status: &str, credits_estimate: f64) {
    WAREHOUSE_QUERIES_TOTAL
        .with_label_values(&[warehouse, status])
        .inc();
    if credits_estimate.is_finite() && credits_estimate > 0.0 {
        WAREHOUSE_CREDITS_ESTIMATE_TOTAL
            .with_label_values(&[warehouse, size])
            .inc_by(credits_estimate);
    }
}

/// Set the cache hit ratio gauge.
pub fn set_cache_hit_ratio(ratio: f64) {
    CACHE_HIT_RATIO.set(ratio);
}

/// Set the table count gauge.
pub fn set_table_count(count: f64) {
    TABLE_COUNT.set(count);
}

/// Increment the active-warehouses gauge (call when a warehouse resumes).
pub fn inc_active_warehouses() {
    ACTIVE_WAREHOUSES.inc();
}

/// Decrement the active-warehouses gauge (call when a warehouse suspends).
pub fn dec_active_warehouses() {
    ACTIVE_WAREHOUSES.dec();
}

/// Set the active-warehouses gauge to an absolute count (e.g. on startup
/// reconciliation).
pub fn set_active_warehouses(count: f64) {
    ACTIVE_WAREHOUSES.set(count);
}

/// Initialise the metrics registry. Call once at startup to ensure the lazy
/// statics are initialised.
pub fn init_metrics() {
    // Accessing the lazy static forces initialisation.
    let _ = &*REGISTRY;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_warehouses_gauge_tracks_inc_dec() {
        init_metrics();
        ACTIVE_WAREHOUSES.set(0.0);

        inc_active_warehouses();
        inc_active_warehouses();
        inc_active_warehouses();
        dec_active_warehouses();

        let metric_families = REGISTRY.gather();
        let mut value = None;
        for mf in metric_families {
            if mf.get_name() == "opensnow_active_warehouses" {
                if let Some(m) = mf.get_metric().first() {
                    value = Some(m.get_gauge().get_value());
                }
            }
        }
        assert_eq!(value, Some(2.0));
    }

    #[test]
    fn warehouse_pending_queries_increments_and_decrements() {
        // Ensure metrics are registered
        init_metrics();

        // Start from a clean gauge for this label
        let gauge = WAREHOUSE_PENDING_QUERIES.with_label_values(&["analytics"]);
        gauge.set(0.0);

        inc_warehouse_pending("analytics");
        inc_warehouse_pending("analytics");
        dec_warehouse_pending("analytics");

        // Value should be 1.0
        let metric_families = REGISTRY.gather();
        let mut value = None;
        for mf in metric_families {
            if mf.get_name() == "opensnow_warehouse_pending_queries" {
                for m in mf.get_metric() {
                    let labels = m.get_label();
                    if labels
                        .iter()
                        .any(|l| l.get_name() == "warehouse" && l.get_value() == "analytics")
                    {
                        value = Some(m.get_gauge().get_value());
                    }
                }
            }
        }

        assert_eq!(value, Some(1.0));
    }
}
