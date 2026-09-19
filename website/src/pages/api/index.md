---
layout: ../../layouts/Docs.astro
title: API reference · VL
section: api
---

<h1>API reference</h1>
<p>A sketch, one page per layer. Crates depend only on the stages below them, and everything may depend on <code>vl-common</code>. Nothing depends on the driver.</p>

<div class="chapter-list mt-8">
  <a href="/api/driver">
    <p class="text-[1.25rem] font-medium leading-snug">The driver</p>
    <p class="mt-1 text-[1.02rem] leading-relaxed text-muted">CLI wiring, and the only place that prints a diagnostic.</p>
  </a>
  <a href="/api/frontend">
    <p class="text-[1.25rem] font-medium leading-snug">Frontend crates</p>
    <p class="mt-1 text-[1.02rem] leading-relaxed text-muted">Spans and reports, tokens, trees, and scopes.</p>
  </a>
  <a href="/api/backend">
    <p class="text-[1.25rem] font-medium leading-snug">Backend crates</p>
    <p class="mt-1 text-[1.02rem] leading-relaxed text-muted">Desugaring, checking, three-address code, and targets.</p>
  </a>
</div>

<h2>Rules the code is held to</h2>
<p>Errors travel as <code>Vec&lt;Diagnostic&gt;</code> and only the driver renders them. Stages recover per item instead of panicking, and poisoned nodes (<code>Ty::Error</code>, no definition) pass through later stages silently.</p>
