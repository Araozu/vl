---
layout: ../../layouts/Docs.astro
title: Frontend crates · VL API
section: api
---

<h1>Frontend crates</h1>
<p>A sketch. Text in, resolved names out.</p>

<h2>vl-common</h2>
<p><code>Span</code>, <code>Sources</code>, and the Ariadne-backed <code>Diagnostic</code>. Every crate builds on it.</p>

<h2>vl-lex</h2>
<p>A hand-rolled tokenizer that never panics. Still to write: the token table.</p>

<h2>vl-syntax</h2>
<p>A recursive-descent parser and its tree, with per-item recovery. Still to write: the node catalogue and grammar notes.</p>

<h2>vl-semantic</h2>
<p>Scope resolution: undefined names and duplicate definitions. Still to write: the resolution rules.</p>
