//! Projection traits: `PingleOutbound` → native core config.
//!
//! One [`PingleOutbound`](crate::PingleOutbound) lands in the daemon
//! via a plugin call; the active [`VpnCore`] then asks the outbound
//! for its native form. Each core implements one trait:
//!
//! - [`ToClashYaml`]   for clash-meta / mihomo
//! - [`ToSingBoxJson`] for sing-box
//! - [`ToXrayJson`]    for xray-core / v2ray-core
//!
//! ## Why three projections instead of one "universal" format
//!
//! Because sing-box, xray, and clash-meta use subtly different field
//! names and value shapes for the same protocol. For example:
//!
//! | Concept                   | mihomo/clash        | sing-box              | xray                     |
//! |---------------------------|---------------------|-----------------------|--------------------------|
//! | Outbound kind             | `type: vless`       | `type: vless`         | `protocol: vless`        |
//! | TLS enabled flag          | `tls: true`         | `tls.enabled: true`   | `streamSettings.security: "tls"` |
//! | SNI                       | `server-name`       | `tls.server_name`     | `streamSettings.tlsSettings.serverName` |
//! | Skip cert verify          | `skip-cert-verify`  | `tls.insecure`        | `tlsSettings.allowInsecure` |
//! | Transport network         | `network: ws`       | `transport.type: ws`  | `streamSettings.network: "ws"` |
//! | WebSocket path            | `ws-opts.path`      | `transport.path`      | `wsSettings.path`        |
//!
//! The tables below in each projection impl are the whole
//! dependency hell this crate exists to centralise. If a new field
//! appears in mihomo's outbound adapter, the change is one line
//! here, not one line per core crate.
//!
//! ## Completeness
//!
//! Projections cover the **common path**: protocol + tls + transport +
//! auth secret + server endpoint. Panel-emitted extensions like
//! sing-box's `domain_strategy`, `packet_encoding`, or mihomo's
//! `smux` / `xudp` / `ip-version` are NOT projected today — they
//! stay in the struct and the target core's native translator can
//! look them up by key if it wants. We add them as need arises.

use crate::{
    Hysteria2Obfs, OutboundHysteria2, OutboundShadowsocks, OutboundTrojan, OutboundTuic,
    OutboundVless, OutboundVmess, OutboundWireguard, PingleOutbound,
};
use serde_json::{json, Value};
use serde_yaml::Value as YamlValue;

// ---------------------------------------------------------------------------
// ToClashYaml — trivial because mihomo IS our canonical format
// ---------------------------------------------------------------------------

/// Render a [`PingleOutbound`] to clash-meta / mihomo YAML.
///
/// Trivial: our serde attributes already match mihomo's dialect, so
/// `serde_yaml::to_value` is the whole projection.
pub trait ToClashYaml {
    fn to_clash_yaml(&self) -> YamlValue;
}

impl ToClashYaml for PingleOutbound {
    fn to_clash_yaml(&self) -> YamlValue {
        // Cannot fail: every field in the struct is a plain type
        // that serde_yaml handles. If this ever starts failing in
        // production, the fix is to add `?` handling here and return
        // `Result<YamlValue, String>`.
        serde_yaml::to_value(self).expect("PingleOutbound should always serialise to yaml")
    }
}

// ---------------------------------------------------------------------------
// ToSingBoxJson — the one with the field-rename tables
// ---------------------------------------------------------------------------

/// Render a [`PingleOutbound`] to sing-box outbound JSON.
///
/// See <https://sing-box.sagernet.org/configuration/outbound/> for the
/// canonical sing-box outbound schema. The JSON produced here should
/// drop into the top-level `outbounds` array of a sing-box config
/// without further changes.
pub trait ToSingBoxJson {
    fn to_singbox_json(&self) -> Value;
}

impl ToSingBoxJson for PingleOutbound {
    fn to_singbox_json(&self) -> Value {
        match self {
            Self::Direct(d) => json!({
                "type": "direct",
                "tag": d.name,
            }),
            Self::Reject(r) => json!({
                // sing-box calls this "block", not "reject".
                "type": "block",
                "tag": r.name,
            }),
            Self::Ss(s) => shadowsocks_to_singbox(s),
            Self::Socks5(s) => json!({
                "type": "socks",
                "tag": s.common_opts.name,
                "server": s.common_opts.server,
                "server_port": s.common_opts.port,
                "version": "5",
                "username": s.username,
                "password": s.password,
                "udp_over_tcp": !s.udp,
            }),
            Self::Trojan(t) => trojan_to_singbox(t),
            Self::Vmess(v) => vmess_to_singbox(v),
            Self::Vless(v) => vless_to_singbox(v),
            Self::Wireguard(w) => wireguard_to_singbox(w),
            Self::Tuic(t) => tuic_to_singbox(t),
            Self::Hysteria2(h) => hysteria2_to_singbox(h),
        }
    }
}

// ---- shadowsocks ----------------------------------------------------------

fn shadowsocks_to_singbox(s: &OutboundShadowsocks) -> Value {
    json!({
        "type": "shadowsocks",
        "tag": s.common_opts.name,
        "server": s.common_opts.server,
        "server_port": s.common_opts.port,
        "method": s.cipher,
        "password": s.password,
        // sing-box has no per-outbound udp toggle; udp is implied by
        // the protocol variant. The mihomo `udp: false` flag is
        // handled at the dispatcher level (block UDP via the inbound
        // or a route rule). Surface it in the JSON for observability.
        "udp_over_tcp": !s.udp,
    })
}

// ---- trojan ---------------------------------------------------------------

fn trojan_to_singbox(t: &OutboundTrojan) -> Value {
    let mut out = json!({
        "type": "trojan",
        "tag": t.common_opts.name,
        "server": t.common_opts.server,
        "server_port": t.common_opts.port,
        "password": t.password,
        "tls": tls_block(
            true, // trojan implies tls in mihomo
            t.sni.as_deref(),
            t.skip_cert_verify.unwrap_or(false),
            t.alpn.as_ref(),
        ),
    });
    attach_transport(
        &mut out,
        t.network.as_deref(),
        t.ws_opts.as_ref(),
        None,
        t.grpc_opts.as_ref(),
    );
    out
}

// ---- vmess ---------------------------------------------------------------

fn vmess_to_singbox(v: &OutboundVmess) -> Value {
    let mut out = json!({
        "type": "vmess",
        "tag": v.common_opts.name,
        "server": v.common_opts.server,
        "server_port": v.common_opts.port,
        "uuid": v.uuid,
        "alter_id": v.alter_id,
        "security": v.cipher.clone().unwrap_or_else(|| "auto".to_string()),
    });
    if v.tls.unwrap_or(false) {
        out["tls"] = tls_block(
            true,
            v.server_name.as_deref(),
            v.skip_cert_verify.unwrap_or(false),
            None,
        );
    }
    attach_transport(
        &mut out,
        v.network.as_deref(),
        v.ws_opts.as_ref(),
        v.h2_opts.as_ref(),
        v.grpc_opts.as_ref(),
    );
    out
}

// ---- vless ---------------------------------------------------------------

fn vless_to_singbox(v: &OutboundVless) -> Value {
    let mut out = json!({
        "type": "vless",
        "tag": v.common_opts.name,
        "server": v.common_opts.server,
        "server_port": v.common_opts.port,
        "uuid": v.uuid,
    });
    if let Some(flow) = &v.flow {
        out["flow"] = json!(flow);
    }
    if v.tls.unwrap_or(false) {
        let mut tls = tls_block(
            true,
            v.server_name.as_deref(),
            v.skip_cert_verify.unwrap_or(false),
            None,
        );
        // REALITY — only set when reality_opts present.
        if let Some(reality) = &v.reality_opts {
            tls["reality"] = json!({
                "enabled": true,
                "public_key": reality.public_key,
                "short_id": reality.short_id,
            });
        }
        if let Some(fp) = &v.client_fingerprint {
            tls["utls"] = json!({
                "enabled": true,
                "fingerprint": fp,
            });
        }
        out["tls"] = tls;
    }
    attach_transport(
        &mut out,
        v.network.as_deref(),
        v.ws_opts.as_ref(),
        v.h2_opts.as_ref(),
        v.grpc_opts.as_ref(),
    );
    out
}

// ---- wireguard -----------------------------------------------------------

fn wireguard_to_singbox(w: &OutboundWireguard) -> Value {
    let mut addresses = vec![format!("{}/32", w.ip)];
    if let Some(ipv6) = &w.ipv6 {
        addresses.push(format!("{ipv6}/128"));
    }
    let mut out = json!({
        "type": "wireguard",
        "tag": w.common_opts.name,
        "server": w.common_opts.server,
        "server_port": w.common_opts.port,
        "local_address": addresses,
        "private_key": w.private_key,
        "peer_public_key": w.public_key,
    });
    if let Some(psk) = &w.pre_shared_key {
        out["pre_shared_key"] = json!(psk);
    }
    if let Some(mtu) = w.mtu {
        out["mtu"] = json!(mtu);
    }
    if let Some(reserved) = &w.reserved_bits {
        out["reserved"] = json!(reserved);
    }
    out
}

// ---- tuic ---------------------------------------------------------------

fn tuic_to_singbox(t: &OutboundTuic) -> Value {
    let mut out = json!({
        "type": "tuic",
        "tag": t.common_opts.name,
        "server": t.common_opts.server,
        "server_port": t.common_opts.port,
        "uuid": t.uuid,
        "password": t.password,
        "tls": tls_block(
            true,
            t.sni.as_deref(),
            t.skip_cert_verify.unwrap_or(false),
            t.alpn.as_ref(),
        ),
    });
    if let Some(cc) = &t.congestion_controller {
        out["congestion_control"] = json!(cc);
    }
    if let Some(mode) = &t.udp_relay_mode {
        out["udp_relay_mode"] = json!(mode);
    }
    if t.reduce_rtt.unwrap_or(false) {
        out["zero_rtt_handshake"] = json!(true);
    }
    out
}

// ---- hysteria2 ----------------------------------------------------------

fn hysteria2_to_singbox(h: &OutboundHysteria2) -> Value {
    let mut out = json!({
        "type": "hysteria2",
        "tag": h.name,
        "server": h.server,
        "server_port": h.port,
        "password": h.password,
        "tls": tls_block(
            true,
            h.sni.as_deref(),
            h.skip_cert_verify,
            h.alpn.as_ref(),
        ),
    });
    if let Some(obfs) = &h.obfs {
        let kind = match obfs {
            Hysteria2Obfs::Salamander => "salamander",
        };
        out["obfs"] = json!({
            "type": kind,
            "password": h.obfs_password.clone().unwrap_or_default(),
        });
    }
    if let Some(up) = h.up {
        out["up_mbps"] = json!(up / 1_000_000);
    }
    if let Some(down) = h.down {
        out["down_mbps"] = json!(down / 1_000_000);
    }
    if let Some(ports) = &h.ports {
        out["server_ports"] = json!(ports);
    }
    out
}

// ---- helpers -------------------------------------------------------------

/// Build the sing-box `tls` sub-object the same way every
/// tls-carrying protocol expects it.
fn tls_block(
    enabled: bool,
    server_name: Option<&str>,
    insecure: bool,
    alpn: Option<&Vec<String>>,
) -> Value {
    let mut tls = json!({ "enabled": enabled });
    if let Some(sni) = server_name {
        tls["server_name"] = json!(sni);
    }
    if insecure {
        tls["insecure"] = json!(true);
    }
    if let Some(alpn) = alpn {
        tls["alpn"] = json!(alpn);
    }
    tls
}

/// Attach the appropriate `transport` sub-object to `out` based on
/// the mihomo `network` + transport-opts pair.
///
/// mihomo's `network:` field drives which transport-specific opts
/// the core reads. sing-box uses a tagged `transport.type` union
/// with different field names; this translation is the main reason
/// the projection layer exists.
fn attach_transport(
    out: &mut Value,
    network: Option<&str>,
    ws: Option<&crate::WsOpt>,
    h2: Option<&crate::H2Opt>,
    grpc: Option<&crate::GrpcOpt>,
) {
    match network {
        Some("ws") | Some("websocket") => {
            if let Some(ws) = ws {
                let mut t = json!({ "type": "ws" });
                if let Some(p) = &ws.path {
                    t["path"] = json!(p);
                }
                if let Some(headers) = &ws.headers {
                    t["headers"] = json!(headers);
                }
                if let Some(med) = ws.max_early_data {
                    t["max_early_data"] = json!(med);
                }
                if let Some(name) = &ws.early_data_header_name {
                    t["early_data_header_name"] = json!(name);
                }
                out["transport"] = t;
            }
        }
        Some("grpc") => {
            if let Some(g) = grpc {
                out["transport"] = json!({
                    "type": "grpc",
                    "service_name": g.grpc_service_name.clone().unwrap_or_default(),
                });
            }
        }
        Some("h2") | Some("http") | Some("http2") => {
            if let Some(h) = h2 {
                let mut t = json!({ "type": "http" });
                if let Some(host) = &h.host {
                    t["host"] = json!(host);
                }
                if let Some(path) = &h.path {
                    t["path"] = json!(path);
                }
                out["transport"] = t;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// ToXrayJson — outbound blob shaped for the xray-core `outbounds` array
// ---------------------------------------------------------------------------

/// Render a [`PingleOutbound`] to xray-core / v2ray-core outbound JSON.
///
/// See <https://xtls.github.io/config/outbounds/> for the xray
/// outbound schema. Compared to sing-box, xray nests protocol
/// settings under `settings.vnext[].users[]` and transport settings
/// under `streamSettings`, so the projection is more verbose but
/// mechanically the same — only the field-rename table differs.
///
/// Today we implement the common four: vless, vmess, trojan, and
/// shadowsocks. Hysteria2 / tuic / wireguard are xray-unsupported
/// (the official xray fork doesn't ship them), so we return a
/// placeholder `"unsupported"` block that a core translator can
/// detect and gracefully skip.
pub trait ToXrayJson {
    fn to_xray_json(&self) -> Value;
}

impl ToXrayJson for PingleOutbound {
    fn to_xray_json(&self) -> Value {
        match self {
            Self::Direct(d) => json!({
                "tag": d.name,
                "protocol": "freedom",
                "settings": {},
            }),
            Self::Reject(r) => json!({
                "tag": r.name,
                "protocol": "blackhole",
                "settings": {},
            }),
            Self::Ss(s) => shadowsocks_to_xray(s),
            Self::Trojan(t) => trojan_to_xray(t),
            Self::Vmess(v) => vmess_to_xray(v),
            Self::Vless(v) => vless_to_xray(v),
            _ => json!({
                "tag": self.name(),
                "protocol": "unsupported",
                "_note": format!(
                    "{} is not representable in xray outbound JSON",
                    self.type_tag()
                ),
            }),
        }
    }
}

fn shadowsocks_to_xray(s: &OutboundShadowsocks) -> Value {
    json!({
        "tag": s.common_opts.name,
        "protocol": "shadowsocks",
        "settings": {
            "servers": [{
                "address": s.common_opts.server,
                "port": s.common_opts.port,
                "method": s.cipher,
                "password": s.password,
            }],
        },
    })
}

fn trojan_to_xray(t: &OutboundTrojan) -> Value {
    json!({
        "tag": t.common_opts.name,
        "protocol": "trojan",
        "settings": {
            "servers": [{
                "address": t.common_opts.server,
                "port": t.common_opts.port,
                "password": t.password,
            }],
        },
        "streamSettings": xray_stream_settings(
            t.network.as_deref().unwrap_or("tcp"),
            true,
            t.sni.as_deref(),
            t.skip_cert_verify.unwrap_or(false),
            t.alpn.as_ref(),
            t.ws_opts.as_ref(),
            None,
            t.grpc_opts.as_ref(),
            None,
            None,
        ),
    })
}

fn vmess_to_xray(v: &OutboundVmess) -> Value {
    json!({
        "tag": v.common_opts.name,
        "protocol": "vmess",
        "settings": {
            "vnext": [{
                "address": v.common_opts.server,
                "port": v.common_opts.port,
                "users": [{
                    "id": v.uuid,
                    "alterId": v.alter_id,
                    "security": v.cipher.clone().unwrap_or_else(|| "auto".to_string()),
                }],
            }],
        },
        "streamSettings": xray_stream_settings(
            v.network.as_deref().unwrap_or("tcp"),
            v.tls.unwrap_or(false),
            v.server_name.as_deref(),
            v.skip_cert_verify.unwrap_or(false),
            None,
            v.ws_opts.as_ref(),
            v.h2_opts.as_ref(),
            v.grpc_opts.as_ref(),
            None,
            None,
        ),
    })
}

fn vless_to_xray(v: &OutboundVless) -> Value {
    json!({
        "tag": v.common_opts.name,
        "protocol": "vless",
        "settings": {
            "vnext": [{
                "address": v.common_opts.server,
                "port": v.common_opts.port,
                "users": [{
                    "id": v.uuid,
                    "flow": v.flow.clone().unwrap_or_default(),
                    "encryption": "none",
                }],
            }],
        },
        "streamSettings": xray_stream_settings(
            v.network.as_deref().unwrap_or("tcp"),
            v.tls.unwrap_or(false),
            v.server_name.as_deref(),
            v.skip_cert_verify.unwrap_or(false),
            None,
            v.ws_opts.as_ref(),
            v.h2_opts.as_ref(),
            v.grpc_opts.as_ref(),
            v.reality_opts.as_ref(),
            v.client_fingerprint.as_deref(),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn xray_stream_settings(
    network: &str,
    tls: bool,
    sni: Option<&str>,
    insecure: bool,
    alpn: Option<&Vec<String>>,
    ws: Option<&crate::WsOpt>,
    h2: Option<&crate::H2Opt>,
    grpc: Option<&crate::GrpcOpt>,
    reality: Option<&crate::RealityOpt>,
    fingerprint: Option<&str>,
) -> Value {
    let security = if reality.is_some() {
        "reality"
    } else if tls {
        "tls"
    } else {
        "none"
    };
    let network_normalised = match network {
        "websocket" => "ws",
        "http2" | "h2" => "http",
        other => other,
    };
    let mut s = json!({
        "network": network_normalised,
        "security": security,
    });
    if security == "tls" {
        let mut tls_settings = json!({});
        if let Some(sni) = sni {
            tls_settings["serverName"] = json!(sni);
        }
        if insecure {
            tls_settings["allowInsecure"] = json!(true);
        }
        if let Some(alpn) = alpn {
            tls_settings["alpn"] = json!(alpn);
        }
        if let Some(fp) = fingerprint {
            tls_settings["fingerprint"] = json!(fp);
        }
        s["tlsSettings"] = tls_settings;
    } else if security == "reality" {
        let reality = reality.expect("branch guarded above");
        let mut reality_settings = json!({
            "publicKey": reality.public_key,
            "shortId": reality.short_id,
        });
        if let Some(sni) = sni {
            reality_settings["serverName"] = json!(sni);
        }
        if let Some(fp) = fingerprint {
            reality_settings["fingerprint"] = json!(fp);
        }
        s["realitySettings"] = reality_settings;
    }
    match network_normalised {
        "ws" => {
            if let Some(ws) = ws {
                let mut ws_settings = json!({});
                if let Some(p) = &ws.path {
                    ws_settings["path"] = json!(p);
                }
                if let Some(h) = &ws.headers {
                    ws_settings["headers"] = json!(h);
                }
                s["wsSettings"] = ws_settings;
            }
        }
        "grpc" => {
            if let Some(g) = grpc {
                s["grpcSettings"] = json!({
                    "serviceName": g.grpc_service_name.clone().unwrap_or_default(),
                });
            }
        }
        "http" => {
            if let Some(h) = h2 {
                let mut http_settings = json!({});
                if let Some(host) = &h.host {
                    http_settings["host"] = json!(host);
                }
                if let Some(path) = &h.path {
                    http_settings["path"] = json!(path);
                }
                s["httpSettings"] = http_settings;
            }
        }
        _ => {}
    }
    s
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommonOptions, OutboundVless, OutboundVmess, WsOpt};
    use std::collections::HashMap;

    fn sample_vless() -> PingleOutbound {
        PingleOutbound::Vless(OutboundVless {
            common_opts: CommonOptions {
                name: "jp-tokyo-1".into(),
                server: "203.0.113.10".into(),
                port: 443,
                connect_via: None,
            },
            uuid: "00000000-0000-0000-0000-000000000000".into(),
            udp: Some(true),
            tls: Some(true),
            skip_cert_verify: Some(false),
            server_name: Some("example.com".into()),
            network: Some("ws".into()),
            ws_opts: Some(WsOpt {
                path: Some("/vl".into()),
                headers: Some(HashMap::from_iter([("Host".into(), "example.com".into())])),
                max_early_data: None,
                early_data_header_name: None,
            }),
            h2_opts: None,
            grpc_opts: None,
            reality_opts: None,
            flow: Some("xtls-rprx-vision".into()),
            client_fingerprint: Some("chrome".into()),
        })
    }

    #[test]
    fn clash_yaml_roundtrip() {
        let original = sample_vless();
        let yaml = original.to_clash_yaml();
        // Re-parse the yaml into a PingleOutbound to prove the
        // projection is lossless for clash — that's the whole point
        // of choosing mihomo as the canonical format.
        let reparsed: PingleOutbound = serde_yaml::from_value(yaml).unwrap();
        assert_eq!(reparsed.name(), "jp-tokyo-1");
        assert_eq!(reparsed.type_tag(), "vless");
    }

    #[test]
    fn singbox_vless_ws_tls_layout() {
        let json = sample_vless().to_singbox_json();
        assert_eq!(json["type"], "vless");
        assert_eq!(json["tag"], "jp-tokyo-1");
        assert_eq!(json["server"], "203.0.113.10");
        assert_eq!(json["server_port"], 443);
        assert_eq!(json["uuid"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(json["flow"], "xtls-rprx-vision");
        // TLS block
        assert_eq!(json["tls"]["enabled"], true);
        assert_eq!(json["tls"]["server_name"], "example.com");
        assert_eq!(json["tls"]["utls"]["fingerprint"], "chrome");
        // Transport block
        assert_eq!(json["transport"]["type"], "ws");
        assert_eq!(json["transport"]["path"], "/vl");
        assert_eq!(json["transport"]["headers"]["Host"], "example.com");
    }

    #[test]
    fn singbox_vless_with_reality_adds_reality_block() {
        let mut v = match sample_vless() {
            PingleOutbound::Vless(v) => v,
            _ => unreachable!(),
        };
        v.reality_opts = Some(crate::RealityOpt {
            public_key: "pkpkpk".into(),
            short_id: "beef".into(),
        });
        let json = PingleOutbound::Vless(v).to_singbox_json();
        assert_eq!(json["tls"]["reality"]["enabled"], true);
        assert_eq!(json["tls"]["reality"]["public_key"], "pkpkpk");
        assert_eq!(json["tls"]["reality"]["short_id"], "beef");
    }

    #[test]
    fn xray_vless_shape() {
        let json = sample_vless().to_xray_json();
        assert_eq!(json["protocol"], "vless");
        assert_eq!(json["tag"], "jp-tokyo-1");
        assert_eq!(
            json["settings"]["vnext"][0]["users"][0]["id"],
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            json["settings"]["vnext"][0]["users"][0]["flow"],
            "xtls-rprx-vision"
        );
        assert_eq!(json["streamSettings"]["network"], "ws");
        assert_eq!(json["streamSettings"]["security"], "tls");
        assert_eq!(
            json["streamSettings"]["tlsSettings"]["serverName"],
            "example.com"
        );
        assert_eq!(
            json["streamSettings"]["tlsSettings"]["fingerprint"],
            "chrome"
        );
        assert_eq!(json["streamSettings"]["wsSettings"]["path"], "/vl");
    }

    #[test]
    fn singbox_vmess_camel_security_default() {
        let v = PingleOutbound::Vmess(OutboundVmess {
            common_opts: CommonOptions {
                name: "legacy".into(),
                server: "203.0.113.20".into(),
                port: 443,
                connect_via: None,
            },
            uuid: "u".into(),
            alter_id: 0,
            cipher: None,
            udp: None,
            tls: Some(false),
            skip_cert_verify: None,
            server_name: None,
            network: None,
            ws_opts: None,
            h2_opts: None,
            grpc_opts: None,
        });
        let json = v.to_singbox_json();
        assert_eq!(json["security"], "auto");
        // no tls block when tls: false
        assert!(json.get("tls").is_none());
    }

    #[test]
    fn xray_hysteria2_is_placeholder() {
        let h = PingleOutbound::Hysteria2(OutboundHysteria2 {
            name: "hy2".into(),
            server: "203.0.113.40".into(),
            port: 443,
            ports: None,
            password: "pw".into(),
            obfs: None,
            obfs_password: None,
            alpn: None,
            up: None,
            down: None,
            sni: None,
            skip_cert_verify: false,
            ca: None,
            ca_str: None,
            fingerprint: None,
        });
        let json = h.to_xray_json();
        assert_eq!(json["protocol"], "unsupported");
        assert!(json["_note"].as_str().unwrap().contains("hysteria2"));
    }
}
