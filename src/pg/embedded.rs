//! Embedded Postgres provisioning for `[postgres] embedded = true`: a real server
//! binary, downloaded once per machine (to `~/.theseus/postgresql`) and booted per
//! process on a free port. Data and the generated superuser password persist under
//! the configured `embedded_dir`, so restarts keep state; the port changes per boot
//! but callers only ever see the minted DSN.

use crate::error::{ForgeError, Result};
use postgresql_embedded::{PostgreSQL, Settings, VersionReq};
use rand_core::RngCore;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

/// The Postgres major pinned for embedded servers. Kept on the oldest supported
/// release (see `MIN_SERVER_VERSION_NUM`), not the newest, so an embedded data
/// directory never needs a pg_upgrade when Forge updates.
const EMBEDDED_PG_VERSION: &str = "=17";

/// The database Forge creates and connects to inside the embedded server.
const EMBEDDED_DATABASE: &str = "forge";

/// Generous ceiling for individual server commands (initdb, pg_ctl start/stop):
/// first boot initializes a cluster, which can be slow on CI-grade disks. The
/// binary download is not bounded by this.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// Owns the running embedded server. Dropping it (with the last `Forge` clone)
/// stops the postmaster via `pg_ctl stop`; the data directory is kept.
pub(crate) struct EmbeddedPg {
    _server: PostgreSQL,
}

/// Boot (installing and initializing if needed) an embedded server rooted at `dir`
/// and return the handle plus the DSN of its `forge` database.
pub(crate) async fn start(dir: &Path) -> Result<(EmbeddedPg, String)> {
    let version = VersionReq::parse(EMBEDDED_PG_VERSION).map_err(|e| {
        ForgeError::config(format!(
            "invalid embedded postgres version requirement: {e}"
        ))
    })?;
    std::fs::create_dir_all(dir).map_err(|e| {
        ForgeError::config(format!(
            "could not create embedded postgres directory {}: {e}",
            dir.display()
        ))
    })?;

    let data_dir = dir.join("data");
    let password_file = dir.join("pwfile");
    let password = load_or_create_password(&password_file, &data_dir)?;

    let settings = Settings {
        version,
        data_dir,
        password_file,
        password,
        temporary: false,
        // 0 = pick a free port at boot; the DSN below carries whatever was chosen.
        port: 0,
        timeout: Some(COMMAND_TIMEOUT),
        ..Settings::default()
    };

    let mut server = PostgreSQL::new(settings);
    server.setup().await.map_err(|e| {
        ForgeError::config(format!(
            "could not install/initialize embedded postgres in {}: {e}",
            dir.display()
        ))
    })?;
    server.start().await.map_err(|e| {
        ForgeError::config(format!(
            "could not start embedded postgres in {dir}: {e}. If another process is \
             using this data directory, stop it; if a previous run crashed, delete \
             {dir} to reset (this discards the embedded data)",
            dir = dir.display()
        ))
    })?;

    let exists = server
        .database_exists(EMBEDDED_DATABASE)
        .await
        .map_err(|e| embedded_db_err(&server, e))?;
    if !exists {
        server
            .create_database(EMBEDDED_DATABASE)
            .await
            .map_err(|e| embedded_db_err(&server, e))?;
    }

    let url = server.settings().url(EMBEDDED_DATABASE);
    Ok((EmbeddedPg { _server: server }, url))
}

fn embedded_db_err(server: &PostgreSQL, e: postgresql_embedded::Error) -> ForgeError {
    ForgeError::config(format!(
        "could not prepare the '{EMBEDDED_DATABASE}' database on the embedded postgres \
         (data dir {}): {e}",
        server.settings().data_dir.display()
    ))
}

/// The superuser password: read back from `pwfile` on later boots (initdb bakes it
/// into the cluster, so a fresh random one would never match), generated and written
/// on the first.
fn load_or_create_password(password_file: &Path, data_dir: &Path) -> Result<String> {
    if password_file.exists() {
        let password = std::fs::read_to_string(password_file)
            .map_err(|e| {
                ForgeError::config(format!(
                    "could not read embedded postgres password file {}: {e}",
                    password_file.display()
                ))
            })?
            .trim()
            .to_string();
        if password.is_empty() {
            return Err(ForgeError::config(format!(
                "embedded postgres password file {} is empty; delete the embedded \
                 directory to reset (this discards the embedded data)",
                password_file.display()
            )));
        }
        return Ok(password);
    }
    // An initialized cluster without its password file is unrecoverable: the
    // password only exists inside the cluster. Fail with the way out.
    if data_dir.join("postgresql.conf").exists() {
        return Err(ForgeError::config(format!(
            "embedded postgres data directory {} is initialized but its password file \
             {} is missing; delete the embedded directory to reset (this discards the \
             embedded data)",
            data_dir.display(),
            password_file.display()
        )));
    }

    let mut bytes = [0u8; 24];
    rand_core::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|e| ForgeError::config(format!("could not generate a password: {e}")))?;
    let mut password = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // Writing into a String cannot fail.
        let _ = write!(password, "{b:02x}");
    }
    std::fs::write(password_file, &password).map_err(|e| {
        ForgeError::config(format!(
            "could not write embedded postgres password file {}: {e}",
            password_file.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(password_file, std::fs::Permissions::from_mode(0o600));
    }
    Ok(password)
}
