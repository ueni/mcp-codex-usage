use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};

const DEFAULT_CODEX_HOME: &str = "/home/nonroot/.codex";
const DEFAULT_CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const DEFAULT_CACHE_TTL_SECONDS: u64 = 30;
const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 10;
const DEFAULT_HTTP_HOST: &str = "127.0.0.1";
const DEFAULT_HTTP_PORT: u16 = 8000;

#[derive(Debug, Deserialize)]
struct Secrets {
    url: String,
    bearer_token: Option<String>,
    api_key: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

enum UsageConfig {
    Codex {
        url: String,
        bearer_token: String,
    },
    Custom(Secrets),
}

#[derive(Debug, PartialEq, Eq)]
enum UsageSourceKind {
    Codex,
    Custom,
}

struct Server {
    client: Client,
    config: UsageConfig,
    cache_ttl: Duration,
    cache: Option<CacheEntry>,
}

struct CacheEntry {
    fetched_at: Instant,
    data: Value,
}

#[derive(Debug, Serialize)]
struct Metric {
    metric: String,
    value: Value,
    path: String,
}

impl Server {
    fn from_environment() -> Result<Self, String> {
        let custom_path = match env::var("USAGE_SECRETS_FILE") {
            Ok(path) => Some(path),
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err("USAGE_SECRETS_FILE is not valid UTF-8".to_owned())
            }
        };
        let config = match usage_source_kind(custom_path.as_deref()) {
            UsageSourceKind::Custom => {
                load_custom_config(custom_path.as_deref().unwrap_or_default())?
            }
            UsageSourceKind::Codex => load_codex_config()?,
        };

        let timeout_seconds = parse_env_u64(
            "USAGE_REQUEST_TIMEOUT_SECONDS",
            DEFAULT_REQUEST_TIMEOUT_SECONDS,
        )?;
        let cache_ttl_seconds =
            parse_env_u64("USAGE_CACHE_TTL_SECONDS", DEFAULT_CACHE_TTL_SECONDS)?;
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .build()
            .map_err(|error| format!("could not create HTTP client: {error}"))?;

        Ok(Self {
            client,
            config,
            cache_ttl: Duration::from_secs(cache_ttl_seconds),
            cache: None,
        })
    }

    fn handle_call(&mut self, name: &str) -> Result<Value, String> {
        if name != "get_usage" {
            return Err(format!("unknown tool: {name}"));
        }

        let (data, cached) = if let Some(entry) = &self.cache {
            if entry.fetched_at.elapsed() < self.cache_ttl {
                (entry.data.clone(), true)
            } else {
                self.fetch_usage()?
            }
        } else {
            self.fetch_usage()?
        };

        Ok(json!({
            "summary": summarize_usage(&data),
            "data": data,
            "cached": cached,
        }))
    }

    fn fetch_usage(&mut self) -> Result<(Value, bool), String> {
        let mut request = match &self.config {
            UsageConfig::Codex { url, bearer_token } => {
                self.client.get(url).bearer_auth(bearer_token)
            }
            UsageConfig::Custom(secrets) => {
                let mut request = self.client.get(&secrets.url);
                if let Some(token) = &secrets.bearer_token {
                    request = request.bearer_auth(token);
                } else if let Some(api_key) = &secrets.api_key {
                    request = request.header("X-API-Key", api_key);
                }
                for (name, value) in &secrets.headers {
                    request = request.header(name, value);
                }
                request
            }
        };

        if let UsageConfig::Codex { .. } = &self.config {
            request = request.header("User-Agent", "usage-mcp-server");
        }

        let response = request
            .send()
            .map_err(|error| format!("usage endpoint request failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("usage endpoint returned HTTP {status}"));
        }
        let data = response
            .json::<Value>()
            .map_err(|error| format!("usage endpoint returned invalid JSON: {error}"))?;

        self.cache = Some(CacheEntry {
            fetched_at: Instant::now(),
            data: data.clone(),
        });
        Ok((data, false))
    }
}

fn usage_source_kind(custom_path: Option<&str>) -> UsageSourceKind {
    if custom_path.is_some() {
        UsageSourceKind::Custom
    } else {
        UsageSourceKind::Codex
    }
}

fn load_custom_config(path: &str) -> Result<UsageConfig, String> {
    let secrets_text = fs::read_to_string(path)
        .map_err(|error| format!("could not read usage secrets file: {error}"))?;
    let secrets: Secrets = serde_json::from_str(&secrets_text)
        .map_err(|error| format!("could not parse usage secrets file: {error}"))?;

    if secrets.url.is_empty() {
        return Err("usage endpoint URL is empty".to_owned());
    }
    if secrets.bearer_token.is_none() && secrets.api_key.is_none() {
        return Err("usage secrets must contain bearer_token or api_key".to_owned());
    }

    Ok(UsageConfig::Custom(secrets))
}

fn load_codex_config() -> Result<UsageConfig, String> {
    let auth_path = codex_auth_path()?;
    let auth_text = fs::read_to_string(&auth_path).map_err(|error| {
        format!(
            "could not read Codex auth file {}: {error}; mount auth.json read-only or set CODEX_AUTH_FILE",
            auth_path.display()
        )
    })?;
    let auth: Value = serde_json::from_str(&auth_text)
        .map_err(|error| format!("could not parse Codex auth file: {error}"))?;
    let bearer_token = extract_access_token(&auth).ok_or_else(|| {
        "Codex auth file does not contain tokens.access_token or access_token; OS keyring and ephemeral credentials are unavailable in this container".to_owned()
    })?;
    let url = env::var("CODEX_USAGE_URL")
        .unwrap_or_else(|_| DEFAULT_CODEX_USAGE_URL.to_owned());
    if url.is_empty() {
        return Err("CODEX_USAGE_URL is empty".to_owned());
    }

    Ok(UsageConfig::Codex {
        url,
        bearer_token: bearer_token.to_owned(),
    })
}

fn codex_auth_path() -> Result<PathBuf, String> {
    if let Ok(path) = env::var("CODEX_AUTH_FILE") {
        return Ok(PathBuf::from(path));
    }

    let home = env::var("CODEX_HOME")
        .or_else(|_| env::var("HOME").map(|home| format!("{home}/.codex")))
        .unwrap_or_else(|_| DEFAULT_CODEX_HOME.to_owned());
    Ok(PathBuf::from(home).join("auth.json"))
}

fn extract_access_token(auth: &Value) -> Option<&str> {
    auth.get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .or_else(|| auth.get("access_token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
}

fn parse_env_u64(name: &str, default: u64) -> Result<u64, String> {
    match env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| format!("{name} must be an unsigned integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid UTF-8")),
    }
}

fn summarize_usage(data: &Value) -> BTreeMap<String, Vec<Metric>> {
    let mut summary = BTreeMap::from([
        ("weekly".to_owned(), Vec::new()),
        ("monthly".to_owned(), Vec::new()),
        ("other".to_owned(), Vec::new()),
    ]);
    collect_metrics(data, "", &mut summary);
    summary.retain(|_, metrics| !metrics.is_empty());
    summary
}

fn collect_metrics(
    value: &Value,
    path: &str,
    summary: &mut BTreeMap<String, Vec<Metric>>,
) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = if path.is_empty() {
                    key.to_owned()
                } else {
                    format!("{path}.{key}")
                };
                if is_usage_metric(key) && is_scalar(child) {
                    let period = period_for_path(&child_path);
                    summary
                        .entry(period.to_owned())
                        .or_default()
                        .push(Metric {
                            metric: metric_name(key),
                            value: child.clone(),
                            path: child_path.clone(),
                        });
                } else if is_rate_limit_metric(key, path) && is_scalar(child) {
                    summary
                        .entry("weekly".to_owned())
                        .or_default()
                        .push(Metric {
                            metric: key.to_owned(),
                            value: child.clone(),
                            path: child_path.clone(),
                        });
                }
                collect_metrics(child, &child_path, summary);
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                collect_metrics(child, &format!("{path}[{index}]"), summary);
            }
        }
        _ => {}
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_usage_metric(key: &str) -> bool {
    let key = normalized(key);
    (key.contains("token") || key.contains("credit") || key.contains("request"))
        && (key.contains("used")
            || key.contains("remaining")
            || key.contains("available")
            || key.contains("limit")
            || key.contains("quota")
            || key == "tokens"
            || key == "credits"
            || key == "requests")
}

fn is_rate_limit_metric(key: &str, parent_path: &str) -> bool {
    normalized(parent_path).contains("primarywindow")
        && matches!(
            normalized(key).as_str(),
            "usedpercent"
                | "limitwindowseconds"
                | "resetafterseconds"
                | "resetat"
                | "limitreached"
        )
}

fn metric_name(key: &str) -> String {
    let normalized_key = normalized(key);
    let unit = if normalized_key.contains("credit") {
        "credits"
    } else if normalized_key.contains("request") {
        "requests"
    } else {
        "tokens"
    };
    let qualifier = if normalized_key.contains("remaining") || normalized_key.contains("available") {
        "remaining"
    } else if normalized_key.contains("limit") || normalized_key.contains("quota") {
        "limit"
    } else if normalized_key.contains("used") {
        "used"
    } else {
        "value"
    };
    format!("{unit}_{qualifier}")
}

fn period_for_path(path: &str) -> &'static str {
    let path = normalized(path);
    if path.contains("week") || path.contains("7day") || path.contains("sevenday") {
        "weekly"
    } else if path.contains("month") || path.contains("30day") || path.contains("thirtyday") {
        "monthly"
    } else {
        "other"
    }
}

fn main() -> Result<(), String> {
    if env::args().skip(1).any(|argument| argument == "--healthcheck") {
        return run_healthcheck();
    }

    let transport = transport_from_environment_and_args();
    let server = Server::from_environment()?;

    match transport.as_str() {
        "stdio" => run_stdio(server),
        "streamable-http" | "http" => run_http(server),
        other => Err(format!("unsupported MCP transport: {other}")),
    }
}

fn run_healthcheck() -> Result<(), String> {
    let port = parse_env_u16("PORT", DEFAULT_HTTP_PORT)?;
    let url = env::var("MCP_HEALTHCHECK_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{port}/mcp/healthz"));
    let client = Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| format!("could not create healthcheck client: {error}"))?;
    let response = client
        .get(&url)
        .send()
        .map_err(|error| format!("healthcheck request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("healthcheck returned HTTP {}", response.status()));
    }
    let report = response
        .json::<Value>()
        .map_err(|error| format!("healthcheck returned invalid JSON: {error}"))?;
    if report.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err("healthcheck report is not ok".to_owned())
    }
}

fn run_stdio(mut server: Server) -> Result<(), String> {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());

    for line in stdin.lock().lines() {
        let line = line.map_err(|error| format!("could not read stdin: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }

        let request: RpcRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write_response(&mut stdout, json_rpc_error(Value::Null, -32700, &error.to_string()))?;
                continue;
            }
        };

        if request.id.is_none() {
            continue;
        }
        if let Some(response) = handle_request(&mut server, request) {
            write_response(&mut stdout, response)?;
        }
    }

    Ok(())
}

fn run_http(server: Server) -> Result<(), String> {
    let host = env::var("HOST").unwrap_or_else(|_| DEFAULT_HTTP_HOST.to_owned());
    let port = parse_env_u16("PORT", DEFAULT_HTTP_PORT)?;
    let address: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|error| format!("HOST and PORT must form a valid socket address: {error}"))?;
    let state = HttpState {
        server: Arc::new(Mutex::new(server)),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not create Tokio runtime: {error}"))?;

    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|error| format!("could not bind MCP HTTP listener: {error}"))?;
        let app = Router::new()
            .route("/", get(http_root))
            .route("/healthz", get(http_healthz))
            .route("/mcp/healthz", get(http_healthz))
            .route("/mcp", post(http_mcp))
            .with_state(state);

        axum::serve(listener, app)
            .with_graceful_shutdown(http_shutdown_signal())
            .await
            .map_err(|error| format!("MCP HTTP server failed: {error}"))
    })
}

async fn http_shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn http_root() -> &'static str {
    "usage-mcp-server"
}

async fn http_healthz() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "ok": true,
        "server": "usage-mcp-server",
        "version": env!("CARGO_PKG_VERSION"),
        "native": true
    }))
}

#[derive(Clone)]
struct HttpState {
    server: Arc<Mutex<Server>>,
}

async fn http_mcp(
    State(state): State<HttpState>,
    Json(request): Json<RpcRequest>,
) -> Response {
    let response = match state.server.lock() {
        Ok(mut server) => handle_request(&mut server, request),
        Err(_) => Some(json_rpc_error(
            Value::Null,
            -32603,
            "MCP server state is unavailable",
        )),
    };

    match response {
        Some(response) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            Json(response),
        )
            .into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

fn transport_from_environment_and_args() -> String {
    let mut transport = env::var("MCP_TRANSPORT").unwrap_or_else(|_| "stdio".to_owned());
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--transport" {
            if let Some(value) = args.next() {
                transport = value;
            }
        }
    }
    transport
}

fn parse_env_u16(name: &str, default: u16) -> Result<u16, String> {
    match env::var(name) {
        Ok(value) => value
            .parse::<u16>()
            .map_err(|_| format!("{name} must be an unsigned 16-bit integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid UTF-8")),
    }
}

fn handle_request(server: &mut Server, request: RpcRequest) -> Option<Value> {
    let id = request.id?;
    Some(match request.method.as_str() {
        "initialize" => json_rpc_result(id, json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "usage-mcp-server", "version": env!("CARGO_PKG_VERSION")}
        })),
        "tools/list" => json_rpc_result(id, json!({
            "tools": [{
                "name": "get_usage",
                "description": "Fetch weekly, monthly, and other usage/token statistics.",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
            }]
        })),
        "tools/call" => handle_tool_call(server, id, request.params),
        "ping" => json_rpc_result(id, json!({})),
        _ => json_rpc_error(id, -32601, "method not found"),
    })
}

fn handle_tool_call(server: &mut Server, id: Value, params: Option<Value>) -> Value {
    let name = params
        .as_ref()
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str);
    let Some(name) = name else {
        return json_rpc_error(id, -32602, "tools/call requires a tool name");
    };

    match server.handle_call(name) {
        Ok(result) => json_rpc_result(
            id,
            json!({"content": [{"type": "text", "text": result.to_string()}]}),
        ),
        Err(error) => json_rpc_result(
            id,
            json!({
                "isError": true,
                "content": [{"type": "text", "text": error}]
            }),
        ),
    }
}

fn json_rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn json_rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn write_response(stdout: &mut impl Write, response: Value) -> Result<(), String> {
    serde_json::to_writer(&mut *stdout, &response)
        .map_err(|error| format!("could not encode response: {error}"))?;
    stdout
        .write_all(b"\n")
        .map_err(|error| format!("could not write response: {error}"))?;
    stdout
        .flush()
        .map_err(|error| format!("could not flush response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_current_codex_access_token() {
        let auth = json!({"tokens": {"access_token": "example"}});
        assert_eq!(extract_access_token(&auth), Some("example"));
    }

    #[test]
    fn extracts_legacy_codex_access_token() {
        let auth = json!({"access_token": "example"});
        assert_eq!(extract_access_token(&auth), Some("example"));
    }

    #[test]
    fn rejects_missing_or_empty_access_token() {
        assert_eq!(extract_access_token(&json!({})), None);
        assert_eq!(
            extract_access_token(&json!({"tokens": {"access_token": ""}})),
            None
        );
    }

    #[test]
    fn uses_codex_auth_by_default_and_custom_file_when_selected() {
        assert_eq!(usage_source_kind(None), UsageSourceKind::Codex);
        assert_eq!(
            usage_source_kind(Some("/run/secrets/usage.json")),
            UsageSourceKind::Custom
        );
    }

    #[test]
    fn summarizes_nested_weekly_and_monthly_values() {
        let data = json!({
            "usage": {
                "weekly": {"tokens_used": 12, "credits_remaining": 8},
                "monthly": {"token_limit": 1000}
            },
            "account": {"requests": 3}
        });

        let summary = summarize_usage(&data);
        assert_eq!(summary["weekly"].len(), 2);
        assert_eq!(summary["monthly"][0].metric, "tokens_limit");
        assert_eq!(summary["other"][0].metric, "requests_value");
    }

    #[test]
    fn ignores_non_usage_values() {
        let summary = summarize_usage(&json!({"week": {"status": "ok"}}));
        assert!(summary.is_empty());
    }

    #[test]
    fn summarizes_codex_primary_rate_limit_window() {
        let summary = summarize_usage(&json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 7,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 123,
                    "reset_at": 1790443131,
                    "limit_reached": false
                }
            }
        }));

        let weekly = &summary["weekly"];
        assert_eq!(weekly.len(), 5);
        for metric in [
            "used_percent",
            "limit_window_seconds",
            "reset_after_seconds",
            "reset_at",
            "limit_reached",
        ] {
            assert!(weekly.iter().any(|entry| entry.metric == metric));
        }
    }
}
