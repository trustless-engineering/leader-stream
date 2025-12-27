use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use flate2::read::GzDecoder;
use maxminddb::geoip2::City;
use maxminddb::{MaxMindDBError, Reader};
use reqwest::Client;
use tar::Archive;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::Config;

#[derive(Clone, Debug)]
pub(crate) struct GeoPoint {
    pub(crate) latitude: f64,
    pub(crate) longitude: f64,
    pub(crate) city: Option<String>,
    pub(crate) country: Option<String>,
}

#[derive(Clone)]
pub(crate) struct GeoIpService {
    reader: Option<Arc<Reader<Vec<u8>>>>,
    cache: Arc<RwLock<HashMap<String, Option<GeoPoint>>>>,
    lookup_error_logged: Arc<AtomicBool>,
}

impl GeoIpService {
    pub(crate) fn from_reader(reader: Reader<Vec<u8>>) -> Self {
        Self {
            reader: Some(Arc::new(reader)),
            cache: Arc::new(RwLock::new(HashMap::new())),
            lookup_error_logged: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_static(entries: HashMap<String, Option<GeoPoint>>) -> Self {
        Self {
            reader: None,
            cache: Arc::new(RwLock::new(entries)),
            lookup_error_logged: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) async fn lookup(&self, ip: &str) -> Option<GeoPoint> {
        if ip.is_empty() {
            return None;
        }

        {
            let cache = self.cache.read().await;
            if let Some(result) = cache.get(ip) {
                return result.clone();
            }
        }

        let ip_addr: IpAddr = match ip.parse() {
            Ok(addr) => addr,
            Err(_) => {
                self.cache_write(ip, None).await;
                return None;
            }
        };

        let reader = match self.reader.as_ref() {
            Some(reader) => reader,
            None => {
                self.cache_write(ip, None).await;
                return None;
            }
        };

        let result = match reader.lookup::<City>(ip_addr) {
            Ok(city) => extract_point(&city),
            Err(err) => {
                if !matches!(err, MaxMindDBError::AddressNotFoundError(_)) {
                    self.log_lookup_error_once(err);
                }
                None
            }
        };
        self.cache_write(ip, result.clone()).await;
        result
    }

    async fn cache_write(&self, ip: &str, value: Option<GeoPoint>) {
        let mut cache = self.cache.write().await;
        cache.insert(ip.to_string(), value);
    }

    fn log_lookup_error_once(&self, err: MaxMindDBError) {
        if !self.lookup_error_logged.swap(true, Ordering::SeqCst) {
            warn!(
                ?err,
                "MaxMind database lookup failed; geolocation data will be empty"
            );
        }
    }
}

pub(crate) async fn load_geoip(config: &Config) -> Result<GeoIpService> {
    let path = resolve_database_path(config)?;
    if !path.exists() {
        info!(
            "MaxMind database not found at {}; downloading",
            path.display()
        );
        download_database(config, &path).await?;
    }
    match fs::metadata(&path) {
        Ok(metadata) => {
            let size = metadata.len();
            info!(
                "MaxMind database ready path={} size_bytes={}",
                path.display(),
                size
            );
            if size < 1_000_000 {
                warn!(
                    "MaxMind database appears unusually small; likely a test DB and lookups may fail"
                );
            }
        }
        Err(err) => {
            warn!(
                ?err,
                "failed to read MaxMind database metadata at {}",
                path.display()
            );
        }
    };
    let reader = Reader::open_readfile(&path)
        .with_context(|| format!("failed to open MaxMind database at {}", path.display()))?;
    info!(
        database_type = %reader.metadata.database_type,
        build_epoch = reader.metadata.build_epoch,
        ip_version = reader.metadata.ip_version,
        node_count = reader.metadata.node_count,
        "MaxMind database metadata loaded"
    );
    if !reader.metadata.database_type.to_lowercase().contains("city") {
        warn!(
            database_type = %reader.metadata.database_type,
            "MaxMind database type does not look like a City database; geolocation fields may be empty"
        );
    }
    Ok(GeoIpService::from_reader(reader))
}

fn resolve_database_path(config: &Config) -> Result<PathBuf> {
    let path = PathBuf::from(config.maxmind_db_path.clone());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create database directory {}", parent.display()))?;
    }
    Ok(path)
}

async fn download_database(config: &Config, target: &Path) -> Result<()> {
    let timeout = std::cmp::min(config.request_timeout, Duration::from_secs(5));
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .context("failed to build HTTP client for database download")?;

    if let Some(url) = config.maxmind_db_download_url.as_ref() {
        if let Err(err) = fetch_and_write(&client, url, target, true).await {
            warn!(
                ?err,
                "failed to download MaxMind database from MAXMIND_DB_DOWNLOAD_URL"
            );
        } else {
            info!("downloaded MaxMind database from custom URL");
            return Ok(());
        }
    }

    if let Some(key) = config.maxmind_license_key.as_ref() {
        let url = format!("https://download.maxmind.com/app/geoip_download?edition_id={}&license_key={}&suffix=tar.gz", config.maxmind_edition_id, key);
        if let Err(err) = fetch_and_write(&client, &url, target, false).await {
            warn!(?err, "failed to download MaxMind database with license key");
        } else {
            info!("downloaded MaxMind database using license key");
            return Ok(());
        }
    }

    let url = config
        .maxmind_fallback_url
        .as_deref()
        .unwrap_or("https://raw.githubusercontent.com/maxmind/MaxMind-DB/main/test-data/GeoLite2-City-Test.mmdb");
    fetch_and_write(&client, url, target, true)
        .await
        .context("failed to download fallback MaxMind database")
}

async fn fetch_and_write(client: &Client, url: &str, target: &Path, raw_mmdb: bool) -> Result<()> {
    let response = client
        .get(url)
        .send()
        .await
        .context("database request failed")?
        .error_for_status()
        .context("database request returned error status")?;

    let bytes = response
        .bytes()
        .await
        .context("failed to read database body")?;

    if raw_mmdb {
        if url.ends_with(".gz") {
            let mut decoder = GzDecoder::new(Cursor::new(bytes));
            let mut buf = Vec::new();
            decoder
                .read_to_end(&mut buf)
                .context("failed to decompress database")?;
            fs::write(target, &buf).context("failed to write database file")?;
            return Ok(());
        } else {
            fs::write(target, &bytes).context("failed to write database file")?;
            return Ok(());
        }
    }

    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(decoder);
    for entry in archive
        .entries()
        .context("failed to iterate archive entries")?
    {
        let mut entry = entry.context("failed to read archive entry")?;
        let path = entry
            .path()
            .context("failed to read archive path")?
            .into_owned();
        if path.extension().map(|ext| ext == "mmdb").unwrap_or(false) {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("failed to read mmdb entry")?;
            fs::write(target, &buf).context("failed to write database file")?;
            return Ok(());
        }
    }

    Err(anyhow!("mmdb file not found in archive"))
}

fn extract_point(city: &City) -> Option<GeoPoint> {
    let location = city.location.as_ref()?;
    let latitude = location.latitude?;
    let longitude = location.longitude?;
    let city_name = city
        .city
        .as_ref()
        .and_then(|record| record.names.as_ref())
        .and_then(|names| names.get("en"))
        .map(|value| value.to_string());
    let country_name = city
        .country
        .as_ref()
        .and_then(|record| record.names.as_ref())
        .and_then(|names| names.get("en"))
        .map(|value| value.to_string());
    Some(GeoPoint {
        latitude,
        longitude,
        city: city_name,
        country: country_name,
    })
}
