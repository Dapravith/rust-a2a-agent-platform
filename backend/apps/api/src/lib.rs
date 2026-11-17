//! API + orchestrator: validates requests, discovers trusted Agent Cards, runs the
//! Worker -> Reviewer workflow over A2A, and persists results.

pub mod a2a;
pub mod config;
pub mod db;
pub mod engine;
pub mod error_code;
pub mod routes;

use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

/// Full application router: `/api` plus (optionally) the static dashboard.
pub fn app(engine: engine::Engine, frontend_dir: Option<&std::path::Path>) -> Router {
    let router = routes::router(engine);
    let router = match frontend_dir {
        Some(dir) => router.fallback_service(
            ServeDir::new(dir).not_found_service(ServeFile::new(dir.join("404.html"))),
        ),
        None => router,
    };
    routes::harden(router)
}
