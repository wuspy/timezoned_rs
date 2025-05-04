FROM rust:1.85-bookworm AS builder
WORKDIR /usr/src/timezoned_rs
COPY . .
RUN cargo install --path .

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y tar wget && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/local/bin

COPY --from=builder --chmod=555 /usr/local/cargo/bin/timezoned_rs .
COPY --from=builder --chmod=555 /usr/src/timezoned_rs/*.sh .

ENTRYPOINT ["timezoned_rs"]
