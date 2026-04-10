//! `RulesetProcessor` — bulletproof native ruleset download + cache.
//!
//! Sing-box's own ruleset fetcher is flaky on 20–50% of users on Windows
//! — silent retries, unpredictable timeouts. This processor downloads
//! every remote ruleset to a local cache directory and rewrites the
//! sing-box config to use `type=local` pointing into the cache. By the
//! time a config reaches the core, sing-box never sees a remote URL.
//!
//! ## Cache layout
//!
//! ```text
//! <cache_root>/
//!   sha256(<url>).<ext>      raw downloaded blob
//! ```
//!
//! `<ext>` is `.srs` for sing-box rule sets (binary format), `.json`
//! for source format. Cache key is the sha256 of the URL — no per-tag
//! namespace because tags can collide between users while URLs cannot.

use crate::attempt::ConfigRequest;
use crate::pipeline::ConfigProcessor;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// On-disk cache for downloaded rulesets. Owns the cache root
/// directory and provides keyed get/put.
pub struct RulesetCache {
    root: PathBuf,
}

impl RulesetCache {
    /// Construct a cache rooted at `root`. Creates the directory if it
    /// doesn't exist; failure to create propagates as an error string.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| format!("create cache dir {root:?}: {e}"))?;
        Ok(Self { root })
    }

    /// Compute the on-disk path for a given URL + format. Stable for
    /// the same URL across runs.
    pub fn path_for(&self, url: &str, format: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(url.as_bytes());
        let digest = hex::encode(hasher.finalize());
        let ext = match format {
            "binary" | "srs" => "srs",
            _ => "json",
        };
        self.root.join(format!("{digest}.{ext}"))
    }

    /// Read a cached blob for `url` if present.
    pub fn get(&self, url: &str, format: &str) -> Option<Vec<u8>> {
        let path = self.path_for(url, format);
        fs::read(&path).ok()
    }

    /// Write a blob to the cache for `url`. Returns the absolute path.
    pub fn put(&self, url: &str, format: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let path = self.path_for(url, format);
        fs::write(&path, bytes).map_err(|e| format!("write {path:?}: {e}"))?;
        Ok(path)
    }

    /// The root directory of this cache.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// The ruleset processor itself. Owns a [`RulesetCache`] and an HTTP
/// client. Cache misses are downloaded; download failures are logged
/// and the entry is left as remote (sing-box will see it and may itself
/// fail — that's surfaced as the next retry's previous_error).
pub struct RulesetProcessor {
    cache: RulesetCache,
    http_timeout: Duration,
    max_retries: u32,
}

impl RulesetProcessor {
    /// Construct with the given cache. Default HTTP timeout 15s,
    /// 3 retries per download.
    pub fn new(cache: RulesetCache) -> Self {
        Self {
            cache,
            http_timeout: Duration::from_secs(15),
            max_retries: 3,
        }
    }

    /// Customize HTTP timeout and retry count.
    pub fn with_http(mut self, timeout: Duration, max_retries: u32) -> Self {
        self.http_timeout = timeout;
        self.max_retries = max_retries;
        self
    }

    /// Download a ruleset blob with retry. Returns Err only after all
    /// retries are exhausted. Each retry sleeps `500ms * attempt`
    /// before trying again.
    fn download(&self, url: &str) -> Result<Vec<u8>, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(self.http_timeout)
            .timeout_read(self.http_timeout)
            .build();

        let mut last_err = String::from("no attempts");
        for attempt in 1..=self.max_retries {
            log::info!(
                "ruleset: download {url} attempt {attempt}/{}",
                self.max_retries
            );
            match agent.get(url).call() {
                Ok(resp) => {
                    let mut buf = Vec::new();
                    if let Err(e) = resp.into_reader().read_to_end(&mut buf) {
                        last_err = format!("read body: {e}");
                    } else {
                        return Ok(buf);
                    }
                }
                Err(ureq::Error::Status(code, _)) => {
                    last_err = format!("http status {code}");
                }
                Err(e) => {
                    last_err = format!("http: {e}");
                }
            }
            if attempt < self.max_retries {
                std::thread::sleep(Duration::from_millis(500 * attempt as u64));
            }
        }
        Err(format!("ruleset download {url} failed: {last_err}"))
    }
}

impl ConfigProcessor for RulesetProcessor {
    fn name(&self) -> &str {
        "ruleset"
    }

    fn process(&self, mut config: Value, _request: &ConfigRequest) -> Result<Value, String> {
        let Some(route) = config.get_mut("route").and_then(|v| v.as_object_mut()) else {
            return Ok(config);
        };
        let Some(rule_sets) = route.get_mut("rule_set").and_then(|v| v.as_array_mut()) else {
            return Ok(config);
        };

        for entry in rule_sets.iter_mut() {
            let Some(obj) = entry.as_object_mut() else {
                continue;
            };
            if obj.get("type").and_then(|v| v.as_str()) != Some("remote") {
                continue;
            }
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let format = obj
                .get("format")
                .and_then(|v| v.as_str())
                .unwrap_or("binary")
                .to_string();
            let tag = obj
                .get("tag")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            if url.is_empty() {
                continue;
            }

            // Cache hit → just rewrite. Cache miss → download then rewrite.
            // Download failure → log + leave as remote.
            let have = self.cache.get(&url, &format).is_some();
            if !have {
                match self.download(&url) {
                    Ok(bytes) => {
                        if let Err(e) = self.cache.put(&url, &format, &bytes) {
                            log::warn!("ruleset: cache write failed for {url}: {e}");
                            continue;
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "ruleset: download failed for {url}: {e} — leaving as remote"
                        );
                        continue;
                    }
                }
            }

            let path = self.cache.path_for(&url, &format);
            *entry = json!({
                "type": "local",
                "tag": tag,
                "format": format,
                "path": path.to_string_lossy()
            });
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempt::AttemptInfo;
    use crate::strategy::{ConnectionStrategy, ResolverType, RetryPolicy, StackType};
    use std::time::Duration;
    use tempfile::TempDir;

    fn req() -> ConfigRequest {
        ConfigRequest {
            with_host_dns: false,
            default_dns_server: None,
            attempt: AttemptInfo {
                strategy: ConnectionStrategy {
                    id: "x".into(),
                    stack: StackType::System,
                    resolver_type: ResolverType::Doh,
                    total_timeout: Duration::from_secs(30),
                    retry: RetryPolicy::NoRetry,
                },
                attempt_number: 1,
                previous_error: None,
            },
        }
    }

    // ----------------------------------------------------------------
    // Cache layer (B11 — no network)
    // ----------------------------------------------------------------

    #[test]
    fn cache_creates_root_dir() {
        let tmp = TempDir::new().unwrap();
        let cache_root = tmp.path().join("ruleset-cache");
        let _ = RulesetCache::new(&cache_root).unwrap();
        assert!(cache_root.exists());
        assert!(cache_root.is_dir());
    }

    #[test]
    fn cache_put_then_get_returns_bytes() {
        let tmp = TempDir::new().unwrap();
        let cache = RulesetCache::new(tmp.path().join("c")).unwrap();
        let url = "https://example.com/geoip.srs";
        cache.put(url, "binary", b"BLOB").unwrap();
        assert_eq!(cache.get(url, "binary").as_deref(), Some(&b"BLOB"[..]));
    }

    #[test]
    fn cache_get_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        let cache = RulesetCache::new(tmp.path().join("c")).unwrap();
        assert!(cache.get("https://nowhere", "binary").is_none());
    }

    #[test]
    fn cache_path_uses_sha256_of_url() {
        let tmp = TempDir::new().unwrap();
        let cache = RulesetCache::new(tmp.path().join("c")).unwrap();
        let p1 = cache.path_for("https://a.example/x", "binary");
        let p2 = cache.path_for("https://b.example/x", "binary");
        assert_ne!(p1, p2);
        assert!(p1.to_string_lossy().ends_with(".srs"));
    }

    #[test]
    fn processor_rewrites_remote_to_local_on_cache_hit() {
        let tmp = TempDir::new().unwrap();
        let cache = RulesetCache::new(tmp.path().join("c")).unwrap();
        let url = "https://example.com/geoip.srs";
        cache.put(url, "binary", b"PRECOMPUTED").unwrap();

        let processor = RulesetProcessor::new(cache);
        let cfg = json!({
            "route": {
                "rule_set": [
                    {"type": "remote", "tag": "geoip", "url": url, "format": "binary"}
                ]
            }
        });
        let out = processor.process(cfg, &req()).unwrap();
        let entry = &out["route"]["rule_set"][0];
        assert_eq!(entry["type"], "local");
        assert_eq!(entry["tag"], "geoip");
        assert_eq!(entry["format"], "binary");
        assert!(entry["path"].as_str().unwrap().ends_with(".srs"));
    }

    #[test]
    fn processor_leaves_local_entries_alone() {
        let tmp = TempDir::new().unwrap();
        let cache = RulesetCache::new(tmp.path().join("c")).unwrap();
        let processor = RulesetProcessor::new(cache);
        let cfg = json!({
            "route": {
                "rule_set": [
                    {"type": "local", "tag": "user", "path": "/etc/foo.srs", "format": "binary"}
                ]
            }
        });
        let out = processor.process(cfg.clone(), &req()).unwrap();
        assert_eq!(out, cfg);
    }

    // ----------------------------------------------------------------
    // HTTP downloader (B12 — mockito-backed)
    // ----------------------------------------------------------------

    #[test]
    fn downloads_and_caches_on_cache_miss() {
        let mut server = mockito::Server::new();
        let body = b"DOWNLOADED-BLOB";
        let mock = server
            .mock("GET", "/geoip.srs")
            .with_status(200)
            .with_body(body)
            .create();

        let tmp = TempDir::new().unwrap();
        let cache = RulesetCache::new(tmp.path().join("c")).unwrap();
        let processor = RulesetProcessor::new(cache);
        let url = format!("{}/geoip.srs", server.url());

        let cfg = json!({
            "route": {
                "rule_set": [
                    {"type": "remote", "tag": "geoip", "url": url, "format": "binary"}
                ]
            }
        });
        let out = processor.process(cfg, &req()).unwrap();
        let entry = &out["route"]["rule_set"][0];
        assert_eq!(entry["type"], "local");
        let path = entry["path"].as_str().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), body);
        mock.assert();
    }

    #[test]
    fn retries_on_http_500_then_succeeds() {
        let mut server = mockito::Server::new();
        let body = b"OK-AT-LAST";
        // First call: 500. Second call: 200.
        let m1 = server
            .mock("GET", "/x.srs")
            .with_status(500)
            .with_body("nope")
            .expect(1)
            .create();
        let m2 = server
            .mock("GET", "/x.srs")
            .with_status(200)
            .with_body(body)
            .expect_at_least(1)
            .create();

        let tmp = TempDir::new().unwrap();
        let cache = RulesetCache::new(tmp.path().join("c")).unwrap();
        let processor =
            RulesetProcessor::new(cache).with_http(Duration::from_secs(2), 3);
        let url = format!("{}/x.srs", server.url());
        let cfg = json!({
            "route": {
                "rule_set": [
                    {"type": "remote", "tag": "x", "url": url, "format": "binary"}
                ]
            }
        });
        let out = processor.process(cfg, &req()).unwrap();
        assert_eq!(out["route"]["rule_set"][0]["type"], "local");
        m1.assert();
        m2.assert();
    }

    #[test]
    fn leaves_remote_when_all_retries_fail() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/dead.srs")
            .with_status(500)
            .expect_at_least(3)
            .create();

        let tmp = TempDir::new().unwrap();
        let cache = RulesetCache::new(tmp.path().join("c")).unwrap();
        let processor =
            RulesetProcessor::new(cache).with_http(Duration::from_secs(2), 3);
        let url = format!("{}/dead.srs", server.url());
        let cfg = json!({
            "route": {
                "rule_set": [
                    {"type": "remote", "tag": "dead", "url": url, "format": "binary"}
                ]
            }
        });
        let out = processor.process(cfg.clone(), &req()).unwrap();
        // Stays remote — pipeline does not error, downstream sing-box sees the original.
        assert_eq!(out["route"]["rule_set"][0]["type"], "remote");
        mock.assert();
    }
}
