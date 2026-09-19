# VL Compiler Service

`vlc` is the small HTTP service behind the docs playground. It accepts VL
source, invokes the Rust compiler with the `naravm` target, and returns a
base64-encoded Naravm vmfile or the compiler diagnostics.

## Local

Build the Rust compiler from the repository root, then run the service with
`VL_COMPILER` pointing at it:

```sh
cargo build --bin vl
VL_COMPILER=../target/debug/vl go run ./cmd/vlc
```

Compile a source file:

```sh
curl -X POST http://localhost:8080/v1/compile \
  -H 'content-type: application/json' \
  -d '{"source":"use std.print; function main() { print(\"hello\\n\"); }"}'
```

The production service is deployed as `https://vlc.nara-lang.org`. Its
`+devops/` directory contains the Docker, Jenkins, Compose, and Ansible setup.
It only compiles artifacts; execution stays out of the browser until the
Naravm Wasm32 build is ready.
