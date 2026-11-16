use std::process::ExitCode;

use api::{app, config::Config, db, engine::Engine};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> ExitCode {
    let json_logs = std::env::var("LOG_FORMAT").is_ok_and(|v| v == "json");
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if json_logs {
        builder.json().init();
    } else {
        builder.init();
    }

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "api failed");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let cfg = Config::from_env()?;
    tracing::info!(config = ?cfg, "starting api");

    let pool = PgPoolOptions::new()
        .max_connections(cfg.db_max_connections)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&cfg.database_url)
        .await
        .map_err(|e| format!("cannot connect to database: {e}"))?;
    db::MIGRATOR
        .run(&pool)
        .await
        .map_err(|e| format!("migrations failed: {e}"))?;

    let recovered = db::recover_interrupted(&pool)
        .await
        .map_err(|e| e.to_string())?;
    if recovered > 0 {
        tracing::warn!(recovered, "marked interrupted workflows as failed");
    }

    let engine = Engine::new(pool, &cfg)?;
    let listener = tokio::net::TcpListener::bind(&cfg.bind)
        .await
        .map_err(|e| format!("cannot bind {}: {e}", cfg.bind))?;
    tracing::info!(bind = %cfg.bind, "api listening");
    axum::serve(listener, app(engine, cfg.frontend_dir.as_deref()))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| format!("server error: {e}"))
}
