---
layout: ../../layouts/Docs.astro
title: Documentation · VL
---

<h1>Documentation</h1>
<p>Start here. In v0, VL is integers, arithmetic, <code>let</code>, <code>function</code>, and <code>//</code> comments, carried through a strict staged pipeline. Three short chapters cover it.</p>

<div class="chapter-list mt-8">
  <a href="/docs/getting-started">
    <p class="text-[1.25rem] font-medium leading-snug">Getting started</p>
    <p class="mt-1 text-[1.02rem] leading-relaxed text-muted">Install nothing but Rust, then check and build your first <code>.vl</code> file.</p>
  </a>
  <a href="/docs/language">
    <p class="text-[1.25rem] font-medium leading-snug">Language tour</p>
    <p class="mt-1 text-[1.02rem] leading-relaxed text-muted">The v0 surface, scoping, and why one root cause stays one error.</p>
  </a>
  <a href="/docs/cli">
    <p class="text-[1.25rem] font-medium leading-snug">CLI reference</p>
    <p class="mt-1 text-[1.02rem] leading-relaxed text-muted">Every subcommand of the <code>vl</code> driver, with examples.</p>
  </a>
</div>

<h2>The pipeline, in one line</h2>
<pre>.vl → vl-lex → vl-syntax → vl-semantic → vl-hir → vl-typecheck → vl-lir → vl-codegen</pre>
<p class="mt-4 text-muted">Each stage gets its own chapter once the language surface settles. For the crate view, see the <a href="/api">API reference</a>.</p>
