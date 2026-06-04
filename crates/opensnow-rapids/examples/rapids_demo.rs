//! Demo: RAPIDS GPU acceleration with graceful CPU fallback
//!
//! Run: cargo run --example rapids_demo

#[tokio::main]
async fn main() {
    // Check availability
    let config = opensnow_rapids::RapidsConfig::default();
    let backend = opensnow_rapids::RapidsBackend::new(config);

    if backend.is_available() {
        println!("✅ RAPIDS/cuDF is available — GPU acceleration enabled");
    } else {
        println!("⚠️  RAPIDS/cuDF not found — falling back to CPU (DataFusion)");
        println!("   Install with: pip install cudf-cu12 cuvs-cu12 pyarrow duckdb");
    }

    // Demo vector search (will use GPU if available, CPU fallback otherwise)
    let embeddings = vec![
        vec![0.1f32, 0.2, 0.3],
        vec![0.4, 0.5, 0.6],
        vec![0.7, 0.8, 0.9],
    ];
    let query = vec![0.15f32, 0.25, 0.35];

    match backend.vector_search(&embeddings, &query, 2).await {
        Ok(results) => {
            println!("Vector search top-2 results:");
            for (idx, score) in results {
                println!("  idx={} score={:.4}", idx, score);
            }
        }
        Err(opensnow_rapids::RapidsError::NotAvailable) => {
            println!("(vector search skipped — RAPIDS not available)");
        }
        Err(e) => eprintln!("Error: {}", e),
    }
}
