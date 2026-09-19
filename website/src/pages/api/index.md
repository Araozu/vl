---
layout: ../../layouts/Docs.astro
title: API reference
description: One page per layer, in dependency order.
eyebrow: Collection
availability: VL 0.1+
section: api
---

# API reference

Crates depend only on the stages below them, and everything may depend on `vl-common`. Nothing depends on the driver.

## Topics

### The driver

CLI wiring, and the only place that prints a diagnostic.

[The driver](/api/driver)

### Frontend crates

Spans and reports, tokens, trees, and scopes.

[Frontend crates](/api/frontend)

### Backend crates

Desugaring, checking, three-address code, and targets.

[Backend crates](/api/backend)

### Compiler service

The HTTP endpoint used by the interactive playground.

[Compiler service](/api/compiler)

## Overview

Errors travel as `Vec<Diagnostic>` and only the driver renders them. Stages recover per item instead of panicking, and poisoned nodes (`Ty::Error`, no definition) pass through later stages silently.
