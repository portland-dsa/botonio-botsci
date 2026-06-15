//! The one-shot migration entry point.
//!
//! Migrations are applied by the binary's `migrate` subcommand in a dedicated
//! `systemd` `ExecStartPre=` phase under the migration role - never by the
//! long-running bot, which connects as a DML-only role with no DDL privilege. This
//! keeps the schema-change authority and the day-to-day query authority on separate
//! credentials.

use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;

use crate::PersistenceError;

/// Apply every pending migration embedded from `crates/persistence/migrations/`.
///
/// Idempotent: [`sqlx::migrate!`] records applied versions in a `_sqlx_migrations`
/// table and runs only those not yet seen, so repeated `ExecStartPre=` invocations
/// across restarts are safe. `pool` must authenticate as the migration role (the one
/// holding `CREATE`), not the runtime role.
///
/// # Errors
///
/// Surfaces a [`PersistenceError::Migrate`] if a migration fails to apply or the
/// recorded history diverges from the embedded set (e.g. a checksum mismatch).
pub async fn run_migrations(pool: &PgPool) -> Result<(), PersistenceError> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

/// Connect to the database as the migration role and apply every pending migration -
/// the production entry point for the `migrate` phase, so the connection details stay in
/// this crate rather than in the bot.
///
/// The connection is built from typed options rather than a DSN string: the migration
/// password is a random secret that can contain URL-significant characters (`/`, `+`,
/// `@`) a DSN would misparse. The migration role is `<db_name>_migrate` by convention
/// (the runtime role is `<db_name>_app`), and it connects over TCP (`host`/`port`) so its
/// DDL authority authenticates by scram, separate from the runtime's peer-over-socket
/// login. `host`/`port` default to loopback at the standard port at the call site, but are
/// passed in so a cluster listening elsewhere can be reached without a rebuild.
///
/// # Errors
///
/// Surfaces a [`PersistenceError`] if the connection fails or a migration cannot apply
/// (see [`run_migrations`]).
pub async fn connect_and_migrate(
    host: &str,
    port: u16,
    db_name: &str,
    password: &str,
) -> Result<(), PersistenceError> {
    let opts = PgConnectOptions::new()
        .host(host)
        .port(port)
        .database(db_name)
        .username(&format!("{db_name}_migrate"))
        .password(password);
    let pool = PgPool::connect_with(opts).await?;
    let result = run_migrations(&pool).await;
    pool.close().await;
    result
}
