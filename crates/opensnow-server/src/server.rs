use std::sync::Arc;

use anyhow::{Context, Result};
use opensnow_core::{EngineHandle, OpenSnowEngine};
use tokio::net::TcpListener;
use tracing::info;

use crate::pg::OpenSnowPgFactory;
use crate::rest;
use crate::telemetry;

fn should_bind_pgwire(pg_enabled: bool, auth_enabled: bool) -> bool {
    let _ = auth_enabled;
    pg_enabled
}

/// Build a tokio-rustls `TlsAcceptor` from PEM cert/key files for the pgwire
/// listener. Uses the same rustls/aws-lc-rs provider that pgwire and
/// axum-server compile in, so there is no process-level provider mismatch.
fn build_pg_tls_acceptor(
    cert_path: &str,
    key_path: &str,
) -> Result<tokio_rustls::TlsAcceptor> {
    use std::io::BufReader;
    use std::sync::Arc;

    let cert_file = std::fs::File::open(cert_path)
        .with_context_path("TLS cert", cert_path)?;
    let key_file = std::fs::File::open(key_path)
        .with_context_path("TLS key", key_path)?;

    let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("failed to parse pgwire TLS cert PEM: {e}"))?;
    if certs.is_empty() {
        anyhow::bail!("pgwire TLS cert file '{cert_path}' contained no certificates");
    }

    let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|e| anyhow::anyhow!("failed to parse pgwire TLS key PEM: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("pgwire TLS key file '{key_path}' contained no private key"))?;

    let config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| anyhow::anyhow!("invalid pgwire TLS cert/key: {e}"))?;

    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

/// Tiny helper to attach a path to file-open errors without pulling in extra deps.
trait ContextPath<T> {
    fn with_context_path(self, what: &str, path: &str) -> Result<T>;
}

impl<T> ContextPath<T> for std::io::Result<T> {
    fn with_context_path(self, what: &str, path: &str) -> Result<T> {
        self.with_context(|| format!("failed to open {what} file '{path}'"))
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost" | "[::1]")
}

fn env_allows_public() -> bool {
    std::env::var("OPENSNOW_ALLOW_PUBLIC")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Decide the address to bind to, refusing to expose an unauthenticated
/// listener on a non-loopback interface unless the operator explicitly opts in
/// with `OPENSNOW_ALLOW_PUBLIC=1`.
fn resolve_bind_host(configured_host: &str, auth_enabled: bool) -> Result<&str> {
    if auth_enabled || is_loopback_host(configured_host) || env_allows_public() {
        Ok(configured_host)
    } else {
        anyhow::bail!(
            "refusing to bind to non-loopback address '{configured_host}' with authentication disabled. \
             Enable auth by setting OPENSNOW_JWT_SECRET, bind to 127.0.0.1, or set OPENSNOW_ALLOW_PUBLIC=1 \
             to explicitly accept an unauthenticated public listener."
        )
    }
}

pub struct OpenSnowServer {
    engine: OpenSnowEngine,
    host: String,
    http_port: u16,
    pg_port: u16,
    pg_enabled: bool,
    /// Optional (cert_path, key_path) for in-process TLS on the HTTP listener.
    /// `None` keeps the plaintext default.
    tls: Option<(String, String)>,
}

impl OpenSnowServer {
    pub fn new(engine: OpenSnowEngine, http_port: u16, pg_port: u16) -> Self {
        Self::new_with_pgwire(engine, http_port, pg_port, false)
    }

    pub fn new_with_pgwire(
        engine: OpenSnowEngine,
        http_port: u16,
        pg_port: u16,
        pg_enabled: bool,
    ) -> Self {
        Self::new_with_options(engine, "127.0.0.1", http_port, pg_port, pg_enabled)
    }

    pub fn new_with_options(
        engine: OpenSnowEngine,
        host: impl Into<String>,
        http_port: u16,
        pg_port: u16,
        pg_enabled: bool,
    ) -> Self {
        Self {
            engine,
            host: host.into(),
            http_port,
            pg_port,
            pg_enabled,
            tls: None,
        }
    }

    /// Enable in-process TLS on the HTTP listener using PEM cert/key files.
    /// Builder-style and additive; without this call the listener is plaintext.
    pub fn with_tls(mut self, cert_path: impl Into<String>, key_path: impl Into<String>) -> Self {
        self.tls = Some((cert_path.into(), key_path.into()));
        self
    }

    pub fn pgwire_enabled(&self) -> bool {
        self.pg_enabled
    }

    pub async fn run(self) -> Result<()> {
        // Best-effort: install tracing + OpenTelemetry. `try_init` inside
        // `telemetry::init` is a no-op if a subscriber is already installed,
        // which is what we want when the CLI has already configured one.
        // Hold the guard for the lifetime of the server so spans flush on
        // shutdown.
        let _telemetry = telemetry::init("opensnow-server").ok();

        // Snapshot the warehouse before consuming the engine.
        let warehouse_path = self.engine.warehouse_path().to_string();

        // Both HTTP and PG share one engine worker via EngineHandle.
        let handle = EngineHandle::spawn(self.engine);

        // Streaming ingest: shared in-memory buffer + background compactor.
        let ingest_buffer = crate::ingest_buffer::IngestBuffer::shared(warehouse_path);
        crate::ingest_buffer::spawn_compactor(ingest_buffer.clone(), handle.clone());

        // Optional JWT auth — enabled when `OPENSNOW_JWT_SECRET` is set.
        let auth = crate::auth::AuthState::from_env();
        if auth.is_some() {
            info!("JWT auth enabled — protecting /api/v1/query, /ingest, /distributed_query");
        } else {
            info!("JWT auth disabled (set OPENSNOW_JWT_SECRET to enable)");
        }

        // Refuse to expose an unauthenticated listener on a public interface
        // unless the operator explicitly opted in. Resolved once and shared by
        // both the HTTP and PG listeners.
        let bind_host = resolve_bind_host(&self.host, auth.is_some())?.to_string();

        // Start HTTP/REST API
        let http_handle_clone = handle.clone();
        let http_port = self.http_port;
        let http_host = bind_host.clone();
        let http_auth = auth.clone();
        let http_buffer = ingest_buffer.clone();
        let http_tls = self.tls.clone();
        let http_task = tokio::spawn(async move {
            let app =
                rest::create_router_with_auth_and_buffer(http_handle_clone, http_auth, http_buffer);
            let addr: std::net::SocketAddr = format!("{http_host}:{http_port}")
                .parse()
                .expect("invalid HTTP bind address");
            match http_tls {
                Some((cert_path, key_path)) => {
                    // Opt-in rustls TLS termination in-process via axum-server.
                    let tls_config =
                        axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path)
                            .await
                            .expect("failed to load TLS cert/key (check [server.tls] paths)");
                    info!(
                        "REST API listening on https://{}:{} (TLS enabled)",
                        http_host, http_port
                    );
                    axum_server::bind_rustls(addr, tls_config)
                        .serve(app.into_make_service())
                        .await
                        .expect("HTTPS server failed");
                }
                None => {
                    let listener = TcpListener::bind(addr)
                        .await
                        .expect("Failed to bind HTTP port");
                    info!("REST API listening on http://{}:{}", http_host, http_port);
                    axum::serve(listener, app)
                        .await
                        .expect("HTTP server failed");
                }
            }
        });

        // PostgreSQL wire protocol is disabled by default for public demos. When
        // explicitly enabled, auth-enabled deployments require pgwire password
        // auth with a bearer JWT and apply the same tenant/policy/audit checks
        // before SQL execution; auth-disabled local runs remain trusted-local.
        let requested_pg_enabled = self.pg_enabled;
        let pg_enabled = should_bind_pgwire(requested_pg_enabled, auth.is_some());
        let pg_port = self.pg_port;
        let pg_task = if pg_enabled {
            let pg_handle_clone = handle.clone();
            let pg_auth = auth.clone();
            let pg_host = bind_host.clone();
            // Build the optional pgwire TLS acceptor up front so a bad cert/key
            // fails the server start rather than failing per-connection.
            let pg_tls_acceptor = match &self.tls {
                Some((cert, key)) => Some(Arc::new(build_pg_tls_acceptor(cert, key)?)),
                None => None,
            };
            Some(tokio::spawn(async move {
                let factory = Arc::new(OpenSnowPgFactory::new(pg_handle_clone, pg_auth));
                let listener = TcpListener::bind(format!("{pg_host}:{pg_port}"))
                    .await
                    .expect("Failed to bind PG port");
                if pg_tls_acceptor.is_some() {
                    info!(
                        "PostgreSQL protocol listening on {}:{} (TLS enabled)",
                        pg_host, pg_port
                    );
                } else {
                    info!("PostgreSQL protocol listening on {}:{}", pg_host, pg_port);
                }
                info!("Connect with: psql -h localhost -p {}", pg_port);

                loop {
                    match listener.accept().await {
                        Ok((socket, addr)) => {
                            let factory = factory.clone();
                            let tls = pg_tls_acceptor.clone();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    pgwire::tokio::process_socket(socket, tls, factory).await
                                {
                                    tracing::error!("PG connection error from {}: {}", addr, e);
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!("Failed to accept PG connection: {}", e);
                        }
                    }
                }
            }))
        } else {
            info!(
                "PostgreSQL wire protocol disabled; set [server].pg_enabled=true or --enable-pgwire to bind port {}",
                pg_port
            );
            None
        };

        println!(
            r#"
   ____                    _____
  / __ \___  ___ ___  / __/ _  ___  _    __
 / /_/ / _ \/ -_) _ \_\ \/ / _ \/ _ \ |/|/ /
 \____/ .__/\__/_//_/___/_/_//_/\___/__,__/
     /_/

  REST API:    http://localhost:{}
  PostgreSQL:  {}
  Status:      curl http://localhost:{}/health
"#,
            self.http_port,
            if pg_enabled {
                format!("psql -h localhost -p {pg_port}")
            } else {
                "disabled by default (use --enable-pgwire only for trusted local smoke tests)"
                    .to_string()
            },
            self.http_port
        );

        if let Some(pg_task) = pg_task {
            tokio::select! {
                r = http_task => { r?; }
                r = pg_task => { r?; }
            }
        } else {
            http_task.await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{is_loopback_host, resolve_bind_host, should_bind_pgwire};

    #[test]
    fn pgwire_binds_when_requested_in_local_or_enterprise_auth_mode() {
        assert!(!should_bind_pgwire(false, false));
        assert!(should_bind_pgwire(true, false));
        assert!(!should_bind_pgwire(false, true));
        assert!(should_bind_pgwire(true, true));
    }

    #[test]
    fn loopback_hosts_are_recognized() {
        for h in ["127.0.0.1", "::1", "localhost", "[::1]"] {
            assert!(is_loopback_host(h), "{h} should be loopback");
        }
        for h in ["0.0.0.0", "10.0.0.1", "192.168.1.5", "example.com"] {
            assert!(!is_loopback_host(h), "{h} should not be loopback");
        }
    }

    #[test]
    fn loopback_bind_is_allowed_without_auth() {
        assert_eq!(resolve_bind_host("127.0.0.1", false).unwrap(), "127.0.0.1");
    }

    #[test]
    fn public_bind_is_allowed_when_auth_is_enabled() {
        assert_eq!(resolve_bind_host("0.0.0.0", true).unwrap(), "0.0.0.0");
    }
}
