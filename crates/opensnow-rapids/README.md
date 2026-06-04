# opensnow-rapids

GPU-accelerated query execution and vector similarity search for OpenSnow, powered by [NVIDIA RAPIDS](https://rapids.ai/).

## What it does

- **SQL execution** — Runs SQL queries over Arrow record-batches via cuDF + DuckDB on the GPU.
- **Vector search** — Brute-force nearest-neighbour search using cuPy (with numpy fallback).

## Requirements

| Component | Version |
|-----------|---------|
| NVIDIA GPU | Compute Capability 7.0+ |
| CUDA | 12.0+ |
| Python 3 | 3.10+ |

Install the Python dependencies:

```bash
pip install cudf-cu12 cuvs-cu12 pyarrow duckdb numpy
```

## Usage

Add the dependency:

```toml
[dependencies]
opensnow-rapids = { path = "../opensnow-rapids" }
```

Create and use the backend:

```rust
let config = opensnow_rapids::RapidsConfig {
    enabled: true,
    ..Default::default()
};
let backend = opensnow_rapids::RapidsBackend::new(config);

if backend.is_available() {
    let results = backend.vector_search(&embeddings, &query, 10).await?;
}
```

## Fallback behaviour

If RAPIDS/cuDF is **not** installed the backend constructor detects this automatically:

- `RapidsBackend::is_available()` returns `false`.
- All methods return `Err(RapidsError::NotAvailable)`.
- The caller (e.g. the OpenSnow engine) should fall back to DataFusion or another CPU backend.

No panic, no crash — just a clean error the caller can match on.

## Configuration

```toml
[rapids]
enabled = true
python_bin = "python3"
gpu_memory_limit_mb = 4096
fallback_to_cpu = true
```

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `false` | Master switch for GPU acceleration |
| `python_bin` | `"python3"` | Path to the Python interpreter with RAPIDS installed |
| `gpu_memory_limit_mb` | `4096` | GPU memory budget (advisory) |
| `fallback_to_cpu` | `true` | Whether the caller should fall back on `NotAvailable` |
