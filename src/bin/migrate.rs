use clap::Parser;
use eyre::WrapErr;
use migrator::Migrator;
use sqlx::{
    postgres::{PgConnectOptions, PgPool},
    ConnectOptions,
};
use std::{path::PathBuf, str::FromStr};
use tracing::{info, instrument, log::LevelFilter, Level};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    dotenv()?;

    let config = Config::parse();
    logging::config()
        .default_directive(config.log_level)
        .init()?;

    let migrator = Migrator::new(config.source)
        .await
        .wrap_err("failed to load migrations")?;
    let db = connect_to_database(&config.database_url).await?;

    migrator::apply(&migrator, &db)
        .await
        .wrap_err("failed to apply migrations")
}

/// Ensure database schema migrations are up-to-date
#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Config {
    /// The default level to log at
    #[arg(short, long, default_value_t = Level::INFO, env = "LOG_LEVEL")]
    log_level: Level,

    /// The database to run migrations on
    #[arg(short, long, env = "DATABASE_URL")]
    database_url: String,

    /// Where the migrations are located
    #[arg(env = "MIGRATIONS_SOURCE")]
    source: PathBuf,
}

/// Load environment variables from a .env file, if it exists.
fn dotenv() -> eyre::Result<()> {
    if let Err(error) = dotenvy::dotenv() {
        if !error.not_found() {
            return Err(error).wrap_err("failed to load .env");
        }
    }

    Ok(())
}

/// Connect to the database and ensure it works
#[instrument(skip_all)]
async fn connect_to_database(url: &str) -> eyre::Result<PgPool> {
    let options = PgConnectOptions::from_str(url)
        .wrap_err("invalid database url format")?
        .log_statements(LevelFilter::Info);

    let db = PgPool::connect_with(options)
        .await
        .wrap_err("failed to connect to the database")?;

    info!("database connected");
    Ok(db)
}
