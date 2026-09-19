---
layout: ../../layouts/Docs.astro
title: The driver · VL API
section: api
---

<h1>The driver</h1>
<p>A sketch. The source is <code>src/main.rs</code> at the workspace root. It reads files, drives the stages in order, renders every diagnostic with <code>emit_all</code>, and chooses the exit code: 0 when clean, 1 when anything was reported.</p>

<h2>What it owns</h2>
<ul class="list-disc space-y-1 pl-6">
  <li>Parsing the CLI (<code>check</code>, <code>build</code>, <code>lex</code>, <code>parse</code>, <code>targets</code>) with clap</li>
  <li>Reading <code>.vl</code> sources off disk</li>
  <li>Rendering <code>vl_common::Diagnostic</code> through Ariadne, in colour, on stderr</li>
</ul>

<h2>Still to write</h2>
<ul class="list-disc space-y-1 pl-6">
  <li>Flags and exit codes per subcommand</li>
  <li>Worked sessions beside the <a href="/docs/cli">CLI reference</a></li>
</ul>
