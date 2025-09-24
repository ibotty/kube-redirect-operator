mod controller;
mod metrics;
mod types;

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use axum_extra::{TypedHeader, headers::Host};
use kube::runtime::reflector;
use prometheus_client::encoding::text::encode;
use tokio::signal::{self, unix::SignalKind};
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::metrics::Metrics;

#[derive(Clone)]
struct AppState {
    store: reflector::Store<types::Redirect>,
    metrics: Arc<Metrics>,
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler")
    };
    let terminate = async {
        signal::unix::signal(SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {}
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // setup logging
    let logger = tracing_subscriber::fmt::layer().compact();
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();
    tracing_subscriber::Registry::default()
        .with(env_filter)
        .with(logger)
        .init();

    let kube_client = kube::Client::try_default().await?;
    let leader_handle = controller::setup_leader_election(kube_client.clone()).await?;
    let (reader, metrics, controller) =
        controller::get_controller(kube_client, leader_handle.state()).await?;

    let app_state = AppState {
        store: reader,
        metrics: metrics.clone(),
    };

    let app = Router::new()
        .route("/", get(redirect))
        .route("/{*path}", get(redirect))
        .with_state(app_state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    let webserver = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    let metrics_app = Router::new()
        .route("/ready", get(get_healthz))
        .route("/healthz", get(get_healthz))
        .route("/metrics", get(get_metrics))
        .with_state(metrics);
    let metrics_listener = tokio::net::TcpListener::bind("0.0.0.0:9880").await?;
    let metrics_server =
        axum::serve(metrics_listener, metrics_app).with_graceful_shutdown(shutdown_signal());

    let (_, r1, r2) = tokio::join!(controller, webserver, metrics_server);
    r1?;
    r2?;

    leader_handle.shutdown().await?;

    Ok(())
}

#[derive(Debug)]
struct NotFoundError {}

impl IntoResponse for NotFoundError {
    fn into_response(self) -> Response {
        todo!()
    }
}

#[axum::debug_handler]
async fn redirect(
    TypedHeader(host): TypedHeader<Host>,
    path: Option<Path<String>>,
    State(app_state): State<AppState>,
) -> Result<Response, NotFoundError> {
    let host = host.to_string();
    let host = host.trim_end_matches('.');
    let p = |redirect: &types::Redirect| redirect.spec.hosts.contains(host);
    if let Some(redirect) = app_state.store.find(p) {
        let to = &redirect.spec.to;
        let uri = if to.include_request_uri {
            let path = path.map(|p| p.0).unwrap_or("".to_string());
            format!("{}/{}", to.uri, path)
        } else {
            to.uri.clone()
        };

        info!("redirecting {} to {}", host, uri);
        app_state.metrics.http.set_request(host);
        Ok(Redirect::permanent(&uri).into_response())
    } else {
        error!("no redirect found for {}", host);
        app_state.metrics.http.set_failure(host);
        Err(NotFoundError {})
    }
}

async fn get_metrics(State(metrics): State<Arc<Metrics>>) -> Response {
    let mut buffer = String::new();
    encode(&mut buffer, &metrics.registry).unwrap();

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )
        .body(Body::from(buffer))
        .unwrap()
}

// this should check the reconcile loop, etc.
async fn get_healthz() -> Response {
    "OK\n".into_response()
}
