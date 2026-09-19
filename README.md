# Usage MCP server

This is a small stdio MCP server exposing one tool, `get_usage`. The tool fetches a JSON usage document from a configured endpoint and returns the complete document together with a concise summary of common weekly, monthly, and other token, credit, and request fields. Successful responses are cached for 30 seconds by default.

The server is provider-neutral because usage and weekly credit endpoints vary. Put the endpoint and its credentials in a JSON file mounted into the container:

```json
{
  "url": "https://usage.example.test/v1/usage",
  "bearer_token": "replace-me",
  "headers": {
    "X-Workspace": "example"
  }
}
```

Use `api_key` instead of `bearer_token` when the provider expects an `X-API-Key` header. `headers` is optional and can add or override request headers. The file is only read from the mounted path and secrets are never included in responses or logs.

Build and start it with Docker (the stdin attachment is required for MCP stdio):

```sh
docker build -t usage-mcp-server .
docker run --rm -i --read-only \
  -v "$PWD/usage.json:/run/secrets/usage.json:ro" \
  usage-mcp-server
```

The default secret path is `/run/secrets/usage.json`; set `USAGE_SECRETS_FILE` to use another mounted path. `USAGE_CACHE_TTL_SECONDS` and `USAGE_REQUEST_TIMEOUT_SECONDS` can be set to unsigned integer values.

For an MCP client that starts local commands, use a configuration like:

```json
{
  "mcpServers": {
    "usage": {
      "command": "docker",
      "args": [
        "run", "--rm", "-i", "--read-only",
        "-v", "/absolute/path/usage.json:/run/secrets/usage.json:ro",
        "usage-mcp-server"
      ]
    }
  }
}
```

The MCP protocol is newline-delimited JSON-RPC over stdin/stdout. The server supports `initialize`, `tools/list`, and `tools/call` for `get_usage`.
