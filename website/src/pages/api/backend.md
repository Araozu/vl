---
layout: ../../layouts/Docs.astro
title: Backend crates · VL API
section: api
---

<h1>Backend crates</h1>
<p>A sketch. A resolved tree in, backend output out.</p>

<h2>vl-hir</h2>
<p>The desugared tree, with node ids and <code>DefId</code> links. Still to write: the node catalogue.</p>

<h2>vl-typecheck</h2>
<p>Types and their rules. In v0 everything is <code>int</code>. Still to write: the judgments.</p>

<h2>vl-lir</h2>
<p>Target-agnostic three-address code. Still to write: the instruction set.</p>

<h2>vl-codegen</h2>
<p>Backends implement <code>Target</code> and register in <code>lookup</code> and <code>all_targets</code>. The placeholder backend stays until a real one takes its place as default. Still to write: how to add a backend.</p>
