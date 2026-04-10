//! Domain value types.
//!
//! Pure data structures with no behaviour. Shared across all layers.

use std::fmt;

// ---------------------------------------------------------------------------
// ConnectionState
// ---------------------------------------------------------------------------

/// Represents the lifecycle state of a VPN connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// No active process, ready to connect.
    Disconnected,
    /// Process is starting, not yet ready.
    Connecting,
    /// Process is running and tunnel is active.
    Connected,
    /// Graceful shutdown in progress.
    Disconnecting,
    /// Process exited unexpectedly or an error occurred.
    Error(String),
}

impl fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => write!(f, "disconnected"),
            Self::Connecting => write!(f, "connecting"),
            Self::Connected => write!(f, "connected"),
            Self::Disconnecting => write!(f, "disconnecting"),
            Self::Error(msg) => write!(f, "error: {msg}"),
        }
    }
}

impl ConnectionState {
    /// Returns `true` if the connection is active or transitioning.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Connected | Self::Disconnecting
        )
    }

    /// Returns `true` if the state is `Connected`.
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }
}

// ---------------------------------------------------------------------------
// CoreEvent
// ---------------------------------------------------------------------------

/// Events emitted by a VPN core process.
///
/// Consumers subscribe to a stream of these events to react to lifecycle
/// changes, log output, and errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    /// The core process has started successfully.
    Started,

    /// The core process has exited (exit code).
    Stopped(i32),

    /// A line of stdout from the core process.
    Log(String),

    /// A line of stderr from the core process.
    ErrorLog(String),

    /// The connection state changed.
    StateChanged(ConnectionState),

    /// The core process crashed or was killed unexpectedly.
    Crashed(String),
}

// ---------------------------------------------------------------------------
// CoreInfo
// ---------------------------------------------------------------------------

/// Describes a VPN core engine's capabilities and metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreInfo {
    /// Human-readable name (e.g. "sing-box").
    pub name: String,

    /// Semantic version string.
    pub version: String,

    /// Supported protocol features (e.g. "vmess", "trojan", "wireguard").
    pub supported_protocols: Vec<String>,
}

impl fmt::Display for CoreInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} v{}", self.name, self.version)
    }
}

// ---------------------------------------------------------------------------
// PrerequisiteCheck
// ---------------------------------------------------------------------------

/// A single prerequisite that can be verified before connecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrerequisiteCheck {
    /// What is being checked.
    pub name: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Human-readable detail.
    pub message: String,
}

// ---------------------------------------------------------------------------
// CoreSource
// ---------------------------------------------------------------------------

/// How a VPN core binary was discovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreSource {
    /// Bundled with the application (sidecar).
    Bundled,
    /// User-specified path to a binary.
    Linked(String),
    /// Found in system PATH.
    System,
    /// Mock core for development/testing — no real binary needed.
    Mocked,
}

impl fmt::Display for CoreSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bundled => write!(f, "bundled"),
            Self::Linked(path) => write!(f, "linked:{path}"),
            Self::System => write!(f, "system"),
            Self::Mocked => write!(f, "mocked"),
        }
    }
}

// ---------------------------------------------------------------------------
// CoreDescriptor
// ---------------------------------------------------------------------------

/// Describes a discovered VPN core and its availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreDescriptor {
    /// Unique engine identifier (e.g. "sing-box", "xray", "mock").
    pub core_type: String,
    /// Human-readable name shown in UI.
    pub display_name: String,
    /// How the core was discovered.
    pub source: CoreSource,
    /// Resolved absolute path to the binary, or `None` for mocked.
    pub binary_path: Option<String>,
    /// Whether the core is ready to use (binary exists, prereqs met).
    pub available: bool,
}

// ---------------------------------------------------------------------------
// OutboundProtocol
// ---------------------------------------------------------------------------

/// VPN tunnel protocol used by an outbound.
///
/// These map 1:1 to protocols that sing-box, xray, and similar engines support.
/// Plugins can filter or re-prioritize outbounds based on protocol.
///
/// ```
/// # use domain::OutboundProtocol;
/// let p: OutboundProtocol = "vless".parse().unwrap();
/// assert_eq!(p, OutboundProtocol::Vless);
/// assert_eq!(p.as_str(), "vless");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OutboundProtocol {
    Vless,
    Vmess,
    Trojan,
    Shadowsocks,
    Wireguard,
    Hysteria,
    Hysteria2,
    Tuic,
    /// A relay/load-balancer node — not a tunnel itself.
    Direct,
    /// Selector group (user picks one child).
    Selector,
    /// Auto-test group (engine picks lowest-latency child).
    UrlTest,
    /// Protocol not known to the daemon. The raw string is carried so plugins
    /// and the Flutter UI can still display it.
    Other(String),
}

impl OutboundProtocol {
    /// Stable string identifier used in JSON-RPC and config files.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Vless => "vless",
            Self::Vmess => "vmess",
            Self::Trojan => "trojan",
            Self::Shadowsocks => "shadowsocks",
            Self::Wireguard => "wireguard",
            Self::Hysteria => "hysteria",
            Self::Hysteria2 => "hysteria2",
            Self::Tuic => "tuic",
            Self::Direct => "direct",
            Self::Selector => "selector",
            Self::UrlTest => "urltest",
            Self::Other(s) => s.as_str(),
        }
    }
}

impl fmt::Display for OutboundProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for OutboundProtocol {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "vless" => Self::Vless,
            "vmess" => Self::Vmess,
            "trojan" => Self::Trojan,
            "shadowsocks" | "ss" => Self::Shadowsocks,
            "wireguard" | "wg" => Self::Wireguard,
            "hysteria" => Self::Hysteria,
            "hysteria2" => Self::Hysteria2,
            "tuic" => Self::Tuic,
            "direct" => Self::Direct,
            "selector" => Self::Selector,
            "urltest" => Self::UrlTest,
            other => Self::Other(other.to_string()),
        })
    }
}

// ---------------------------------------------------------------------------
// OutboundTransport
// ---------------------------------------------------------------------------

/// Network transport underlying an outbound connection.
///
/// ```
/// # use domain::OutboundTransport;
/// let t: OutboundTransport = "ws".parse().unwrap();
/// assert_eq!(t, OutboundTransport::WebSocket);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OutboundTransport {
    Tcp,
    Udp,
    Http,
    WebSocket,
    Quic,
    Grpc,
    Other(u8), // placeholder — kept tiny since unknown transports are rare
}

impl OutboundTransport {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
            Self::Http => "http",
            Self::WebSocket => "ws",
            Self::Quic => "quic",
            Self::Grpc => "grpc",
            Self::Other(_) => "unknown",
        }
    }
}

impl fmt::Display for OutboundTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for OutboundTransport {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "tcp" => Self::Tcp,
            "udp" => Self::Udp,
            "http" | "h2" | "http2" => Self::Http,
            "ws" | "websocket" => Self::WebSocket,
            "quic" => Self::Quic,
            "grpc" => Self::Grpc,
            _ => Self::Other(0),
        })
    }
}

// ---------------------------------------------------------------------------
// Outbound
// ---------------------------------------------------------------------------

/// A VPN outbound (proxy server) that the user can connect through.
///
/// This is orthogonal to [`CoreDescriptor`]: the *core* is the engine (sing-box
/// vs xray), the *outbound* is a specific server within the engine's config.
///
/// Outbounds are parsed from the core's running config by the `VpnCore`
/// implementation and returned from [`VpnCore::list_outbounds`].
///
/// Plugins can modify the outbound list at the [`FilterOutbounds`](crate::hooks::HookPoint::FilterOutbounds)
/// hook — for example, filtering by country, adjusting latency scores, or
/// injecting custom outbounds.
///
/// ```
/// # use domain::types::{Outbound, OutboundProtocol, OutboundTransport};
/// let outbound = Outbound {
///     id: "jp-tokyo-1".into(),
///     name: "Tokyo #1".into(),
///     protocol: OutboundProtocol::Vless,
///     transport: OutboundTransport::WebSocket,
///     country_code: Some("JP".into()),
///     location: Some("Tokyo".into()),
///     latency_ms: None,
///     selected: false,
///     metadata: Default::default(),
/// };
/// assert_eq!(outbound.protocol.as_str(), "vless");
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Outbound {
    /// Unique identifier within the config (e.g. `"jp-tokyo-1"`).
    pub id: String,
    /// Human-readable display name (e.g. `"Tokyo #1"`).
    pub name: String,
    /// Tunnel protocol.
    pub protocol: OutboundProtocol,
    /// Network transport.
    pub transport: OutboundTransport,
    /// ISO 3166-1 alpha-2 country code (e.g. `"JP"`). `None` if unknown.
    pub country_code: Option<String>,
    /// Human-readable location name (e.g. `"Tokyo"`). `None` if unknown.
    pub location: Option<String>,
    /// Measured latency in milliseconds. `None` if untested.
    pub latency_ms: Option<u32>,
    /// Whether this outbound is currently selected/active.
    pub selected: bool,
    /// Extensible key-value metadata.
    ///
    /// Core implementations can stash extra data here (SNI, IP, tags).
    /// Plugins can read and write metadata at the `FilterOutbounds` hook
    /// without needing changes to this struct.
    pub metadata: std::collections::BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// ConnectionInfo
// ---------------------------------------------------------------------------

/// Information about an active VPN connection.
///
/// Returned by [`VpnCore::connection_info`] after a successful `start()`.
/// Forwarded to Flutter clients via JSON-RPC push (`event.connected`).
///
/// ```
/// # use domain::ConnectionInfo;
/// let info = ConnectionInfo {
///     server_name: "Tokyo #1".into(),
///     server_id: Some("jp-tokyo-1".into()),
///     country_code: Some("JP".into()),
///     connected_at: None,
///     session_id: None,
/// };
/// assert_eq!(info.server_name, "Tokyo #1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    /// Display name of the connected server.
    pub server_name: String,
    /// Outbound ID that was connected to.
    pub server_id: Option<String>,
    /// ISO 3166-1 alpha-2 country code.
    pub country_code: Option<String>,
    /// Timestamp when the connection was established (epoch seconds).
    pub connected_at: Option<u64>,
    /// Opaque session identifier for log correlation.
    pub session_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_state_display() {
        assert_eq!(ConnectionState::Disconnected.to_string(), "disconnected");
        assert_eq!(ConnectionState::Connecting.to_string(), "connecting");
        assert_eq!(ConnectionState::Connected.to_string(), "connected");
        assert_eq!(ConnectionState::Disconnecting.to_string(), "disconnecting");
        assert_eq!(
            ConnectionState::Error("timeout".into()).to_string(),
            "error: timeout"
        );
    }

    #[test]
    fn connection_state_is_active() {
        assert!(!ConnectionState::Disconnected.is_active());
        assert!(ConnectionState::Connecting.is_active());
        assert!(ConnectionState::Connected.is_active());
        assert!(ConnectionState::Disconnecting.is_active());
        assert!(!ConnectionState::Error("x".into()).is_active());
    }

    #[test]
    fn connection_state_is_connected() {
        assert!(!ConnectionState::Disconnected.is_connected());
        assert!(!ConnectionState::Connecting.is_connected());
        assert!(ConnectionState::Connected.is_connected());
        assert!(!ConnectionState::Disconnecting.is_connected());
        assert!(!ConnectionState::Error("fail".into()).is_connected());
    }

    #[test]
    fn core_event_variants() {
        assert!(matches!(CoreEvent::Started, CoreEvent::Started));

        let stopped = CoreEvent::Stopped(1);
        assert!(matches!(stopped, CoreEvent::Stopped(1)));

        let log = CoreEvent::Log("listening".into());
        assert!(matches!(&log, CoreEvent::Log(s) if s == "listening"));

        let err = CoreEvent::ErrorLog("warn".into());
        assert!(matches!(&err, CoreEvent::ErrorLog(s) if s == "warn"));

        let state = CoreEvent::StateChanged(ConnectionState::Connected);
        assert!(matches!(
            state,
            CoreEvent::StateChanged(ConnectionState::Connected)
        ));

        let crash = CoreEvent::Crashed("segfault".into());
        assert!(matches!(&crash, CoreEvent::Crashed(s) if s == "segfault"));
    }

    #[test]
    fn core_info_display() {
        let info = CoreInfo {
            name: "sing-box".into(),
            version: "1.13.0".into(),
            supported_protocols: vec!["vmess".into(), "trojan".into()],
        };
        assert_eq!(info.to_string(), "sing-box v1.13.0");
    }

    #[test]
    fn prerequisite_check_construction() {
        let check = PrerequisiteCheck {
            name: "binary".into(),
            passed: true,
            message: "sing-box found at /usr/bin/sing-box".into(),
        };
        assert_eq!(check.name, "binary");
        assert!(check.passed);
        assert_eq!(check.message, "sing-box found at /usr/bin/sing-box");
    }

    #[test]
    fn core_source_display() {
        assert_eq!(CoreSource::Bundled.to_string(), "bundled");
        assert_eq!(
            CoreSource::Linked("/opt/xray".into()).to_string(),
            "linked:/opt/xray"
        );
        assert_eq!(CoreSource::System.to_string(), "system");
        assert_eq!(CoreSource::Mocked.to_string(), "mocked");
    }

    #[test]
    fn core_descriptor_construction() {
        let desc = CoreDescriptor {
            core_type: "sing-box".into(),
            display_name: "Sing-Box".into(),
            source: CoreSource::Bundled,
            binary_path: Some("/usr/bin/sing-box".into()),
            available: true,
        };
        assert_eq!(desc.core_type, "sing-box");
        assert_eq!(desc.display_name, "Sing-Box");
        assert_eq!(desc.source, CoreSource::Bundled);
        assert_eq!(desc.binary_path, Some("/usr/bin/sing-box".into()));
        assert!(desc.available);
    }

    // -- OutboundProtocol ---------------------------------------------------

    #[test]
    fn outbound_protocol_roundtrip() {
        for (s, expected) in [
            ("vless", OutboundProtocol::Vless),
            ("vmess", OutboundProtocol::Vmess),
            ("trojan", OutboundProtocol::Trojan),
            ("shadowsocks", OutboundProtocol::Shadowsocks),
            ("ss", OutboundProtocol::Shadowsocks),
            ("wireguard", OutboundProtocol::Wireguard),
            ("wg", OutboundProtocol::Wireguard),
            ("hysteria2", OutboundProtocol::Hysteria2),
            ("tuic", OutboundProtocol::Tuic),
            ("direct", OutboundProtocol::Direct),
            ("selector", OutboundProtocol::Selector),
            ("urltest", OutboundProtocol::UrlTest),
        ] {
            let parsed: OutboundProtocol = s.parse().unwrap();
            assert_eq!(parsed, expected, "parse({s})");
        }
    }

    #[test]
    fn outbound_protocol_unknown_preserved() {
        let p: OutboundProtocol = "some-future-protocol".parse().unwrap();
        assert_eq!(p.as_str(), "some-future-protocol");
        assert_eq!(p.to_string(), "some-future-protocol");
    }

    // -- OutboundTransport --------------------------------------------------

    #[test]
    fn outbound_transport_roundtrip() {
        for (s, expected) in [
            ("tcp", OutboundTransport::Tcp),
            ("udp", OutboundTransport::Udp),
            ("ws", OutboundTransport::WebSocket),
            ("websocket", OutboundTransport::WebSocket),
            ("quic", OutboundTransport::Quic),
            ("grpc", OutboundTransport::Grpc),
        ] {
            let parsed: OutboundTransport = s.parse().unwrap();
            assert_eq!(parsed, expected, "parse({s})");
        }
    }

    // -- Outbound -----------------------------------------------------------

    #[test]
    fn outbound_construction() {
        let o = Outbound {
            id: "jp-1".into(),
            name: "Tokyo".into(),
            protocol: OutboundProtocol::Vless,
            transport: OutboundTransport::WebSocket,
            country_code: Some("JP".into()),
            location: Some("Tokyo".into()),
            latency_ms: Some(42),
            selected: true,
            metadata: Default::default(),
        };
        assert_eq!(o.id, "jp-1");
        assert_eq!(o.protocol.as_str(), "vless");
        assert_eq!(o.transport.as_str(), "ws");
        assert!(o.selected);
        assert_eq!(o.latency_ms, Some(42));
    }

    // -- ConnectionInfo -----------------------------------------------------

    #[test]
    fn connection_info_construction() {
        let info = ConnectionInfo {
            server_name: "Tokyo #1".into(),
            server_id: Some("jp-1".into()),
            country_code: Some("JP".into()),
            connected_at: Some(1700000000),
            session_id: Some("abc-123".into()),
        };
        assert_eq!(info.server_name, "Tokyo #1");
        assert_eq!(info.server_id.as_deref(), Some("jp-1"));
    }
}
