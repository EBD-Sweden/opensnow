//! Worker-side Arrow Flight service + coordinator-side remote executor.
//!
//! ## What lives here
//!
//! * [`WorkerFlightService`] — the FlightService a worker pod runs so the
//!   coordinator can dispatch a partition to it. Implements `do_action` for
//!   the action `execute_fragment`.
//! * [`RemoteWorkerExecutor`] — coordinator-side adapter implementing
//!   [`WorkerExecutor`](crate::distributed_executor::WorkerExecutor). It
//!   serialises a [`PartitionedFragment`] as JSON, invokes the remote
//!   worker's `do_action`, and decodes the returned Arrow IPC stream back
//!   into `RecordBatch`es.
//! * [`encode_record_batches`] / [`decode_record_batches`] — Arrow IPC
//!   stream codec used as the on-the-wire result format. JSON would lose
//!   types; full Arrow Flight `do_get` would be more work than the MVP
//!   needs.
//!
//! ## Wire contract
//!
//! ```text
//!  Coordinator                                        Worker
//!  -----------                                        ------
//!  do_action(Action {                                 worker.engine.execute_sql(fragment.sql)
//!     type: "execute_fragment",         ─────────►    encode batches as Arrow IPC stream
//!     body: serde_json(PartitionedFragment) })        return WorkerFragmentResult { ipc_bytes, rows, error? }
//! ```

use std::io::Cursor;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use arrow::array::RecordBatch;
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::StreamWriter;
use arrow_flight::flight_service_client::FlightServiceClient;
use arrow_flight::flight_service_server::{FlightService, FlightServiceServer};
use arrow_flight::{
    Action, ActionType, Criteria, Empty, FlightData, FlightDescriptor, FlightInfo,
    HandshakeRequest, HandshakeResponse, PutResult, SchemaResult, Ticket,
};
use async_trait::async_trait;
use opensnow_core::EngineHandle;
use serde::{Deserialize, Serialize};
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

use crate::distributed_executor::WorkerExecutor;
use crate::partitioner::PartitionedFragment;

/// Action name used by the coordinator → worker dispatch.
pub const EXECUTE_FRAGMENT_ACTION: &str = "execute_fragment";

/// Result returned by a worker after running one fragment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerFragmentResult {
    pub partition_id: u32,
    pub rows: u64,
    /// Arrow IPC stream encoding of all produced `RecordBatch`es. Empty when
    /// the worker produced no rows.
    pub ipc_bytes: Vec<u8>,
    /// `Some` if the worker hit an error while executing — the coordinator
    /// surfaces it as a partition failure.
    pub error: Option<String>,
}

/// Encode a slice of `RecordBatch`es as an Arrow IPC stream.
///
/// Empty input collapses to an empty byte vector — the coordinator detects
/// "no rows" via `result.rows == 0` rather than parsing the (empty) stream.
pub fn encode_record_batches(batches: &[RecordBatch]) -> Result<Vec<u8>> {
    if batches.is_empty() {
        return Ok(Vec::new());
    }
    let schema = batches[0].schema();
    let mut buf = Vec::new();
    {
        let mut writer =
            StreamWriter::try_new(&mut buf, &schema).context("create Arrow IPC stream writer")?;
        for batch in batches {
            writer.write(batch).context("write batch to IPC stream")?;
        }
        writer.finish().context("finalise IPC stream")?;
    }
    Ok(buf)
}

/// Inverse of [`encode_record_batches`].
pub fn decode_record_batches(bytes: &[u8]) -> Result<Vec<RecordBatch>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let cursor = Cursor::new(bytes);
    let reader = StreamReader::try_new(cursor, None).context("create Arrow IPC stream reader")?;
    let mut out = Vec::new();
    for batch in reader {
        out.push(batch.context("decode IPC batch")?);
    }
    Ok(out)
}

// ── Worker side ──────────────────────────────────────────────────────────────

/// FlightService instance served by a worker pod.
pub struct WorkerFlightService {
    pub worker_id: String,
    pub engine: EngineHandle,
}

impl WorkerFlightService {
    pub fn new(worker_id: String, engine: EngineHandle) -> Self {
        Self { worker_id, engine }
    }

    /// Execute a single fragment and serialise the result.
    pub async fn execute_fragment_local(
        &self,
        fragment: &PartitionedFragment,
    ) -> WorkerFragmentResult {
        match self.engine.execute_sql(&fragment.sql).await {
            Ok(batches) => {
                let rows: u64 = batches.iter().map(|b| b.num_rows() as u64).sum();
                match encode_record_batches(&batches) {
                    Ok(ipc_bytes) => WorkerFragmentResult {
                        partition_id: fragment.partition_id,
                        rows,
                        ipc_bytes,
                        error: None,
                    },
                    Err(e) => WorkerFragmentResult {
                        partition_id: fragment.partition_id,
                        rows: 0,
                        ipc_bytes: Vec::new(),
                        error: Some(format!("encode: {e}")),
                    },
                }
            }
            Err(e) => WorkerFragmentResult {
                partition_id: fragment.partition_id,
                rows: 0,
                ipc_bytes: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    }
}

/// Spawn the worker's Flight server. Runs until shutdown or unrecoverable error.
pub async fn run_worker_grpc(service: Arc<WorkerFlightService>, port: u16) -> Result<()> {
    let addr = format!("0.0.0.0:{port}").parse()?;
    info!(worker_id = %service.worker_id, "Worker Flight service listening on {}", addr);
    let svc = WorkerFlightServiceWrapper { inner: service };
    Server::builder()
        .add_service(FlightServiceServer::new(svc))
        .serve(addr)
        .await
        .context("worker grpc serve")?;
    Ok(())
}

struct WorkerFlightServiceWrapper {
    inner: Arc<WorkerFlightService>,
}

#[tonic::async_trait]
impl FlightService for WorkerFlightServiceWrapper {
    type HandshakeStream =
        futures::stream::Once<futures::future::Ready<Result<HandshakeResponse, Status>>>;
    type ListFlightsStream = futures::stream::Empty<Result<FlightInfo, Status>>;
    type DoGetStream = futures::stream::Once<futures::future::Ready<Result<FlightData, Status>>>;
    type DoPutStream = futures::stream::Once<futures::future::Ready<Result<PutResult, Status>>>;
    type DoExchangeStream = futures::stream::Empty<Result<FlightData, Status>>;
    type DoActionStream =
        futures::stream::Once<futures::future::Ready<Result<arrow_flight::Result, Status>>>;
    type ListActionsStream = futures::stream::Empty<Result<ActionType, Status>>;

    async fn handshake(
        &self,
        _request: Request<Streaming<HandshakeRequest>>,
    ) -> Result<Response<Self::HandshakeStream>, Status> {
        let response = HandshakeResponse {
            protocol_version: 1,
            payload: bytes::Bytes::from("opensnow-worker"),
        };
        Ok(Response::new(futures::stream::once(futures::future::ok(
            response,
        ))))
    }

    async fn list_flights(
        &self,
        _request: Request<Criteria>,
    ) -> Result<Response<Self::ListFlightsStream>, Status> {
        Ok(Response::new(futures::stream::empty()))
    }

    async fn get_flight_info(
        &self,
        _request: Request<FlightDescriptor>,
    ) -> Result<Response<FlightInfo>, Status> {
        Err(Status::unimplemented("get_flight_info"))
    }

    async fn poll_flight_info(
        &self,
        _request: Request<FlightDescriptor>,
    ) -> Result<Response<arrow_flight::PollInfo>, Status> {
        Err(Status::unimplemented("poll_flight_info"))
    }

    async fn get_schema(
        &self,
        _request: Request<FlightDescriptor>,
    ) -> Result<Response<SchemaResult>, Status> {
        Err(Status::unimplemented("get_schema"))
    }

    async fn do_get(
        &self,
        _request: Request<Ticket>,
    ) -> Result<Response<Self::DoGetStream>, Status> {
        Err(Status::unimplemented("do_get"))
    }

    async fn do_put(
        &self,
        _request: Request<Streaming<FlightData>>,
    ) -> Result<Response<Self::DoPutStream>, Status> {
        Err(Status::unimplemented("do_put"))
    }

    async fn do_exchange(
        &self,
        _request: Request<Streaming<FlightData>>,
    ) -> Result<Response<Self::DoExchangeStream>, Status> {
        Err(Status::unimplemented("do_exchange"))
    }

    async fn do_action(
        &self,
        request: Request<Action>,
    ) -> Result<Response<Self::DoActionStream>, Status> {
        let action = request.into_inner();
        match action.r#type.as_str() {
            EXECUTE_FRAGMENT_ACTION => {
                let fragment: PartitionedFragment =
                    serde_json::from_slice(&action.body).map_err(|e| {
                        Status::invalid_argument(format!("decode PartitionedFragment: {e}"))
                    })?;
                let result = self.inner.execute_fragment_local(&fragment).await;
                if let Some(ref err) = result.error {
                    warn!(
                        worker_id = %self.inner.worker_id,
                        partition = result.partition_id,
                        "fragment failed: {err}"
                    );
                }
                let body = serde_json::to_vec(&result)
                    .map_err(|e| Status::internal(format!("encode WorkerFragmentResult: {e}")))?;
                let result = arrow_flight::Result {
                    body: bytes::Bytes::from(body),
                };
                Ok(Response::new(futures::stream::once(futures::future::ok(
                    result,
                ))))
            }
            other => Err(Status::unimplemented(format!(
                "Unknown worker action: {other}"
            ))),
        }
    }

    async fn list_actions(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::ListActionsStream>, Status> {
        Ok(Response::new(futures::stream::empty()))
    }
}

// ── Coordinator side ─────────────────────────────────────────────────────────

/// `WorkerExecutor` implementation that dispatches to a remote worker via
/// Arrow Flight `do_action`.
pub struct RemoteWorkerExecutor {
    /// `http://host:grpc_port` URL of the worker's Flight service.
    pub endpoint: String,
    /// Human label used by the coordinator's logs.
    pub label: String,
}

impl RemoteWorkerExecutor {
    pub fn new(endpoint: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            label: label.into(),
        }
    }
}

#[async_trait]
impl WorkerExecutor for RemoteWorkerExecutor {
    async fn execute_fragment(&self, fragment: &PartitionedFragment) -> Result<Vec<RecordBatch>> {
        let mut client = FlightServiceClient::connect(self.endpoint.clone())
            .await
            .with_context(|| format!("connect worker {}", self.endpoint))?;

        let body = serde_json::to_vec(fragment).context("encode fragment")?;
        let action = Action {
            r#type: EXECUTE_FRAGMENT_ACTION.to_string(),
            body: bytes::Bytes::from(body),
        };
        let mut stream = client
            .do_action(tonic::Request::new(action))
            .await
            .with_context(|| format!("do_action on {}", self.endpoint))?
            .into_inner();

        // The worker sends exactly one Result message — pull it.
        let next = stream
            .message()
            .await
            .with_context(|| format!("recv worker reply from {}", self.endpoint))?;
        let response = match next {
            Some(r) => r,
            None => bail!(
                "worker {} closed the stream without responding",
                self.endpoint
            ),
        };

        let result: WorkerFragmentResult =
            serde_json::from_slice(&response.body).context("decode WorkerFragmentResult")?;

        if let Some(err) = result.error {
            bail!(
                "worker {} fragment {} failed: {err}",
                self.endpoint,
                fragment.partition_id
            );
        }
        decode_record_batches(&result.ipc_bytes)
    }

    fn label(&self) -> &str {
        &self.label
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use arrow::array::Int64Array;
    use arrow::datatypes::{DataType, Field, Schema};

    fn sample_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
        RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(vec![1, 2, 3]))]).unwrap()
    }

    #[test]
    fn ipc_roundtrip_preserves_rows() {
        let batch = sample_batch();
        let bytes = encode_record_batches(std::slice::from_ref(&batch)).expect("encode");
        let back = decode_record_batches(&bytes).expect("decode");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].num_rows(), 3);
        assert_eq!(back[0].schema(), batch.schema());
    }

    #[test]
    fn ipc_roundtrip_handles_empty_input() {
        let bytes = encode_record_batches(&[]).expect("encode empty");
        assert!(bytes.is_empty());
        let back = decode_record_batches(&bytes).expect("decode empty");
        assert!(back.is_empty());
    }

    #[test]
    fn ipc_roundtrip_multiple_batches() {
        let b = sample_batch();
        let bytes = encode_record_batches(&[b.clone(), b.clone(), b.clone()]).expect("encode");
        let back = decode_record_batches(&bytes).expect("decode");
        assert_eq!(back.len(), 3);
        let total: usize = back.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 9);
    }

    #[test]
    fn worker_fragment_result_serialises_with_error() {
        let r = WorkerFragmentResult {
            partition_id: 1,
            rows: 0,
            ipc_bytes: Vec::new(),
            error: Some("boom".into()),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: WorkerFragmentResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.partition_id, 1);
        assert_eq!(back.error.as_deref(), Some("boom"));
    }

    #[test]
    fn remote_executor_label_round_trips() {
        let e = RemoteWorkerExecutor::new("http://localhost:9100", "w-test");
        assert_eq!(e.label(), "w-test");
        assert_eq!(e.endpoint, "http://localhost:9100");
    }
}
