FROM rust:1.97-alpine AS builder
RUN apk add --no-cache build-base
WORKDIR /flowerss
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM alpine
RUN apk add --no-cache ca-certificates
COPY --from=builder /flowerss/target/release/flowerss-bot /bin/
VOLUME /root/.flowerss
WORKDIR /root/.flowerss
ENTRYPOINT ["/bin/flowerss-bot"]

