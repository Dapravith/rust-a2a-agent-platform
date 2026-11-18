use std::process::ExitCode;

use api::{config, config::Config, db, engine::Engine};

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
    // Phase 6: in-flight engineering stages fail as `interrupted`; workflows
    // waiting for a human decision are preserved as they are.
    let eng_recovered = api::eng::store::recover_interrupted(&pool)
        .await
        .map_err(|e| e.to_string())?;
    if eng_recovered > 0 {
        tracing::warn!(
            eng_recovered,
            "marked interrupted engineering workflows as failed"
        );
    }
    // Real providers come only from ENG_PROVIDER; simulation is never wired
    // here. No identity provider is integrated yet, so the authenticator is
    // `Disabled` and every `/api/eng` write answers 503 (BLOCKED).
    let providers = api::eng::providers::from_env()?;
    let orchestrator = providers.map(|p| {
        let mut o = api::eng::orchestrator::Orchestrator::with_agents(pool.clone(), p.agents);
        o.base_commit_sha = p.base_commit_sha;
        o.mcp_control = p.mcp_control;
        for (role, id) in o.identities() {
            tracing::info!(role = role.as_str(), provider = %id.provider, model = ?id.model, "engineering agent configured");
        }
        std::sync::Arc::new(o)
    });
    let eng = api::eng::routes::EngState {
        pool: pool.clone(),
        orchestrator,
        auth: std::sync::Arc::new(api::eng::auth::Disabled),
        budget: engineering::state::Budget::default(),
    };

    let engine = Engine::new(pool, &cfg)?;
    let listener = tokio::net::TcpListener::bind(&cfg.bind)
        .await
        .map_err(|e| format!("cannot bind {}: {e}", cfg.bind))?;
    tracing::info!(bind = %cfg.bind, "api listening");
    axum::serve(
        listener,
        api::app_with(engine, eng, cfg.frontend_dir.as_deref()),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|e| format!("server error: {e}"))
}
