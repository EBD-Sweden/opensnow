//! Python subprocess bridge for RAPIDS/cuDF operations.
//!
//! Embeds `helper.py` at compile time and executes it via `tokio::process`
//! to perform GPU-accelerated SQL and vector search operations.

use std::io::Write as _;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Embedded Python helper script for RAPIDS/cuDF bridge.
const HELPER_PY: &str = include_str!("helper.py");

/// Execute a SQL query via the Python bridge.
///
/// `ipc_bytes` should contain a single Arrow IPC stream representing the
/// input table.  Returns the result as Arrow IPC bytes.
pub async fn run_sql(
    python_bin: &str,
    sql: &str,
    ipc_bytes: &[u8],
) -> Result<Vec<u8>, super::RapidsError> {
    // Write the helper script to a temporary file.
    let tmp = tempfile_helper()?;
    let tmp_path = tmp.path().to_path_buf();

    // Build the JSON command line.
    let cmd_json = serde_json::json!({ "cmd": "sql", "sql": sql }).to_string();

    let mut child = Command::new(python_bin)
        .arg(&tmp_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(super::RapidsError::Io)?;

    // Write command JSON + newline, then raw IPC bytes.
    {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        stdin
            .write_all(cmd_json.as_bytes())
            .await
            .map_err(super::RapidsError::Io)?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(super::RapidsError::Io)?;
        stdin
            .write_all(ipc_bytes)
            .await
            .map_err(super::RapidsError::Io)?;
        // Drop stdin to signal EOF.
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(super::RapidsError::Io)?;

    // Clean up temp file (best-effort).
    let _ = std::fs::remove_file(&tmp_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(super::RapidsError::BridgeError(stderr.into_owned()));
    }

    Ok(output.stdout)
}

/// Perform a brute-force vector similarity search via the Python bridge.
///
/// Returns `(index, score)` pairs sorted by descending similarity.
pub async fn run_vector_search(
    python_bin: &str,
    embeddings: &[Vec<f32>],
    query: &[f32],
    top_k: usize,
) -> Result<Vec<(usize, f32)>, super::RapidsError> {
    let tmp = tempfile_helper()?;
    let tmp_path = tmp.path().to_path_buf();

    let cmd_json = serde_json::json!({ "cmd": "vector_search", "top_k": top_k }).to_string();
    let embeddings_json = serde_json::to_string(embeddings)
        .map_err(|e| super::RapidsError::BridgeError(e.to_string()))?;
    let query_json =
        serde_json::to_string(query).map_err(|e| super::RapidsError::BridgeError(e.to_string()))?;

    let mut child = Command::new(python_bin)
        .arg(&tmp_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(super::RapidsError::Io)?;

    {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        stdin
            .write_all(cmd_json.as_bytes())
            .await
            .map_err(super::RapidsError::Io)?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(super::RapidsError::Io)?;
        stdin
            .write_all(embeddings_json.as_bytes())
            .await
            .map_err(super::RapidsError::Io)?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(super::RapidsError::Io)?;
        stdin
            .write_all(query_json.as_bytes())
            .await
            .map_err(super::RapidsError::Io)?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(super::RapidsError::Io)?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(super::RapidsError::Io)?;

    let _ = std::fs::remove_file(&tmp_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(super::RapidsError::BridgeError(stderr.into_owned()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw: Vec<Vec<f64>> = serde_json::from_str(stdout.trim())
        .map_err(|e| super::RapidsError::BridgeError(format!("JSON parse error: {e}")))?;

    let results: Vec<(usize, f32)> = raw
        .into_iter()
        .map(|pair| (pair[0] as usize, pair[1] as f32))
        .collect();

    Ok(results)
}

/// Write the embedded helper script to a temporary file and return a handle.
///
/// The caller is responsible for cleaning up.
struct TempFile {
    path: std::path::PathBuf,
}

impl TempFile {
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

fn tempfile_helper() -> Result<TempFile, super::RapidsError> {
    let dir = std::env::temp_dir();
    let name = format!("opensnow_rapids_{}.py", std::process::id());
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).map_err(super::RapidsError::Io)?;
    f.write_all(HELPER_PY.as_bytes())
        .map_err(super::RapidsError::Io)?;
    Ok(TempFile { path })
}
