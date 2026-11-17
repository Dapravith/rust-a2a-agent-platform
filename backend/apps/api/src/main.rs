use std::process::ExitCode;

use api::{app, config, config::Config, db, engine::Engine};

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

    let result = match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve().await,
        Some("migrate") => migrate().await,
        Some(other) => Err(format!(
            "unknown command `{other}`; use `serve` (default) or `migrate`"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "api failed");
            ExitCode::FAILURE
        }
    }
}

async fn migrate() -> Result<(), String> {
    let url = config::migration_database_url()?;
    let pool = db::connect(&url, 1).await?;
    db::MIGRATOR
        .run(&pool)
        .await
        .map_err(|e| format!("migrations failed: {e}"))?;
    db::verify_schema(&pool).await?;
    tracing::info!(
        migrations = db::MIGRATOR.iter().count(),
        "database schema is up to date"
    );
    Ok(())
}

async fn serve() -> Result<(), String> {
    let cfg = Config::from_env()?;
    tracing::info!(config = ?cfg, "starting api");

    let pool = db::connect(&cfg.database_url, cfg.db_max_connections).await?;
    db::verify_schema(&pool).await?;
    // Held until the process exits: recovery below is only safe when no
    // other orchestrator is running against this database.
    let _instance = db::InstanceLock::acquire(&pool).await?;

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
