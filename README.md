<!--
SPDX-FileCopyrightText: 2026 Nico Ueberfeldt - ueni
SPDX-License-Identifier: MIT
-->

# Usage MCP server

A small Rust [Model Context Protocol (MCP)](https://modelcontextprotocol.io/)
server that exposes Codex usage and rate-limit information through one tool:
`get_usage`.

The server supports local stdio and Streamable HTTP transports. It reads the
Codex CLI access token from a read-only `auth.json` mount, calls the Codex usage
endpoint, and returns the original JSON response together with a concise
summary. Successful responses are cached for 30 seconds by default.

## Features

- `get_usage` for weekly, monthly, credit, request, and rate-limit statistics.
- stdio transport for clients that launch a local command.
- Streamable HTTP transport at `/mcp` for long-running local services.
- Readiness reports at `/healthz` and `/mcp/healthz`.
- Opt-in provider-neutral configuration through `USAGE_SECRETS_FILE`.
- No credential output or logging by the server.

## Requirements

- Docker with BuildKit and Docker Compose v2 for the container workflow.
- A Codex CLI login at `$HOME/.codex/auth.json` when using the default Codex
  configuration.

If your network intercepts crates.io TLS with a private CA, pass the host CA
to the build as shown below. The CA is used only during the builder stage.

## Quick start

### Stdio

```sh
docker build \
  --secret id=host_ca,src=/etc/ssl/certs/ca-certificates.crt \
  -t usage-mcp-server:local .

docker run --rm -i --read-only --user 1000:1000 \
  -e CODEX_AUTH_FILE=/mnt/codex/auth.json \
  -v "$HOME/.codex:/mnt/codex:ro" \
  usage-mcp-server:local
```

### Streamable HTTP with Docker Compose

The HTTP example uses port `8001` so it can run beside
`mcp-context-manager` on port `8000`.

```sh
docker build \
  --secret id=host_ca,src=/etc/ssl/certs/ca-certificates.crt \
  -t usage-mcp-server:local .
docker compose up -d --force-recreate --wait --wait-timeout 30 usage-mcp-server
docker compose ps usage-mcp-server
```

The MCP endpoint is `http://127.0.0.1:8001/mcp`. Compose checks
`http://127.0.0.1:8001/mcp/healthz` and keeps the service running in the
background. Stop it with:

```sh
docker compose down
```

## Codex configuration

Register the HTTP server globally for Codex:

```sh
codex mcp add usage --url http://127.0.0.1:8001/mcp
```

For an explicit user-level configuration in `~/.codex/config.toml`:

```toml
[mcp_servers.usage]
url = "http://127.0.0.1:8001/mcp"
default_tools_approval_mode = "approve"
enabled_tools = ["get_usage"]
```

The HTTP container must be running before Codex can call `get_usage`.

For a client that launches the stdio server directly, use an absolute host
path because JSON argument arrays do not expand `${HOME}`:

```json
{
  "mcpServers": {
    "usage": {
      "command": "docker",
      "args": [
        "run", "--rm", "-i", "--read-only", "--user", "1000:1000",
        "-e", "CODEX_AUTH_FILE=/mnt/codex/auth.json",
        "-v", "/absolute/path/.codex:/mnt/codex:ro",
        "usage-mcp-server:local"
      ]
    }
  }
}
```

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `MCP_TRANSPORT` | `stdio` | Set to `streamable-http` or `http` for the TCP-backed HTTP transport. |
| `HOST` | `127.0.0.1` | HTTP bind address. Use `0.0.0.0` inside a container. |
| `PORT` | `8000` | HTTP listen port. |
| `CODEX_AUTH_FILE` | `$CODEX_HOME/auth.json` | Path to the Codex CLI authentication file. |
| `CODEX_HOME` | `$HOME/.codex` | Codex directory used when `CODEX_AUTH_FILE` is unset. |
| `CODEX_USAGE_URL` | `https://chatgpt.com/backend-api/wham/usage` | Usage API endpoint. |
| `USAGE_SECRETS_FILE` | unset | Explicitly selects provider-neutral JSON configuration. |
| `USAGE_CACHE_TTL_SECONDS` | `30` | Successful usage response cache lifetime. |
| `USAGE_REQUEST_TIMEOUT_SECONDS` | `10` | Upstream usage request timeout. |

When `USAGE_SECRETS_FILE` is set, the file must contain an endpoint and either
`bearer_token` or `api_key`:

```json
{
  "url": "https://usage.example.test/v1/usage",
  "bearer_token": "replace-me",
  "headers": {
    "X-Workspace": "example"
  }
}
```

Use `api_key` instead of `bearer_token` when the provider expects an
`X-API-Key` header. `headers` is optional.

## MCP and HTTP surface

The server supports `initialize`, `tools/list`, `ping`, and `tools/call` for
`get_usage` over both transports.

| Endpoint | Purpose |
| --- | --- |
| `GET /healthz` | Health and version report. |
| `GET /mcp/healthz` | Health report under the MCP base path. |
| `POST /mcp` | Streamable HTTP MCP endpoint. |

Health responses include `status`, `ok`, `server`, `version`, and `native`.

## Development

Run the Rust tests in a Rust toolchain environment:

```sh
cargo test
```

Validate the Compose configuration without starting it:

```sh
docker compose config
```

The VS Code task **Build and start usage MCP HTTP server** builds the image,
starts Compose detached, waits for the healthcheck, and prints the service
status.

## Security notes

- Mount `auth.json` read-only and never commit it or a provider secrets file.
- The default HTTP bind address is loopback; the Compose deployment publishes
  only `127.0.0.1:8001`.
- The HTTP transport does not provide remote authentication. Keep it on a
  trusted local interface or place it behind an authenticated reverse proxy
  before exposing it beyond the local host.
- Credentials are used only for the upstream request and are not included in
  tool responses or server logs.

## License

This project is licensed under the [MIT License](LICENSE). SPDX metadata and
the REUSE configuration are provided in `REUSE.toml`.
