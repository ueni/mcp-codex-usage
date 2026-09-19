FROM rust:1.85-alpine AS builder

WORKDIR /src
RUN apk add --no-cache ca-certificates musl-dev
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder /src/target/release/usage-mcp-server /usage-mcp-server
USER nonroot:nonroot
ENTRYPOINT ["/usage-mcp-server"]
