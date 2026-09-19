use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

const DEFAULT_SECRETS_FILE: &str = "/run/secrets/usage.json";
const DEFAULT_CACHE_TTL_SECONDS: u64 = 30;
const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 10;

#[derive(Debug, Deserialize)]
struct Secrets {
    url: String,
    bearer_token: Option<String>,
    api_key: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

struct Server {
    client: Client,
    secrets: Secrets,
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
        let secrets_path = env::var("USAGE_SECRETS_FILE")
            .unwrap_or_else(|_| DEFAULT_SECRETS_FILE.to_owned());
        let secrets_text = fs::read_to_string(&secrets_path)
            .map_err(|error| format!("could not read usage secrets file: {error}"))?;
        let secrets: Secrets = serde_json::from_str(&secrets_text)
            .map_err(|error| format!("could not parse usage secrets file: {error}"))?;

        if secrets.url.is_empty() {
            return Err("usage endpoint URL is empty".to_owned());
        }
        if secrets.bearer_token.is_none() && secrets.api_key.is_none() {
            return Err("usage secrets must contain bearer_token or api_key".to_owned());
        }

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
            secrets,
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
        let mut request = self.client.get(&self.secrets.url);

        if let Some(token) = &self.secrets.bearer_token {
            request = request.bearer_auth(token);
        } else if let Some(api_key) = &self.secrets.api_key {
            request = request.header("X-API-Key", api_key);
        }
        for (name, value) in &self.secrets.headers {
            request = request.header(name, value);
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
    let mut server = Server::from_environment()?;
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
        let id = request.id.unwrap_or(Value::Null);
        let response = match request.method.as_str() {
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
            "tools/call" => handle_tool_call(&mut server, id, request.params),
            _ => json_rpc_error(id, -32601, "method not found"),
        };
        write_response(&mut stdout, response)?;
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    id: Option<Value>,
    method: String,
    params: Option<Value>,
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
}
