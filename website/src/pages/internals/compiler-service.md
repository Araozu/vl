---
layout: ../../layouts/Docs.astro
title: Compiler service
description: The HTTP service used by the interactive playground.
eyebrow: Internals
availability: VL 0.1+
---

# Compiler service

The playground sends source to `https://vlc.nara-lang.org`. The service runs
the Rust compiler with the `naravm` target and returns either a base64-encoded
vmfile or the compiler's Ariadne diagnostics. The service only compiles;
execution happens in the browser with the Naravm WASM artifact.

## Run

`GET /naravm.wasm` serves the Luna WASM build that the docs host mounts
from `/var/bin/naravm-luna-ai.wasm`. The playground (`src/lib/naravm-run.ts`)
instantiates it with `host_stdout_write` / `host_stderr_write` imports, copies
the compiled vmfile through `alloc_input`, calls `run_bytecode`, and renders
stdout, stderr, and the exit status (`ok`, `invalid_input`, `manager_error`,
`entrypoint_error`, `init_error`, `runtime_error`). The WASM build registers
the wasm-safe std subset (`std::{print,print_u64,string,math,fmt}`);
`std::fs` / `std::process` stay native-only. Override the URL with
`PUBLIC_NARAVM_WASM_URL` when building the site.

## Compile

```http
POST /v1/compile
Content-Type: application/json
```

Request:

```json
{
  "source": "use std.println;\nfun main() { println(\"hello\"); }",
  "filename": "playground.vl"
}
```

A successful response contains the Naravm vmfile in `bytecode_base64`:

```json
{
  "ok": true,
  "target": "naravm",
  "bytecode_base64": "bmFyYQ..."
}
```

Invalid VL returns HTTP `422` with `ok: false` and rendered diagnostics.
Requests are limited to 256 KiB and compilation is limited to ten seconds.
`GET /healthz` returns `{ "status": "ok" }`.
