//! Zero-config durable default database: a real native Postgres that Forge runs itself,
//! so a user need not bring their own. Gated behind the `embedded-postgres` feature and
//! selected with `[postgres] url = "embedded"`. Everything downstream — pooling, migrations,
//! `LISTEN`/`NOTIFY` — is unchanged, because this is genuine Postgres over the wire, not an
//! emulation.

use crate::error::{ForgeError, Result};
use postgresql_embedded::{PostgreSQL, Settings};
use std::path::{Path, PathBuf};

/// The application database Forge creates and uses inside the embedded server.
const EMBEDDED_DB: &str = "forge";

/// File inside the data directory holding the superuser password chosen at first init. The
/// crate generates a fresh random password on every construction, but the server bakes the
/// first one into the data directory, so a restart must reuse it or authentication fails.
const PASSWORD_FILE: &str = ".forge_superuser_password";

/// Optional path to a pre-installed Postgres (the directory whose `bin/` holds `initdb`,
/// `postgres`, `pg_ctl`, e.g. `/usr/lib/postgresql/16`). When set, Forge runs that build
/// instead of downloading one — the escape hatch for air-gapped hosts or networks that block
/// the binary download.
const INSTALL_DIR_ENV: &str = "FORGE_EMBEDDED_POSTGRES_DIR";

/// A running embedded Postgres owned by the Forge runtime. Holding this value keeps the
/// child process alive; dropping it stops the server (the crate's `Drop` runs a `pg_ctl
/// stop`). The data directory persists across restarts, so the default database is durable.
pub(crate) struct EmbeddedPostgres {
    pg: PostgreSQL,
}

impl EmbeddedPostgres {
    /// Download (once, then cached under `~/.theseus/postgresql`) and start a native Postgres
    /// under `data_dir`, then ensure the `forge` database exists. The server binds an
    /// OS-assigned free port (`port = 0`), so a busy 5432 — or any other in-use port — never
    /// collides.
    #[allow(clippy::field_reassign_with_default)]
    pub(crate) async fn start(data_dir: PathBuf) -> Result<Self> {
        let mut settings = Settings::default();
        settings.data_dir = data_dir;
        // Explicit even though 0 is the default: 0 means "pick a free port", the whole point
        // of running embedded without asking the operator to reserve one.
        settings.port = 0;
        // Durable: keep the data directory (and its contents) across restarts.
        settings.temporary = false;

        // Reuse the superuser password across restarts. On a first init the crate's random
        // password is written into the data directory by `initdb`; on later starts the crate
        // skips `initdb` (it keys off the data directory already existing), so we must present
        // that same password or the server rejects us. So: an existing data directory means a
        // prior init whose password we must reload; a missing one is a genuine first init where
        // we keep the generated password and persist it below. An existing directory with no
        // saved password is unmanaged or corrupt — fail clearly rather than initialising a
        // fresh password the server won't accept.
        let pw_file = settings.data_dir.join(PASSWORD_FILE);
        let first_init = !settings.data_dir.exists();
        if !first_init {
            settings.password = std::fs::read_to_string(&pw_file).map_err(|e| {
                ForgeError::config(format!(
                    "embedded postgres: data directory {} exists but its saved password at {} \
                     could not be read ({e}). If it is corrupt or was not created by Forge, \
                     delete it to re-initialise",
                    settings.data_dir.display(),
                    pw_file.display()
                ))
            })?;
        }

        // Keep the server's Unix socket inside the app-owned data directory. Postgres always
        // creates a socket even for TCP clients, and its compiled default (e.g.
        // `/var/run/postgresql`) may not be writable by this process; that would fail start-up
        // for a reason unrelated to the app. Connections still go over TCP on 127.0.0.1 at the
        // auto-assigned port — this only relocates the socket file.
        let socket_dir = settings.data_dir.to_string_lossy().into_owned();
        settings
            .configuration
            .insert("unix_socket_directories".to_string(), socket_dir);

        // Air-gap / restricted-network escape hatch: run a pre-installed Postgres instead of
        // downloading one. `trust_installation_dir` tells the crate to use the directory
        // as-is rather than treating it as a versioned download cache.
        if let Some(dir) = std::env::var_os(INSTALL_DIR_ENV) {
            settings.installation_dir = dir.into();
            settings.trust_installation_dir = true;
        }

        let password = settings.password.clone();
        let mut pg = PostgreSQL::new(settings);
        pg.setup().await.map_err(|e| {
            ForgeError::config(format!(
                "embedded postgres setup failed (downloading/initialising the server): {e}"
            ))
        })?;

        // `setup` just ran `initdb`, baking `password` into the fresh data directory. Persist
        // it now — before `start`, while the data directory is the only new state — so a later
        // restart authenticates even if start-up fails here.
        if first_init {
            persist_password(&pw_file, &password)?;
        }

        pg.start().await.map_err(|e| {
            ForgeError::config(format!("embedded postgres failed to start: {e}"))
        })?;

        // create_database errors if the database already exists, so probe first — the data
        // directory is reused on a restart and the database is already there.
        let exists = pg.database_exists(EMBEDDED_DB).await.map_err(|e| {
            ForgeError::config(format!("embedded postgres database probe failed: {e}"))
        })?;
        if !exists {
            pg.create_database(EMBEDDED_DB).await.map_err(|e| {
                ForgeError::config(format!(
                    "embedded postgres could not create the '{EMBEDDED_DB}' database: {e}"
                ))
            })?;
        }

        Ok(Self { pg })
    }

    /// The connection string for the `forge` database on this embedded server, to hand to
    /// the normal pool/connect path.
    pub(crate) fn url(&self) -> String {
        self.pg.settings().url(EMBEDDED_DB)
    }
}

/// Write the superuser password into the data directory, owner-readable only where the OS
/// supports it. It never leaves the local machine — the server listens only on 127.0.0.1.
fn persist_password(path: &Path, password: &str) -> Result<()> {
    std::fs::write(path, password).map_err(|e| {
        ForgeError::config(format!(
            "embedded postgres: could not save the superuser password to {}: {e}",
            path.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}
