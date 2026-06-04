use std::sync::Arc;

use anyhow::Result;
use arrow::array::RecordBatch;
use arrow_flight::flight_service_server::{FlightService, FlightServiceServer};
use arrow_flight::{
    Action, ActionType, Criteria, Empty, FlightData, FlightDescriptor, FlightInfo,
    HandshakeRequest, HandshakeResponse, PutResult, SchemaResult, Ticket,
};
use opensnow_core::{EngineHandle, OpenSnowEngine};
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};
use tracing::info;

use crate::protocol::*;
use crate::scheduler::Scheduler;

/// The coordinator is the entry point for distributed queries.
/// It manages workers, plans queries, and distributes execution.
pub struct Coordinator {
    handle: EngineHandle,
    scheduler: Arc<Scheduler>,
    grpc_port: u16,
}

impl Coordinator {
    pub fn new(engine: OpenSnowEngine, grpc_port: u16) -> Self {
        Self {
            handle: EngineHandle::spawn(engine),
            scheduler: Arc::new(Scheduler::new()),
            grpc_port,
        }
    }

    pub fn scheduler(&self) -> &Arc<Scheduler> {
        &self.scheduler
    }

    /// Execute a query, distributing to workers via scatter-gather if any
    /// workers are available, otherwise running locally on the coordinator.
    ///
    /// See [`crate::distributed_executor`] for the full scatter-gather
    /// protocol. The Coordinator currently uses [`LocalWorkerExecutor`]
    /// instances bound to its own engine handle for each registered worker
    /// — this exercises the full split/dispatch/merge path while keeping
    /// the wire transport pluggable for a follow-up Arrow Flight executor.
    pub async fn execute_query(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        let workers = self.scheduler.get_available_workers().await;

        if workers.is_empty() {
            info!("No workers available, executing locally");
            let batches = self
                .handle
                .execute_sql(sql)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            return Ok(batches);
        }

        info!(
            "Distributing query to {} worker(s) via scatter-gather",
            workers.len()
        );

        // Build one executor per registered worker. We use the coordinator's
        // local engine handle as the underlying runner — replacing this with
        // a Flight-based executor is a drop-in change once `do_get` is
        // implemented for the worker side.
        let executors: Vec<Arc<dyn crate::distributed_executor::WorkerExecutor>> = workers
            .iter()
            .map(|w| {
                Arc::new(
                    crate::distributed_executor::LocalWorkerExecutor::with_label(
                        self.handle.clone(),
                        format!("worker:{}", w.registration.worker_id),
                    ),
                ) as Arc<dyn crate::distributed_executor::WorkerExecutor>
            })
            .collect();

        let executor = crate::distributed_executor::DistributedExecutor::new(executors);
        executor.execute(sql).await
    }

    /// Start the coordinator's gRPC service for worker communication.
    pub async fn start_grpc(self: Arc<Self>) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.grpc_port).parse()?;
        let service = CoordinatorFlightService {
            coordinator: self.clone(),
        };

        info!("Coordinator gRPC listening on {}", addr);

        Server::builder()
            .add_service(FlightServiceServer::new(service))
            .serve(addr)
            .await?;

        Ok(())
    }

    /// Register a worker with the coordinator.
    pub async fn register_worker(&self, reg: WorkerRegistration) {
        self.scheduler.register_worker(reg).await;
    }

    /// Get cluster status.
    pub async fn cluster_state(&self) -> ClusterState {
        self.scheduler.cluster_state().await
    }
}

/// Arrow Flight service implementation for coordinator.
/// Workers use this to register, send heartbeats, and exchange data.
struct CoordinatorFlightService {
    coordinator: Arc<Coordinator>,
}

#[tonic::async_trait]
impl FlightService for CoordinatorFlightService {
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
            payload: bytes::Bytes::from("opensnow-coordinator"),
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
            "register_worker" => {
                let reg: WorkerRegistration = serde_json::from_slice(&action.body)
                    .map_err(|e| Status::invalid_argument(e.to_string()))?;
                self.coordinator.register_worker(reg).await;
                let result = arrow_flight::Result {
                    body: bytes::Bytes::from("registered"),
                };
                Ok(Response::new(futures::stream::once(futures::future::ok(
                    result,
                ))))
            }
            "heartbeat" => {
                let hb: Heartbeat = serde_json::from_slice(&action.body)
                    .map_err(|e| Status::invalid_argument(e.to_string()))?;
                self.coordinator.scheduler.heartbeat(hb).await;
                let result = arrow_flight::Result {
                    body: bytes::Bytes::from("ok"),
                };
                Ok(Response::new(futures::stream::once(futures::future::ok(
                    result,
                ))))
            }
            "cluster_state" => {
                let state = self.coordinator.cluster_state().await;
                let json =
                    serde_json::to_vec(&state).map_err(|e| Status::internal(e.to_string()))?;
                let result = arrow_flight::Result {
                    body: bytes::Bytes::from(json),
                };
                Ok(Response::new(futures::stream::once(futures::future::ok(
                    result,
                ))))
            }
            _ => Err(Status::unimplemented(format!(
                "Unknown action: {}",
                action.r#type
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
