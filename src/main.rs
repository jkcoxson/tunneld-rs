// Jackson Coxson

use std::net::IpAddr;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Router,
};
use log::info;
use serde::Deserialize;

mod runner;

#[tokio::main]
async fn main() {
    env_logger::init();
    println!("Starting tunneld");
    info!("Initialized logger");

    let runner = runner::start_runner().await;

    let app = Router::new()
        // `GET /` goes to `root`
        .route("/", get(list_tunnels))
        .route("/cancel", get(cancel_tunnel))
        .route("/clear_tunnels", get(clear_tunnels))
        .route("/start_tunnel", get(start_tunnel))
        .with_state(runner);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:49151")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn list_tunnels(State(runner): State<runner::Runner>) -> (StatusCode, String) {
    if let Some(res) = runner.list_tunnels().await {
        (StatusCode::OK, res)
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "bad".to_string())
    }
}

#[derive(Deserialize)]
struct CancelTunnel {
    udid: String,
}

async fn cancel_tunnel(
    State(runner): State<runner::Runner>,
    params: Query<CancelTunnel>,
) -> (StatusCode, String) {
    if runner.cancel_tunnel(params.udid.to_string()).await {
        (StatusCode::OK, "ok".to_string())
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "bad".to_string())
    }
}

async fn clear_tunnels(State(runner): State<runner::Runner>) -> (StatusCode, String) {
    if runner.clear_tunnels().await {
        (StatusCode::OK, "ok".to_string())
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "bad".to_string())
    }
}

#[derive(Deserialize)]
struct StartTunnel {
    udid: String,
    ip: String,
}

async fn start_tunnel(
    State(runner): State<runner::Runner>,
    params: Query<StartTunnel>,
) -> (StatusCode, String) {
    let ip: IpAddr = match params.ip.parse() {
        Ok(i) => i,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid ip: {e:?}"));
        }
    };
    if runner.start_tunnel(params.udid.to_string(), ip).await {
        (StatusCode::OK, "ok".to_string())
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "bad".to_string())
    }
}
