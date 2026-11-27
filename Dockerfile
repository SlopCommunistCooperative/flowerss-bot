FROM rust:1.97-alpine AS builder
RUN apk add --no-cache build-base
WORKDIR /flowerss
COPY Cargo.toml Cargo.lock ./

# Persist the Cargo registry and compiled release dependencies in a BuildKit layer.
# Clean only the temporary binary crate so the real source always recompiles.
RUN mkdir src \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --locked --release --bin flowerss-bot \
    && cargo clean --release -p flowerss-bot \
    && rm -r src

COPY src ./src
RUN cargo build --locked --release --bin flowerss-bot

FROM alpine
RUN apk add --no-cache ca-certificates
COPY --from=builder /flowerss/target/release/flowerss-bot /bin/
VOLUME /root/.flowerss
WORKDIR /root/.flowerss
ENTRYPOINT ["/bin/flowerss-bot"]

