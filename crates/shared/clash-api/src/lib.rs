use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};
use url::Url;

#[derive(Debug)]
pub enum ClashApiError {
    InvalidController(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    WebSocket(tungstenite::Error),
    HttpStatus { status: u16, body: String },
    Protocol(String),
}

impl fmt::Display for ClashApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidController(value) => write!(f, "invalid clash controller: {value}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
            Self::WebSocket(error) => write!(f, "{error}"),
            Self::HttpStatus { status, body } => write!(f, "http {status}: {body}"),
            Self::Protocol(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ClashApiError {}

impl From<std::io::Error> for ClashApiError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for ClashApiError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<tungstenite::Error> for ClashApiError {
    fn from(value: tungstenite::Error) -> Self {
        Self::WebSocket(value)
    }
}

pub type Result<T> = std::result::Result<T, ClashApiError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClashApiClient {
    host: String,
    port: u16,
    timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySelector {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub now: Option<String>,
    #[serde(default)]
    pub all: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficSnapshot {
    pub up: u64,
    pub down: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionMetadata {
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub source_ip: Option<String>,
    #[serde(default)]
    pub destination_ip: Option<String>,
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRecord {
    pub id: String,
    #[serde(default)]
    pub download: u64,
    #[serde(default)]
    pub upload: u64,
    #[serde(default)]
    pub metadata: Option<ConnectionMetadata>,
    #[serde(default)]
    pub chains: Vec<String>,
    #[serde(default)]
    pub rule: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionsSnapshot {
    #[serde(default)]
    pub connections: Vec<ConnectionRecord>,
    #[serde(default)]
    pub download_total: u64,
    #[serde(default)]
    pub upload_total: u64,
    #[serde(default)]
    pub memory: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub version: String,
}

pub struct JsonWebSocketStream<T> {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    _marker: std::marker::PhantomData<T>,
}

impl<T> JsonWebSocketStream<T>
where
    T: DeserializeOwned,
{
    pub fn recv(&mut self) -> Result<T> {
        loop {
            match self.socket.read()? {
                Message::Text(text) => return Ok(serde_json::from_str::<T>(&text)?),
                Message::Binary(bytes) => return Ok(serde_json::from_slice::<T>(&bytes)?),
                Message::Ping(payload) => self.socket.send(Message::Pong(payload))?,
                Message::Pong(_) => continue,
                Message::Close(_) => {
                    return Err(ClashApiError::Protocol(
                        "clash websocket closed".to_string(),
                    ));
                }
                Message::Frame(_) => continue,
            }
        }
    }
}

impl ClashApiClient {
    pub fn new(controller: &str) -> Result<Self> {
        let (host, port) = parse_controller(controller)?;
        Ok(Self {
            host,
            port,
            timeout: Duration::from_secs(2),
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn get_proxy_selectors(&self) -> Result<Vec<ProxySelector>> {
        #[derive(Deserialize)]
        struct ProxiesResponse {
            proxies: std::collections::BTreeMap<String, serde_json::Value>,
        }

        let response: ProxiesResponse =
            serde_json::from_value(self.http_request("GET", "/proxies", None)?)?;
        let mut selectors = Vec::new();
        for (name, value) in response.proxies {
            let Some(object) = value.as_object() else {
                continue;
            };
            let kind = object
                .get("type")
                .and_then(|entry| entry.as_str())
                .unwrap_or_default();
            if !kind.eq_ignore_ascii_case("selector") {
                continue;
            }
            selectors.push(ProxySelector {
                name,
                kind: kind.to_string(),
                now: object
                    .get("now")
                    .and_then(|entry| entry.as_str())
                    .map(ToOwned::to_owned),
                all: object
                    .get("all")
                    .and_then(|entry| entry.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| entry.as_str())
                    .map(ToOwned::to_owned)
                    .collect(),
            });
        }
        Ok(selectors)
    }

    pub fn get_active_proxy(&self, selector_name: &str) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct ProxyDetails {
            #[serde(default)]
            now: Option<String>,
        }

        let encoded = encode_path(selector_name);
        let body = self.http_request("GET", &format!("/proxies/{encoded}"), None)?;
        Ok(serde_json::from_value::<ProxyDetails>(body)?.now)
    }

    pub fn set_active_proxy(&self, selector_name: &str, proxy_name: &str) -> Result<()> {
        let encoded = encode_path(selector_name);
        let payload = serde_json::json!({ "name": proxy_name }).to_string();
        let _ = self.http_request("PUT", &format!("/proxies/{encoded}"), Some(&payload))?;
        Ok(())
    }

    pub fn get_connections(&self) -> Result<ConnectionsSnapshot> {
        Ok(serde_json::from_value(self.http_request(
            "GET",
            "/connections",
            None,
        )?)?)
    }

    pub fn get_version(&self) -> Result<VersionInfo> {
        Ok(serde_json::from_value(
            self.http_request("GET", "/version", None)?,
        )?)
    }

    pub fn subscribe_traffic(&self) -> Result<JsonWebSocketStream<TrafficSnapshot>> {
        let url = self.websocket_url("/traffic")?;
        let (socket, _) = connect(url.as_str())?;
        Ok(JsonWebSocketStream {
            socket,
            _marker: std::marker::PhantomData,
        })
    }

    pub fn subscribe_connections(&self) -> Result<JsonWebSocketStream<ConnectionsSnapshot>> {
        let url = self.websocket_url("/connections")?;
        let (socket, _) = connect(url.as_str())?;
        Ok(JsonWebSocketStream {
            socket,
            _marker: std::marker::PhantomData,
        })
    }

    fn websocket_url(&self, path: &str) -> Result<Url> {
        Url::parse(&format!("ws://{}:{}{}", self.host, self.port, path))
            .map_err(|error| ClashApiError::InvalidController(error.to_string()))
    }

    #[allow(dead_code)]
    fn http_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<serde_json::Value> {
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))?;
        stream.set_read_timeout(Some(self.timeout)).ok();
        stream.set_write_timeout(Some(self.timeout)).ok();
        let payload = body.unwrap_or("");
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            self.host,
            payload.len(),
            payload
        );
        stream.write_all(request.as_bytes())?;
        stream.flush()?;

        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        let (head, body) = response
            .split_once("\r\n\r\n")
            .ok_or_else(|| ClashApiError::Protocol("malformed http response".into()))?;
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(ClashApiError::HttpStatus {
                status,
                body: body.to_string(),
            });
        }
        if body.trim().is_empty() {
            return Ok(serde_json::Value::Null);
        }
        Ok(serde_json::from_str(body)?)
    }
}

fn parse_controller(controller: &str) -> Result<(String, u16)> {
    let trimmed = controller.trim();
    let (host, port) = trimmed
        .rsplit_once(':')
        .ok_or_else(|| ClashApiError::InvalidController(trimmed.to_string()))?;
    let port = port
        .parse::<u16>()
        .map_err(|_| ClashApiError::InvalidController(trimmed.to_string()))?;
    Ok((host.to_string(), port))
}

fn encode_path(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;
    use tungstenite::accept;

    #[test]
    fn fetches_proxy_selectors_from_rest_api() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert!(first.starts_with("GET /proxies HTTP/1.1"));
            write_http_json(
                stream,
                serde_json::json!({
                    "proxies": {
                        "🌐 Proxy": {
                            "name": "🌐 Proxy",
                            "type": "Selector",
                            "now": "🇩🇪 Germany",
                            "all": ["🇳🇱 Netherlands", "🇩🇪 Germany"]
                        },
                        "↔️ Direct": {
                            "name": "↔️ Direct",
                            "type": "Direct"
                        }
                    }
                }),
            );
        });

        let selectors = ClashApiClient::new(&format!("127.0.0.1:{port}"))
            .unwrap()
            .get_proxy_selectors()
            .unwrap();

        handle.join().unwrap();
        assert_eq!(selectors.len(), 1);
        assert_eq!(selectors[0].name, "🌐 Proxy");
        assert_eq!(selectors[0].now.as_deref(), Some("🇩🇪 Germany"));
        assert_eq!(selectors[0].all.len(), 2);
    }

    #[test]
    fn gets_and_sets_active_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            for expected in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                if expected == 0 {
                    assert!(first.starts_with(&format!(
                        "GET /proxies/{} HTTP/1.1",
                        encode_path("🌐 Proxy")
                    )));
                    write_http_json(
                        stream,
                        serde_json::json!({
                            "name": "🌐 Proxy",
                            "type": "Selector",
                            "now": "🇳🇱 Netherlands",
                            "all": ["🇳🇱 Netherlands", "🇩🇪 Germany"]
                        }),
                    );
                } else {
                    assert!(first.starts_with(&format!(
                        "PUT /proxies/{} HTTP/1.1",
                        encode_path("🌐 Proxy")
                    )));
                    write_http_json(stream, serde_json::json!({}));
                }
            }
        });

        let client = ClashApiClient::new(&format!("127.0.0.1:{port}")).unwrap();
        let current = client.get_active_proxy("🌐 Proxy").unwrap();
        client.set_active_proxy("🌐 Proxy", "🇩🇪 Germany").unwrap();

        handle.join().unwrap();
        assert_eq!(current.as_deref(), Some("🇳🇱 Netherlands"));
    }

    #[test]
    fn subscribes_to_traffic_websocket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = accept(stream).unwrap();
            socket
                .send(Message::Text(r#"{"up":321,"down":654}"#.into()))
                .unwrap();
        });

        let mut stream = ClashApiClient::new(&format!("127.0.0.1:{port}"))
            .unwrap()
            .subscribe_traffic()
            .unwrap();
        let snapshot = stream.recv().unwrap();

        handle.join().unwrap();
        assert_eq!(snapshot.up, 321);
        assert_eq!(snapshot.down, 654);
    }

    #[test]
    fn subscribes_to_connections_websocket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = accept(stream).unwrap();
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "download_total": 2048,
                        "upload_total": 1024,
                        "memory": 8192,
                        "connections": [
                            { "id": "abc", "download": 10, "upload": 20, "chains": ["🇩🇪 Germany"] }
                        ]
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
        });

        let mut stream = ClashApiClient::new(&format!("127.0.0.1:{port}"))
            .unwrap()
            .subscribe_connections()
            .unwrap();
        let snapshot = stream.recv().unwrap();

        handle.join().unwrap();
        assert_eq!(snapshot.download_total, 2048);
        assert_eq!(snapshot.upload_total, 1024);
        assert_eq!(snapshot.memory, 8192);
        assert_eq!(snapshot.connections.len(), 1);
        assert_eq!(snapshot.connections[0].id, "abc");
    }

    fn write_http_json(mut stream: TcpStream, body: serde_json::Value) {
        let text = body.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            text.len(),
            text
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.flush().unwrap();
    }
}
