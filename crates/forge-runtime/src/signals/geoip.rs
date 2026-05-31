//! IP geolocation resolution.
//!
//! Two independent, additive backends, selected by cargo feature:
//!
//! - `geoip` (offline-safe): a runtime MaxMind MMDB reader. Load a
//!   GeoLite2-City database from a file via [`GeoIpResolver::from_mmdb`] for
//!   city-level granularity. Pulls only the pure-Rust `maxminddb` crate.
//! - `geoip-embedded`: additionally bakes a DB-IP Country Lite database into
//!   the binary for zero-config country resolution. This is the only option
//!   that needs a build-time network fetch (for the `db_ip` database).
//!
//! With neither feature, [`GeoIpResolver`] is a stub that returns empty results,
//! so signals callers compile without conditional code paths.

use forge_core::ForgeError;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

/// Resolved geolocation for a single IP address.
#[derive(Debug, Clone, Default)]
pub struct GeoInfo {
    /// ISO 3166-1 alpha-2 country code.
    pub country: Option<String>,
    /// Localized city name (English), when a city-level MMDB is configured.
    pub city: Option<String>,
}

enum Backend {
    /// Bundled DB-IP country database, compiled in via `geoip-embedded`.
    #[cfg(feature = "geoip-embedded")]
    Embedded(db_ip::DbIpDatabase<db_ip::CountryCode>),
    /// Runtime-loaded MaxMind MMDB reader (`geoip` + [`GeoIpResolver::from_mmdb`]).
    #[cfg(feature = "geoip")]
    Mmdb(maxminddb::Reader<Vec<u8>>),
    /// No data source: `geoip` enabled without an MMDB loaded, or the geoip
    /// features disabled entirely. Lookups return empty results.
    #[cfg(not(feature = "geoip-embedded"))]
    Empty,
}

/// Thread-safe GeoIP resolver. Backed by a runtime MaxMind MMDB file (the
/// `geoip` feature) for city-level resolution, the bundled DB-IP country
/// database (the `geoip-embedded` feature), or nothing when neither is enabled.
#[derive(Clone)]
pub struct GeoIpResolver {
    backend: Arc<Backend>,
}

impl Default for GeoIpResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl GeoIpResolver {
    /// Create a resolver backed by the bundled DB-IP Country Lite database when
    /// `geoip-embedded` is enabled, otherwise a no-data resolver (use
    /// [`Self::from_mmdb`] with the `geoip` feature for runtime city data).
    pub fn new() -> Self {
        #[cfg(feature = "geoip-embedded")]
        {
            Self {
                backend: Arc::new(Backend::Embedded(db_ip::include_country_code_database!())),
            }
        }
        #[cfg(not(feature = "geoip-embedded"))]
        {
            Self {
                backend: Arc::new(Backend::Empty),
            }
        }
    }

    /// Create a resolver backed by a MaxMind MMDB file (GeoLite2-City or similar).
    /// Only available with the `geoip` feature.
    #[cfg(feature = "geoip")]
    pub fn from_mmdb(path: &Path) -> Result<Self, ForgeError> {
        let reader = maxminddb::Reader::open_readfile(path).map_err(|e| {
            ForgeError::config_with(
                format!("failed to load GeoIP database {}", path.display()),
                e,
            )
        })?;
        Ok(Self {
            backend: Arc::new(Backend::Mmdb(reader)),
        })
    }

    /// Stub for `from_mmdb` when the `geoip` feature is off — always errors so
    /// operators discover the misconfiguration immediately at startup.
    #[cfg(not(feature = "geoip"))]
    pub fn from_mmdb(_path: &Path) -> Result<Self, ForgeError> {
        Err(ForgeError::config(
            "geoip MMDB support requires the `geoip` feature on forge-runtime",
        ))
    }

    /// Resolve an IP string to country and optionally city.
    pub fn lookup(&self, ip_str: &str) -> GeoInfo {
        let _ip: IpAddr = match ip_str.parse() {
            Ok(ip) => ip,
            Err(_) => return GeoInfo::default(),
        };

        match self.backend.as_ref() {
            #[cfg(feature = "geoip-embedded")]
            Backend::Embedded(db) => GeoInfo {
                country: db.get(&_ip).map(|c| c.as_str().to_string()),
                city: None,
            },
            #[cfg(feature = "geoip")]
            Backend::Mmdb(reader) => match reader.lookup(_ip) {
                Ok(lookup) => match lookup.decode::<maxminddb::geoip2::City>() {
                    Ok(Some(record)) => GeoInfo {
                        country: record.country.iso_code.map(|s| s.to_string()),
                        city: record.city.names.english.map(|s| s.to_string()),
                    },
                    _ => GeoInfo::default(),
                },
                Err(_) => GeoInfo::default(),
            },
            #[cfg(not(feature = "geoip-embedded"))]
            Backend::Empty => GeoInfo::default(),
        }
    }
}
