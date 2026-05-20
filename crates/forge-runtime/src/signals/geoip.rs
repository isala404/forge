//! IP geolocation resolution.
//!
//! With the `geoip` feature: ships an embedded DB-IP Country Lite database for
//! zero-config country resolution, plus optional MaxMind GeoLite2-City MMDB for
//! city-level granularity.
//!
//! Without the `geoip` feature: provides a stub `GeoIpResolver` that returns
//! empty results, so signals callers compile without conditional code paths.

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

#[cfg(feature = "geoip")]
enum Backend {
    Embedded(db_ip::DbIpDatabase<db_ip::CountryCode>),
    Mmdb(maxminddb::Reader<Vec<u8>>),
}

#[cfg(not(feature = "geoip"))]
enum Backend {
    Stub,
}

/// Thread-safe GeoIP resolver. Uses the embedded DB-IP database by default,
/// or a MaxMind MMDB file when configured for city-level resolution.
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
    /// Create a resolver backed by the embedded DB-IP Country Lite database
    /// (or a no-op stub when the `geoip` feature is disabled).
    pub fn new() -> Self {
        #[cfg(feature = "geoip")]
        {
            Self {
                backend: Arc::new(Backend::Embedded(db_ip::include_country_code_database!())),
            }
        }
        #[cfg(not(feature = "geoip"))]
        {
            Self {
                backend: Arc::new(Backend::Stub),
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
            #[cfg(feature = "geoip")]
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
            #[cfg(not(feature = "geoip"))]
            Backend::Stub => GeoInfo::default(),
        }
    }
}
