//! JSON-RPC 2.0 envelope types.
//!
//! Spec: <https://www.jsonrpc.org/specification>
//!
//! Only the parts of the spec we actually use are modelled. Notifications
//! (push events) are requests with no `id`. Batch requests are not supported —
//! the protocol is one-message-per-line.
//!
//! Wire format on the socket: **newline-delimited JSON**. Every message is a
//! single JSON object terminated by `\n`. This makes the framing trivial in
//! every language and easy to debug with `nc`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Inbound request from a client.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    /// Always `"2.0"`. Validated on deserialization.
    pub jsonrpc: String,
    /// Optional id. `None` means notification — no response will be sent.
    #[serde(default)]
    pub id: Option<Value>,
    /// Method name, e.g. `"vpn.connect"`.
    pub method: String,
    /// Method parameters. Defaults to `null` if absent.
    #[serde(default)]
    pub params: Value,
}

/// Outbound response to a request.
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    /// Echoes the request id. Required by the spec — even errors must echo it.
    pub id: Value,
    /// Either `result` or `error`, never both.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Outbound notification (push event from daemon to subscribed clients).
///
/// Notifications have no `id`. The client cannot reply to them.
#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub jsonrpc: &'static str,
    pub method: String,
    pub params: Value,
}

/// JSON-RPC error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    /// Optional structured detail. We use this to carry the stable
    /// `VpnError::code()` string and the `recoverable()` flag so clients
    /// can offer retry buttons without parsing the message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ---------------------------------------------------------------------------
// Standard JSON-RPC error codes
// ---------------------------------------------------------------------------

/// Invalid JSON received by the server. The server cannot parse it.
pub const PARSE_ERROR: i64 = -32700;
/// JSON is not a valid request object.
pub const INVALID_REQUEST: i64 = -32600;
/// Method does not exist or is not available.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// Invalid method parameters.
pub const INVALID_PARAMS: i64 = -32602;
/// Internal server error (panic, mutex poison, etc.).
pub const INTERNAL_ERROR: i64 = -32603;
/// Application-level error (the method ran but returned a `VpnError`).
/// Spec reserves -32000..-32099 for "implementation-defined server errors".
pub const APPLICATION_ERROR: i64 = -32000;

impl Response {
    /// Build a successful response.
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Build an error response. `id` may be `Value::Null` for parse errors
    /// where the original id could not be recovered.
    pub fn err(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data,
            }),
        }
    }
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            method: method.into(),
            params,
        }
    }
}

impl Request {
    /// Returns `true` if this is a notification (no id) — no response should be sent.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Returns the id, defaulting to `Null` for notifications/parse errors so
    /// it can still be echoed in an error response if needed.
    pub fn id_or_null(&self) -> Value {
        self.id.clone().unwrap_or(Value::Null)
    }
}
