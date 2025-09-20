use axum::{Json, Router, routing::get};
use serde_json::{Value, json};
use std::time::Instant;
use std::{
    env,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::signal;
use tracing::{Level, debug, error, info, instrument, warn};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, thiserror::Error)]
enum ServerError {
    #[error("Invalid HOST/PORT combination: {0}")]
    AddrParse(#[from] std::net::AddrParseError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Environment variable error: {0}")]
    Env(#[from] env::VarError),

    #[error("Invalid PORT value '{value}': {source}")]
    InvalidPort {
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },

    #[error("Failed to bind TCP listener on {addr}: {source}")]
    BindFailed {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("HTTP server error: {0}")]
    Serve(#[from] hyper::Error),
}

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    init_tracing()?;

    let addr = parse_server_address()?;

    let app = create_router();
    let listener = create_tcp_listener(addr).await?;

    info!("Server started and listening on http://{}", addr);

    serve_application(listener, app).await?;

    info!("Server shutdown complete");
    Ok(())
}

fn init_tracing() -> Result<(), ServerError> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(
            fmt::layer()
                .compact()
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true),
        )
        .init();

    Ok(())
}

fn parse_server_address() -> Result<SocketAddr, ServerError> {
    let host_raw = env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());

    let port_raw = env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let port: u16 = port_raw.parse().map_err(|e| ServerError::InvalidPort {
        value: port_raw.clone(),
        source: e,
    })?;

    match host_raw.parse::<IpAddr>() {
        Ok(ip) => Ok(SocketAddr::from((ip, port))),
        Err(_) => format!("{host_raw}:{port}")
            .parse()
            .map_err(ServerError::from),
    }
}

fn create_router() -> Router {
    Router::new()
        .route("/", get(root_handler))
        .route("/health", get(health_check_handler))
        .route("/ready", get(ready_check_handler))
}

async fn create_tcp_listener(addr: SocketAddr) -> Result<tokio::net::TcpListener, ServerError> {
    tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| ServerError::BindFailed { addr, source: e })
}

async fn serve_application(
    listener: tokio::net::TcpListener,
    app: Router,
) -> Result<(), ServerError> {
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServerError::from)
}

#[instrument(level = Level::DEBUG, name = "root_handler", skip_all)]
async fn root_handler() -> Json<Value> {
    let started = Instant::now();
    let ok = true;
    let status = 200;

    let body = json!({
        "message": "Hello, World!",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "version": env!("CARGO_PKG_VERSION"),
        "endpoint": "/"
    });

    info!(
        route = "/",
        method = "GET",
        ok,
        status,
        elapsed_us = started.elapsed().as_micros() as u64,
        response_size = body.to_string().len(),
        "Request completed successfully"
    );

    Json(body)
}

#[instrument(level = Level::INFO, name = "health_check_handler")]
async fn health_check_handler() -> axum::http::StatusCode {
    axum::http::StatusCode::OK
}

#[instrument(level = Level::INFO, name = "ready_check_handler")]
async fn ready_check_handler() -> Json<Value> {
    let started = Instant::now();

    // TODO: add readiness checks here
    let is_ready = true;

    debug!(
        route = "/ready",
        ok = is_ready,
        status = if is_ready { "ready" } else { "not_ready" },
        elapsed_us = started.elapsed().as_millis() as u64,
        "Readiness probe completed successfully"
    );

    Json(json!({
        "status": if is_ready { "ready" } else { "not_ready" },
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            error!("Failed to install Ctrl+C handler: {}", e);
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use signal::unix::{SignalKind, signal};

        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to install SIGTERM handler: {}", e);
                return std::future::pending::<()>().await;
            }
        };
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to install SIGINT handler: {}", e);
                return std::future::pending::<()>().await;
            }
        };

        tokio::select! {
            _ = sigterm.recv() => {},
            _ = sigint.recv() => {},
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    info!("Press Ctrl+C or send SIGTERM to shutdown...");

    tokio::select! {
        _ = ctrl_c => {
            info!("Ctrl+C received, initiating graceful shutdown");
        },
        _ = terminate => {
            info!("Terminate signal received, initiating graceful shutdown");
        },
    }

    tokio::time::sleep(Duration::from_secs(1)).await;
    warn!("Shutdown signal received, stopping server gracefully...");
}
