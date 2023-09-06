use chrono::Utc;
use sqlx::{
    migrate::{AppliedMigration, Migrate, Migration, MigrationType, Migrator},
    PgConnection, PgPool,
};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Write,
    path::Path,
};
use tracing::{debug, error, info, instrument, warn};

mod error;

pub use error::Error;
use error::Result;

/// Create a new migration
#[instrument]
pub fn add(name: &str) -> Result<()> {
    let migrations = {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"));
        base.join("migrations")
    };

    let name = name.replace(' ', "_");
    create_file(&migrations, &name, MigrationType::ReversibleUp)?;
    create_file(&migrations, &name, MigrationType::ReversibleDown)?;

    Ok(())
}

fn create_file(source: &Path, name: &str, kind: MigrationType) -> Result<()> {
    let mut file_name = Utc::now().format("%Y%m%d%H%M%S").to_string();
    file_name.push('_');
    file_name.push_str(name);
    file_name.push_str(kind.suffix());

    let path = source.join(file_name);
    let mut file = File::create(&path)?;
    file.write_all(kind.file_content().as_bytes())?;

    info!(path = %path.display(), "created migration");

    Ok(())
}

/// Retrieve the current state of migrations
#[instrument(skip_all)]
pub async fn info(migrator: &Migrator, db: &PgPool) -> Result<()> {
    let mut conn = db.acquire().await?;

    if migrator.locking {
        conn.lock().await?;
    }

    ensure_migrations_table(&mut conn).await?;
    ensure_no_dirty_migrations(&mut conn, false).await?;

    // Don't use `list_applied_migrations` here as it performs extra validation we don't want
    let applied_migrations = conn
        .list_applied_migrations()
        .await?
        .into_iter()
        .map(|m| (m.version, m))
        .collect::<HashMap<_, _>>();

    info!(
        applied = applied_migrations.len(),
        total = migrator.migrations.len() / 2,
    );

    for migration in migrator.iter() {
        if migration.migration_type.is_down_migration() {
            debug!(version = migration.version, description = %migration.description, "skipping down migration");
            continue;
        }

        let applied = match applied_migrations.get(&migration.version) {
            Some(applied) => {
                if applied.checksum != migration.checksum {
                    warn!(
                        version = migration.version,
                        description = %migration.description,
                        applied = %hex::encode(&applied.checksum),
                        source = %hex::encode(&migration.checksum),
                        "applied checksum is different from source checksum",
                    );
                }

                true
            }
            None => false,
        };

        info!(version = migration.version, description = %migration.description, applied);
    }

    if migrator.locking {
        conn.unlock().await?;
    }

    Ok(())
}

/// Apply all pending migrations
#[instrument(skip_all)]
pub async fn apply(migrator: &Migrator, db: &PgPool) -> Result<()> {
    let mut conn = db.acquire().await?;

    if migrator.locking {
        conn.lock().await?;
    }

    ensure_migrations_table(&mut conn).await?;
    ensure_no_dirty_migrations(&mut conn, true).await?;

    let applied_migrations = list_applied_migrations(migrator, &mut conn)
        .await?
        .into_iter()
        .map(|m| m.version)
        .collect::<HashSet<_>>();

    info!(
        applied = applied_migrations.len(),
        total = migrator.migrations.len() / 2,
    );

    for migration in migrator.iter() {
        apply_migration(migration, &mut conn, &applied_migrations).await?;
    }

    if migrator.locking {
        conn.unlock().await?;
    }

    Ok(())
}

/// Apply a migration
#[instrument(skip_all, fields(version = migration.version, description = %migration.description))]
async fn apply_migration(
    migration: &Migration,
    conn: &mut PgConnection,
    applied_migrations: &HashSet<i64>,
) -> Result<()> {
    if migration.migration_type.is_down_migration() {
        debug!("skipping down migration");
        return Ok(());
    }

    if applied_migrations.contains(&migration.version) {
        debug!("skipping already applied migration");
        return Ok(());
    }

    let elapsed = conn.apply(migration).await?;
    info!(?elapsed, "applied migration");

    Ok(())
}

/// Undo migrations to the specified target
#[instrument(skip(migrator, db))]
pub async fn undo(migrator: &Migrator, db: &PgPool, target: Option<i64>) -> Result<()> {
    if let Some(target) = target {
        if target != 0 && !migrator.iter().any(|m| m.version == target) {
            return Err(Error::UnknownVersion(target));
        }
    }

    let mut conn = db.acquire().await?;

    if migrator.locking {
        conn.lock().await?;
    }

    ensure_migrations_table(&mut conn).await?;
    ensure_no_dirty_migrations(&mut conn, true).await?;

    let applied_migrations = list_applied_migrations(migrator, &mut conn).await?;

    let latest = applied_migrations
        .iter()
        .max_by(|a, b| a.version.cmp(&b.version))
        .map(|m| m.version)
        .unwrap_or(0);
    if let Some(target) = target {
        if target > latest {
            return Err(Error::VersionTooNew { target, latest });
        }
    }

    let applied_migrations = applied_migrations
        .into_iter()
        .map(|m| m.version)
        .collect::<HashSet<_>>();

    let mut is_applied = false;
    for migration in migrator.iter().rev() {
        if undo_migration(migration, &mut conn, &applied_migrations, target).await? {
            continue;
        }

        is_applied = true;

        // Only revert the latest migration if a target is not passed
        if target.is_none() {
            break;
        }
    }
    if !is_applied {
        info!("no migrations available to revert");
    }

    if migrator.locking {
        conn.unlock().await?;
    }

    Ok(())
}

/// Undo a migration. Returns whether the migration was skipped
#[instrument(skip_all, fields(version = migration.version, description = %migration.description))]
async fn undo_migration(
    migration: &Migration,
    conn: &mut PgConnection,
    applied_migrations: &HashSet<i64>,
    target: Option<i64>,
) -> Result<bool> {
    if !migration.migration_type.is_down_migration() {
        debug!("skipping up migration");
        return Ok(true);
    }

    if !applied_migrations.contains(&migration.version) {
        debug!("skipping unapplied migration");
        return Ok(true);
    }

    if matches!(target, Some(target) if migration.version <= target) {
        debug!("skipping migration older than target");
        return Ok(true);
    }

    let elapsed = conn.revert(migration).await?;
    info!(?elapsed, "reverted migration");

    Ok(false)
}

/// Ensure the migrations table exists
async fn ensure_migrations_table(conn: &mut PgConnection) -> Result<()> {
    conn.ensure_migrations_table().await?;
    debug!("migrations table created (if it does not already exist)");

    Ok(())
}

/// Ensure there are no dirty migrations. Only returns an error when a dirty migration is
/// encountered if `should_error` is true.
async fn ensure_no_dirty_migrations(conn: &mut PgConnection, should_error: bool) -> Result<()> {
    if let Some(version) = conn.dirty_version().await? {
        warn!(%version, "unsuccessful migration detected, cannot apply new migrations");

        if should_error {
            return Err(Error::Dirty(version));
        }
    }

    Ok(())
}

/// Get a list of all the applied migrations
async fn list_applied_migrations(
    migrator: &Migrator,
    conn: &mut PgConnection,
) -> Result<Vec<AppliedMigration>> {
    let applied_migrations = conn.list_applied_migrations().await?;
    validate_applied_migrations(migrator, &applied_migrations)?;

    Ok(applied_migrations)
}

/// Check the applied migrations still exist in the source migrations
fn validate_applied_migrations(
    migrator: &Migrator,
    applied_migrations: &[AppliedMigration],
) -> Result<()> {
    if migrator.ignore_missing {
        return Ok(());
    }

    let migrations = migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .map(|m| (m.version, m))
        .collect::<HashMap<_, _>>();

    for applied_migration in applied_migrations {
        match migrations.get(&applied_migration.version) {
            Some(migration) => {
                if migration.checksum != applied_migration.checksum {
                    error!(
                        version = applied_migration.version,
                        applied = %hex::encode(&applied_migration.checksum),
                        source = %hex::encode(&migration.checksum),
                        "checksum mismatch, migration was modified after being applied",
                    );
                    return Err(Error::VersionMismatch(applied_migration.version));
                }
            }
            None => {
                error!(
                    version = applied_migration.version,
                    "migration no longer exists in the source",
                );
                return Err(Error::MissingPreviouslyAppliedVersion(
                    applied_migration.version,
                ));
            }
        }
    }

    Ok(())
}
