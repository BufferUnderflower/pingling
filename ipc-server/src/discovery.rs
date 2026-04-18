//! Discovery — both publishing (so clients can find this daemon) and the
//! UDP beacon listener (so clients on the same LAN can probe for any daemon).
//!
//! ## Local: filesystem registry
//!
//! On startup the server writes the registry entry under the shared
//! runtime cache root (`util::paths::registry_dir()`), as
//! `<cache>/pingle/daemons/<pid>.json`. A sibling `latest.json` copy points
//! at the most recent file. Clients on the same machine can `readdir` the
//! directory to find every running daemon.
//!
//! ## Network: UDP beacon
//!
//! The server binds `0.0.0.0:7878` UDP and answers any datagram whose payload
//! starts with `AVARS_DISCOVER_v1` with a JSON description of itself. A client
//! sends that probe to `255.255.255.255:7878` (broadcast) and receives one
//! response per daemon on the LAN. Stateless, no mDNS daemon required.

use serde::Serialize;
use serde_json::json;
use std::fs;
use std::io;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use util::paths::registry_dir as runtime_registry_dir;

/// Well-known UDP port for the discovery beacon. Same value used by client
/// and server. Pick a number unlikely to clash on a developer machine.
pub const DISCOVERY_PORT: u16 = 7878;

/// Magic prefix for probe datagrams. The client sends this; the server
/// recognizes it and replies. Versioning the prefix lets us evolve the
/// payload format later without breaking old clients.
pub const PROBE_MAGIC: &str = "AVARS_DISCOVER_v1";

/// What the daemon advertises about itself. Identical shape on disk and on
/// the wire (UDP beacon response).
#[derive(Debug, Clone, Serialize)]
pub struct DaemonAdvertisement {
    /// Always `"pingle"` — distinguishes our beacon from anything else
    /// happening on port 7878.
    pub service: &'static str,
    /// Daemon protocol version. See [`crate::PROTOCOL_VERSION`].
    pub protocol_version: u32,
    /// OS process id. Unique per daemon on a machine.
    pub pid: u32,
    /// Advertised host name for local loopback clients.
    pub hostname: String,
    /// `host:port` for TCP loopback. `None` if TCP listener failed to bind.
    pub tcp: Option<String>,
    /// Filesystem path of the Unix domain socket. `None` on Windows or if
    /// UDS failed to bind.
    pub uds: Option<String>,
    /// Session log file path for this daemon, if the caller published one.
    pub log_file: Option<String>,
    /// Unix epoch seconds when the daemon started.
    pub started_at: u64,
}

impl DaemonAdvertisement {
    pub fn new(tcp: Option<String>, uds: Option<String>) -> Self {
        Self {
            service: "pingle",
            protocol_version: crate::PROTOCOL_VERSION,
            pid: std::process::id(),
            hostname: hostname(),
            tcp,
            uds,
            log_file: std::env::var("PINGLE_DAEMON_LOG_FILE").ok(),
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_else(|_| b"{}".to_vec())
    }
}

/// Advertised host for loopback-only daemons.
///
/// The daemon only binds local addresses, so discovery should surface the
/// loopback name that clients can actually connect to rather than the machine
/// hostname, which is only useful for labels.
fn hostname() -> String {
    "localhost".to_string()
}

/// Compute the directory where per-daemon registration files live.
/// Created on demand. Returns the path even if the create failed; callers
/// should still try to write — they may have partial perms.
pub fn registry_dir() -> PathBuf {
    let dir = runtime_registry_dir();
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Path to this daemon's registration file.
pub fn registry_file_for_self() -> PathBuf {
    registry_dir().join(format!("{}.json", std::process::id()))
}

/// Write the registration file. Best-effort; logs but does not fail.
///
/// Also prunes stale registry files whose PIDs are no longer alive — a
/// previous daemon that died via SIGKILL or crash leaves behind a dangling
/// JSON file because its `Drop` handler never ran. Cleaning on every fresh
/// start keeps discovery results honest without a separate janitor process.
pub fn publish_registry(ad: &DaemonAdvertisement) -> io::Result<PathBuf> {
    prune_stale_entries();
    let path = registry_file_for_self();
    fs::write(&path, ad.to_json_bytes())?;
    // Maintain a "latest.json" pointer so simple clients can grab the most
    // recently started daemon without scanning. We use a regular file copy
    // (not a symlink) for cross-platform compatibility.
    let latest = registry_dir().join("latest.json");
    let _ = fs::write(&latest, ad.to_json_bytes());
    Ok(path)
}

/// Remove registry files for PIDs that are no longer alive on this machine.
/// Called from [`publish_registry`] on every daemon startup.
///
/// We consider a PID "dead" if sending it signal 0 returns ESRCH (no such
/// process). On Unix this is the canonical "is this process alive" check —
/// it does not actually deliver a signal.
pub fn prune_stale_entries() {
    let dir = registry_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !file_name.ends_with(".json") || file_name == "latest.json" {
            continue;
        }
        // Filename is "<pid>.json" — parse out the pid.
        let pid_str = file_name.trim_end_matches(".json");
        let Ok(pid) = pid_str.parse::<i32>() else {
            continue;
        };
        if !pid_alive(pid) {
            let _ = fs::remove_file(&path);
            log::debug!("ipc: pruned stale registry entry for pid {pid}");
        }
    }
}

/// Returns `true` if the given PID is still alive on this machine.
///
/// Unix implementation uses `kill(pid, 0)` which does nothing but reports
/// whether the process exists. Returns `true` on Windows fallback (we just
/// can't prune — the registry file still gets overwritten on port reuse).
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // Safety: we only call kill(pid, 0) which never delivers a signal. The
    // only side effect is setting errno. We translate the return value.
    let rc = unsafe { libc_kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    // errno == ESRCH means "no such process". Any other error (EPERM: exists
    // but we don't own it) still counts as alive for our purposes.
    let err = std::io::Error::last_os_error();
    err.raw_os_error() != Some(3 /* ESRCH */)
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    true
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    // We don't pull the `libc` crate for just one symbol — extern "C" above
    // suffices. `kill` is in libSystem/libc and always linked.
    kill(pid, sig)
}

/// Best-effort cleanup of this daemon's registration file. Called from
/// the server's shutdown path or via [`RegistryGuard`] in scope-drop tests.
pub fn cleanup_registry() {
    let _ = fs::remove_file(registry_file_for_self());
}

/// RAII guard that removes the registration file on drop. Useful in tests
/// and any future structured shutdown path.
pub struct RegistryGuard;
impl Drop for RegistryGuard {
    fn drop(&mut self) {
        cleanup_registry();
    }
}

/// Spawn the UDP beacon listener thread. Returns immediately. The thread
/// runs until the process exits.
pub fn spawn_beacon(ad: Arc<DaemonAdvertisement>) -> io::Result<()> {
    let socket = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(60)))?;

    log::info!("ipc: discovery beacon listening on udp/{}", DISCOVERY_PORT);

    thread::Builder::new()
        .name("pingle-ipc-beacon".into())
        .spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((n, peer)) => {
                        let payload = &buf[..n];
                        if !payload.starts_with(PROBE_MAGIC.as_bytes()) {
                            // Not for us — ignore.
                            continue;
                        }
                        let response = ad.to_json_bytes();
                        if let Err(e) = socket.send_to(&response, peer) {
                            log::debug!("ipc: beacon reply to {peer} failed: {e}");
                        } else {
                            log::debug!("ipc: beacon replied to {peer}");
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        // Periodic timeout — loop and recv again. This lets
                        // the thread react to socket close in the future.
                        continue;
                    }
                    Err(e) => {
                        log::warn!("ipc: beacon recv error: {e}");
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        })
        .map(|_| ())
}

/// Probe handler for tests / future client-side use. Sends a probe to the
/// given address and parses the JSON response.
#[cfg(test)]
pub fn probe_once(addr: &str, timeout: Duration) -> io::Result<DaemonAdvertisementOwned> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(timeout))?;
    socket.send_to(PROBE_MAGIC.as_bytes(), addr)?;

    let mut buf = [0u8; 4096];
    let (n, _peer) = socket.recv_from(&mut buf)?;
    serde_json::from_slice(&buf[..n]).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Owned (Deserialize) twin of [`DaemonAdvertisement`]. Lets tests parse
/// what the server sends without sharing the `&'static service` field.
#[cfg(test)]
#[derive(Debug, Clone, serde::Deserialize)]
#[allow(dead_code)]
pub struct DaemonAdvertisementOwned {
    pub service: String,
    pub protocol_version: u32,
    pub pid: u32,
    pub hostname: String,
    pub tcp: Option<String>,
    pub uds: Option<String>,
    #[serde(default)]
    pub log_file: Option<String>,
    pub started_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    struct EnvGuard(&'static str);

    impl EnvGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            unsafe { std::env::set_var(key, value) };
            Self(key)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var(self.0) };
        }
    }

    fn install_runtime_env(root: &std::path::Path) -> Vec<EnvGuard> {
        let mut guards = Vec::new();
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "APPDATA",
            "LOCALAPPDATA",
            "TMPDIR",
            "TEMP",
            "TMP",
        ] {
            guards.push(EnvGuard::set(key, root));
        }
        guards
    }

    #[test]
    #[serial]
    fn registry_file_round_trip() {
        let home = TempDir::new().expect("runtime tempdir");
        let _guards = install_runtime_env(home.path());
        let ad = DaemonAdvertisement::new(Some("127.0.0.1:9999".into()), None);
        let path = publish_registry(&ad).expect("publish");
        let read = fs::read_to_string(&path).expect("read");
        assert!(read.contains("\"service\":\"pingle\""));
        assert!(read.contains("\"tcp\":\"127.0.0.1:9999\""));
        assert!(read.contains("\"log_file\":null"));
        cleanup_registry();
    }

    #[test]
    fn beacon_replies_to_probe() {
        let ad = Arc::new(DaemonAdvertisement::new(
            Some("127.0.0.1:9000".into()),
            Some("/tmp/pingle-test.sock".into()),
        ));
        // We can't predict if the well-known port is free in CI; bind ad-hoc.
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind server");
        let server_addr = server.local_addr().unwrap();
        let ad_for_thread = ad.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            let (n, peer) = server.recv_from(&mut buf).unwrap();
            assert!(buf[..n].starts_with(PROBE_MAGIC.as_bytes()));
            server
                .send_to(&ad_for_thread.to_json_bytes(), peer)
                .unwrap();
        });
        thread::sleep(Duration::from_millis(50));
        let result = probe_once(&server_addr.to_string(), Duration::from_secs(2)).unwrap();
        assert_eq!(result.service, "pingle");
        assert_eq!(result.hostname, "localhost");
        assert_eq!(result.tcp.as_deref(), Some("127.0.0.1:9000"));
        assert_eq!(result.log_file, None);
    }
}

// Suppress unused-import warnings if json! is unused later
#[allow(dead_code)]
fn _ensure_json_used() -> serde_json::Value {
    json!({})
}
