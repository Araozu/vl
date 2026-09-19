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
vmfile or the compiler's Ariadne diagnostics. It does not run the VM.

## Compile

```http
POST /v1/compile
Content-Type: application/json
```

Request:

```json
{
  "source": "use std.print;\nfunction main() { print(\"hello\\n\"); }",
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
