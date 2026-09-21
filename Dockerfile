# syntax=docker/dockerfile:1.7

FROM rust:1.88-alpine AS builder

WORKDIR /src
RUN apk add --no-cache ca-certificates musl-dev
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN --mount=type=secret,id=host_ca,required=false \
    if [ -f /run/secrets/host_ca ]; then \
      cp /run/secrets/host_ca /usr/local/share/ca-certificates/host-ca.crt && \
      update-ca-certificates; \
    fi && \
    cargo build --release

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder /src/target/release/usage-mcp-server /usage-mcp-server
ENV HOME=/home/nonroot \
    CODEX_HOME=/home/nonroot/.codex
EXPOSE 8000
USER nonroot:nonroot
ENTRYPOINT ["/usage-mcp-server"]
