---
layout: ../../layouts/Docs.astro
title: Getting started · VL
---

<h1>Getting started</h1>
<p>A sketch of the final guide. The commands below already work.</p>

<h2>Ask for Rust, get a compiler</h2>
<p>You need Rust stable 1.80 or newer, plus Cargo. Nothing else. Clone the repository; the compiler lives at the workspace root.</p>

<h2>Check a program</h2>
<pre>cargo run -- check examples/hello.vl</pre>
<p class="mt-4 text-muted">A clean program exits 0 and stays silent. A broken one exits 1 with an Ariadne report pointing at the span.</p>

<h2>Build it and look inside</h2>
<pre>cargo run -- build examples/arith.vl --emit lir
cargo run -- build examples/arith.vl --target stackvm</pre>

<h2>Still to write</h2>
<ul class="list-disc space-y-1 pl-6">
  <li>Editor setup and <code>.vl</code> file association</li>
  <li>A first program, walked line by line</li>
  <li>How to read an Ariadne diagnostic</li>
</ul>
