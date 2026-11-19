//! API + orchestrator: validates requests, discovers trusted Agent Cards, runs the
//! Worker -> Reviewer workflow over A2A, and persists results.

pub mod a2a;
pub mod config;
pub mod db;
pub mod eng;
pub mod engine;
pub mod error_code;
pub mod metrics;
pub mod routes;

use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

/// Full application router: `/api` plus (optionally) the static dashboard.
pub fn app(engine: engine::Engine, frontend_dir: Option<&std::path::Path>) -> Router {
    let eng = eng::routes::EngState {
        pool: engine.pool.clone(),
        orchestrator: None,
        auth: std::sync::Arc::new(eng::auth::Disabled),
        budget: engineering::state::Budget::default(),
    };
    app_with(engine, eng, frontend_dir)
}

/// [`app`] with an explicit engineering-workflow state (providers, identity).
pub fn app_with(
    engine: engine::Engine,
    eng: eng::routes::EngState,
    frontend_dir: Option<&std::path::Path>,
) -> Router {
    let eng_api = eng::routes::router(eng);
    let router = routes::router(engine)
        .nest("/api/eng", eng_api.clone())
        .nest("/api/v1/eng", eng_api);
    let router = match frontend_dir {
        Some(dir) => router.fallback_service(
            ServeDir::new(dir).not_found_service(ServeFile::new(dir.join("404.html"))),
        ),
        None => router,
    };
    // Route layer: runs after routing, so the matched route *template* is known.
    let router = router.route_layer(axum::middleware::from_fn(metrics::track));
    routes::harden(router)
}
