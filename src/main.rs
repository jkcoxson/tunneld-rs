// Jackson Coxson

use axum::{extract::State, http::StatusCode, routing::get, Router};
use log::info;

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
