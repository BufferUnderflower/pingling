//! `proxy-dto` — neutral mihomo-dialect proxy outbound DTO for Proxy.
//!
//! ## What this crate is
//!
//! One typed Rust representation of a VPN proxy outbound that any
//! panel plugin (3x-ui, marzban, marzneshin, bpb, happ-panel,
//! sub-store users, panel users, …) can return from its
//! `UserApi::list_outbounds` implementation. The active [`VpnCore`]
//! in the host daemon then projects those structs into its native
//! config format (sing-box JSON, xray JSON, clash-meta YAML, …) via
//! the [`ToSingBoxJson`] / [`ToXrayJson`] / [`ToClashYaml`] traits
//! in [`crate::project`].
//!
//! ## Why the mihomo dialect
//!
//! Every modern proxy management panel that ships a Rust/Go/TS
//! subscription server emits the mihomo `proxies:` YAML dialect as
//! one of its output formats. sub-store and subconverter (the two
//! biggest normalization layers) both use mihomo's field names as
//! their pivot. Building on this convention means:
//!
//! - plugin authors can literally copy their panel's API response
//!   into these structs without renaming fields
//! - sub-store / subconverter output deserialises with no adapter layer
//! - adding a new protocol means adding one enum variant here, not
//!   inventing field names
//!
//! Research + rationale live in `docs/architecture-userapi.md` at the
//! workspace root.
//!
//! ## What this crate is NOT
//!
//! - Not a subscription URL codec (vmess:// / vless:// share links).
//!   Plugins that consume share links can convert them to these
//!   structs via crates like `vpn-link-serde`, but that's out of scope
//!   here.
//! - Not a core. The structs carry protocol secrets + transport
//!   details but don't open sockets; that's the [`VpnCore`] layer.
//! - Not a parser for every clash-meta feature. We vendored only the
//!   outbound struct definitions; clash rule / dns / tun / profile
//!   sections stay in mihomo.
//!
//! ## Attribution
//!
//! The struct definitions below are derived from clash-rs
//! (<https://github.com/Watfaq/clash-rs>, `clash-lib/src/config/internal/proxy.rs`).
//! clash-rs is Apache-2.0. See `NOTICE` at the crate root for the
//! complete attribution + list of changes.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod project;

// ---------------------------------------------------------------------------
// ProxyOutbound — the tagged union every plugin returns
// ---------------------------------------------------------------------------

/// A single proxy outbound, tagged by protocol.
///
/// Deserialise / serialise as mihomo YAML:
///
/// ```
/// # use pingling_proxy_dto::{ProxyOutbound};
/// let yaml = r#"
/// name: jp-tokyo-1
/// server: 203.0.113.10
/// port: 443
/// type: vless
/// uuid: 00000000-0000-0000-0000-000000000000
/// tls: true
/// server-name: example.com
/// network: ws
/// ws-opts:
///   path: /vl
///   headers:
///     Host: example.com
/// "#;
/// let parsed: ProxyOutbound = serde_yaml::from_str(yaml).unwrap();
/// assert!(matches!(parsed, ProxyOutbound::Vless(_)));
/// ```
///
/// Plugin authors typically construct these programmatically:
///
/// ```
/// # use pingling_proxy_dto::{ProxyOutbound, OutboundVless, CommonOptions, WsOpt};
/// # use std::collections::HashMap;
/// let outbound = ProxyOutbound::Vless(OutboundVless {
///     common_opts: CommonOptions {
///         name: "jp-tokyo-1".into(),
///         server: "203.0.113.10".into(),
///         port: 443,
///         connect_via: None,
///     },
///     uuid: "00000000-0000-0000-0000-000000000000".into(),
///     udp: Some(true),
///     tls: Some(true),
///     skip_cert_verify: None,
///     server_name: Some("example.com".into()),
///     network: Some("ws".into()),
///     ws_opts: Some(WsOpt {
///         path: Some("/vl".into()),
///         headers: Some(HashMap::from_iter([
///             ("Host".into(), "example.com".into()),
///         ])),
///         max_early_data: None,
///         early_data_header_name: None,
///     }),
///     h2_opts: None,
///     grpc_opts: None,
///     reality_opts: None,
///     flow: None,
///     client_fingerprint: None,
/// });
/// ```
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum ProxyOutbound {
    #[serde(rename = "direct")]
    Direct(OutboundDirect),

    #[serde(rename = "reject")]
    Reject(OutboundReject),

    #[serde(rename = "ss")]
    Ss(OutboundShadowsocks),

    #[serde(rename = "socks5")]
    Socks5(OutboundSocks5),

    #[serde(rename = "trojan")]
    Trojan(OutboundTrojan),

    #[serde(rename = "vmess")]
    Vmess(OutboundVmess),

    #[serde(rename = "vless")]
    Vless(OutboundVless),

    #[serde(rename = "wireguard")]
    Wireguard(OutboundWireguard),

    #[serde(rename = "tuic")]
    Tuic(OutboundTuic),

    #[serde(rename = "hysteria2")]
    Hysteria2(OutboundHysteria2),
}

impl ProxyOutbound {
    pub fn name(&self) -> &str {
        match self {
            Self::Direct(x) => &x.name,
            Self::Reject(x) => &x.name,
            Self::Ss(x) => &x.common_opts.name,
            Self::Socks5(x) => &x.common_opts.name,
            Self::Trojan(x) => &x.common_opts.name,
            Self::Vmess(x) => &x.common_opts.name,
            Self::Vless(x) => &x.common_opts.name,
            Self::Wireguard(x) => &x.common_opts.name,
            Self::Tuic(x) => &x.common_opts.name,
            Self::Hysteria2(x) => &x.name,
        }
    }

    pub fn type_tag(&self) -> &'static str {
        match self {
            Self::Direct(_) => "direct",
            Self::Reject(_) => "reject",
            Self::Ss(_) => "ss",
            Self::Socks5(_) => "socks5",
            Self::Trojan(_) => "trojan",
            Self::Vmess(_) => "vmess",
            Self::Vless(_) => "vless",
            Self::Wireguard(_) => "wireguard",
            Self::Tuic(_) => "tuic",
            Self::Hysteria2(_) => "hysteria2",
        }
    }
}

// ---------------------------------------------------------------------------
// Common options shared across every variant that talks to a server
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct CommonOptions {
    pub name: String,
    pub server: String,
    pub port: u16,
    #[serde(
        alias = "dialer-proxy",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub connect_via: Option<String>,
}

// ---------------------------------------------------------------------------
// Transport sub-structs (ws, grpc, h2, reality, tls)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct WsOpt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_early_data: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub early_data_header_name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct H2Opt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct GrpcOpt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc_service_name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct RealityOpt {
    pub public_key: String,
    pub short_id: String,
}

// ---------------------------------------------------------------------------
// Per-protocol structs
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundDirect {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundReject {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundShadowsocks {
    #[serde(flatten)]
    pub common_opts: CommonOptions,
    pub cipher: String,
    pub password: String,
    #[serde(default = "default_true")]
    pub udp: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_opts: Option<HashMap<String, serde_yaml::Value>>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundSocks5 {
    #[serde(flatten)]
    pub common_opts: CommonOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sni: Option<String>,
    #[serde(default)]
    pub skip_cert_verify: bool,
    #[serde(default = "default_true")]
    pub udp: bool,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundTrojan {
    #[serde(flatten)]
    pub common_opts: CommonOptions,
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpn: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sni: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_cert_verify: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc_opts: Option<GrpcOpt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_opts: Option<WsOpt>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundVmess {
    #[serde(flatten)]
    pub common_opts: CommonOptions,
    pub uuid: String,
    #[serde(alias = "alterId", default)]
    pub alter_id: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cipher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_cert_verify: Option<bool>,
    #[serde(alias = "servername", default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_opts: Option<WsOpt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h2_opts: Option<H2Opt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc_opts: Option<GrpcOpt>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundVless {
    #[serde(flatten)]
    pub common_opts: CommonOptions,
    pub uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_cert_verify: Option<bool>,
    #[serde(alias = "servername", default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_opts: Option<WsOpt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h2_opts: Option<H2Opt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc_opts: Option<GrpcOpt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reality_opts: Option<RealityOpt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_fingerprint: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundWireguard {
    #[serde(flatten)]
    pub common_opts: CommonOptions,
    pub private_key: String,
    pub public_key: String,
    #[serde(
        alias = "preshared-key",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub pre_shared_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp: Option<bool>,
    pub ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_dns_resolve: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_ips: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_bits: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundTuic {
    #[serde(flatten)]
    pub common_opts: CommonOptions,
    pub uuid: String,
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_interval: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpn: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_sni: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reduce_rtt: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp_relay_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub congestion_controller: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_udp_relay_packet_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_cert_verify: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_open_stream: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sni: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundHysteria2 {
    pub name: String,
    pub server: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<String>,
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obfs: Option<Hysteria2Obfs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obfs_password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpn: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sni: Option<String>,
    #[serde(default)]
    pub skip_cert_verify: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_str: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Hysteria2Obfs {
    Salamander,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn vless_roundtrip_preserves_mihomo_fields() {
        let yaml = indoc! {r#"
            name: jp-tokyo-1
            server: 203.0.113.10
            port: 443
            type: vless
            uuid: 00000000-0000-0000-0000-000000000000
            tls: true
            server-name: example.com
            network: ws
            ws-opts:
              path: /vl
              headers:
                Host: example.com
            flow: xtls-rprx-vision
        "#};

        let parsed: ProxyOutbound = serde_yaml::from_str(yaml).expect("vless yaml should parse");

        match &parsed {
            ProxyOutbound::Vless(v) => {
                assert_eq!(v.common_opts.name, "jp-tokyo-1");
                assert_eq!(v.common_opts.server, "203.0.113.10");
                assert_eq!(v.common_opts.port, 443);
                assert_eq!(v.uuid, "00000000-0000-0000-0000-000000000000");
                assert_eq!(v.tls, Some(true));
                assert_eq!(v.server_name.as_deref(), Some("example.com"));
                assert_eq!(v.network.as_deref(), Some("ws"));
                assert_eq!(v.flow.as_deref(), Some("xtls-rprx-vision"));
                let ws = v.ws_opts.as_ref().expect("ws-opts");
                assert_eq!(ws.path.as_deref(), Some("/vl"));
                assert_eq!(
                    ws.headers
                        .as_ref()
                        .and_then(|h| h.get("Host"))
                        .map(|s| s.as_str()),
                    Some("example.com")
                );
            }
            other => panic!("expected Vless, got {other:?}"),
        }

        let yaml2 = serde_yaml::to_string(&parsed).unwrap();
        let reparsed: ProxyOutbound = serde_yaml::from_str(&yaml2).unwrap();
        assert_eq!(reparsed.name(), "jp-tokyo-1");
    }

    #[test]
    fn vmess_alias_alterid_camel_case_accepted() {
        let yaml = indoc! {r#"
            name: legacy
            server: 203.0.113.20
            port: 443
            type: vmess
            uuid: 11111111-2222-3333-4444-555555555555
            alterId: 64
        "#};
        let parsed: ProxyOutbound = serde_yaml::from_str(yaml).unwrap();
        match parsed {
            ProxyOutbound::Vmess(v) => assert_eq!(v.alter_id, 64),
            other => panic!("expected Vmess, got {other:?}"),
        }
    }

    #[test]
    fn shadowsocks_parses_with_plugin_opts_as_opaque_map() {
        let yaml = indoc! {r#"
            name: ss-1
            server: 203.0.113.30
            port: 8388
            type: ss
            cipher: aes-128-gcm
            password: s3cret
            plugin: v2ray-plugin
            plugin-opts:
              mode: websocket
              host: example.com
              path: /ss
        "#};
        let parsed: ProxyOutbound = serde_yaml::from_str(yaml).unwrap();
        match parsed {
            ProxyOutbound::Ss(s) => {
                assert_eq!(s.cipher, "aes-128-gcm");
                assert_eq!(s.plugin.as_deref(), Some("v2ray-plugin"));
                assert!(s.plugin_opts.is_some());
            }
            other => panic!("expected Ss, got {other:?}"),
        }
    }

    #[test]
    fn hysteria2_with_salamander_obfs() {
        let yaml = indoc! {r#"
            name: hy2-edge
            server: 203.0.113.40
            port: 443
            type: hysteria2
            password: hyhy
            obfs: salamander
            obfs-password: obfsobfs
            up: 50000000
            down: 200000000
        "#};
        let parsed: ProxyOutbound = serde_yaml::from_str(yaml).unwrap();
        match parsed {
            ProxyOutbound::Hysteria2(h) => {
                assert!(matches!(h.obfs, Some(Hysteria2Obfs::Salamander)));
                assert_eq!(h.obfs_password.as_deref(), Some("obfsobfs"));
                assert_eq!(h.up, Some(50000000));
                assert_eq!(h.down, Some(200000000));
            }
            other => panic!("expected Hysteria2, got {other:?}"),
        }
    }

    #[test]
    fn wireguard_with_preshared_key_alias() {
        let yaml = indoc! {r#"
            name: wg-1
            server: 203.0.113.50
            port: 51820
            type: wireguard
            private-key: cHJpdmF0ZS1rZXk=
            public-key: cHVibGljLWtleQ==
            preshared-key: cHNrLWtleQ==
            ip: 10.0.0.2
            mtu: 1280
        "#};
        let parsed: ProxyOutbound = serde_yaml::from_str(yaml).unwrap();
        match parsed {
            ProxyOutbound::Wireguard(w) => {
                assert_eq!(w.pre_shared_key.as_deref(), Some("cHNrLWtleQ=="));
                assert_eq!(w.ip, "10.0.0.2");
                assert_eq!(w.mtu, Some(1280));
            }
            other => panic!("expected Wireguard, got {other:?}"),
        }
    }

    #[test]
    fn name_accessor_covers_every_variant() {
        let outbounds = [
            ProxyOutbound::Direct(OutboundDirect { name: "d".into() }),
            ProxyOutbound::Reject(OutboundReject { name: "r".into() }),
            ProxyOutbound::Ss(OutboundShadowsocks {
                common_opts: CommonOptions {
                    name: "ss".into(),
                    server: "x".into(),
                    port: 1,
                    connect_via: None,
                },
                cipher: "aes".into(),
                password: "pw".into(),
                udp: true,
                plugin: None,
                plugin_opts: None,
            }),
        ];
        let names: Vec<&str> = outbounds.iter().map(|o| o.name()).collect();
        assert_eq!(names, vec!["d", "r", "ss"]);
    }
}
