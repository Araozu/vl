FROM rust:1.88-bookworm AS compiler

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src

RUN cargo build --release --bin vl

FROM golang:1.23-bookworm AS service

WORKDIR /src/vlc
COPY vlc/go.mod ./go.mod
COPY vlc/cmd ./cmd
RUN CGO_ENABLED=0 go build -o /vlc ./cmd/vlc

FROM debian:bookworm-slim

RUN useradd --create-home --uid 10001 vlc
COPY --from=compiler /src/target/release/vl /usr/local/bin/vl
COPY --from=service /vlc /usr/local/bin/vlc

USER vlc
EXPOSE 8080
ENV VLC_ADDR=:8080 VL_COMPILER=/usr/local/bin/vl
ENTRYPOINT ["/usr/local/bin/vlc"]
