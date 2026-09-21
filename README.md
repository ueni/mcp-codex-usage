# Usage MCP server

This MCP server exposes one `get_usage` tool. By default it uses stdio; set `MCP_TRANSPORT=streamable-http` to expose the same MCP service over TCP using Streamable HTTP. It reads the Codex CLI access token from a mounted `auth.json`, calls the Codex usage endpoint, and returns the complete JSON response with a concise summary of weekly, monthly, and other token, credit, and request fields. Successful responses are cached for 30 seconds by default.

Build and start it with Docker. The Codex directory is mounted read-only, so the server can reuse the CLI login without copying or creating a secret file:

```sh
docker build -t usage-mcp-server .
docker run --rm -i --read-only --user 1000:1000 \
  -e CODEX_AUTH_FILE=/mnt/codex/auth.json \
  -v "$HOME/.codex:/mnt/codex:ro" \
  usage-mcp-server
```

To run the Streamable HTTP connector for Codex, bind the container port and
listen on all container interfaces:

```sh
docker run --rm --read-only --user 1000:1000 \
  -e MCP_TRANSPORT=streamable-http \
  -e HOST=0.0.0.0 \
  -e PORT=8001 \
  -e CODEX_AUTH_FILE=/mnt/codex/auth.json \
  -v "$HOME/.codex:/mnt/codex:ro" \
  -p 127.0.0.1:8001:8001 \
  usage-mcp-server
```

The example uses port `8001` so it can run beside mcp-context-manager on
port `8000`. The MCP endpoint is `http://127.0.0.1:8001/mcp`; `/healthz` is
available for readiness checks. `HOST` defaults to `127.0.0.1` and `PORT` to
`8000`.

The same service can be managed with Docker Compose after building the image:

```sh
docker build --secret id=host_ca,src=/etc/ssl/certs/ca-certificates.crt \
  -t usage-mcp-server:local .
docker compose up -d --force-recreate --wait --wait-timeout 30 usage-mcp-server
docker compose ps usage-mcp-server
```

Compose reports the service as healthy through `/mcp/healthz`.

Register it globally for Codex with:

```sh
codex mcp add usage --url http://127.0.0.1:8001/mcp
```

This writes the server to the user-level `~/.codex/config.toml`. The HTTP
container must be running before Codex can call `get_usage`.

If the host's network intercepts crates.io TLS with a private CA, pass that CA
to the builder as an optional BuildKit secret:

```sh
docker build \
  --secret id=host_ca,src=/etc/ssl/certs/ca-certificates.crt \
  -t usage-mcp-server .
```

The CA is trusted only during the builder stage and is not copied into the
runtime image. Omitting `--secret` keeps the normal build behavior.

To mount only the credential file, use `CODEX_AUTH_FILE`:

```sh
docker run --rm -i --read-only --user 1000:1000 \
  -e CODEX_AUTH_FILE=/run/secrets/auth.json \
  -v "$HOME/.codex/auth.json:/run/secrets/auth.json:ro" \
  usage-mcp-server
```

The endpoint defaults to `https://chatgpt.com/backend-api/wham/usage`. Set `CODEX_USAGE_URL` to override it. The server extracts `tokens.access_token` from the current Codex auth shape, with a top-level `access_token` fallback. It never emits or logs the token. Credentials stored only in an OS keyring or supplied ephemerally cannot be used because the container can read mounted files only.

The previous provider-neutral JSON configuration remains available when explicitly selected with `USAGE_SECRETS_FILE`:

```json
{
  "url": "https://usage.example.test/v1/usage",
  "bearer_token": "replace-me",
  "headers": {
    "X-Workspace": "example"
  }
}
```

```sh
docker run --rm -i --read-only --user 1000:1000 \
  -e USAGE_SECRETS_FILE=/run/secrets/usage.json \
  -v "$PWD/usage.json:/run/secrets/usage.json:ro" \
  usage-mcp-server
```

Use `api_key` instead of `bearer_token` when the provider expects an `X-API-Key` header. `headers` is optional. `USAGE_CACHE_TTL_SECONDS` and `USAGE_REQUEST_TIMEOUT_SECONDS` accept unsigned integer values.

For an MCP client that starts local commands, replace `/absolute/path` with the host's absolute home path. JSON argument arrays do not expand `${HOME}`:

```json
{
  "mcpServers": {
    "usage": {
      "command": "docker",
      "args": [
        "run", "--rm", "-i", "--read-only", "--user", "1000:1000",
        "-e", "CODEX_AUTH_FILE=/mnt/codex/auth.json",
        "-v", "/absolute/path/.codex:/mnt/codex:ro",
        "usage-mcp-server"
      ]
    }
  }
}
```

The stdio transport uses newline-delimited JSON-RPC over stdin/stdout. Both
transports support `initialize`, `tools/list`, `ping`, and `tools/call` for
`get_usage`.
