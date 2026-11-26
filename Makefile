app_name = flowerss-bot

VERSION ?= $(shell git describe --tags --always)
COMMIT ?= $(shell git rev-parse --short HEAD)
BUILD_TIME ?= $(shell date -u +%Y-%m-%dT%H:%M:%SZ)

.PHONY: all build get run test clean

test:
	cargo test --locked

all: build

build:
	APP_VERSION="$(VERSION)" APP_COMMIT="$(COMMIT)" BUILD_TIME="$(BUILD_TIME)" cargo build --locked --release
	cp target/release/$(app_name) .

get:
	cargo fetch --locked

run:
	cargo run --locked

clean:
	rm -f $(app_name)
	cargo clean