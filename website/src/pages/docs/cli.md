---
layout: ../../layouts/Docs.astro
title: CLI reference · VL
---

<h1>CLI reference</h1>
<p>A sketch. The subcommands below are real; the driver (<code>src/main.rs</code>) owns the flags, the file reading, the exit codes, and all printing.</p>

<h2>Subcommands</h2>
<pre>vl check &lt;file&gt;</pre>
<p class="mt-4">Runs the frontend end to end. Prints nothing on success; on failure, an Ariadne report on stderr and exit 1.</p>
<pre>vl build &lt;file&gt; [--emit lir] [--target &lt;name&gt;]</pre>
<p class="mt-4">Compiles through the selected backend. The default backend is a placeholder until a real target lands.</p>
<pre>vl lex &lt;file&gt;
vl parse &lt;file&gt;</pre>
<p class="mt-4">Inspection helpers that stop after tokens, or after the tree.</p>
<pre>vl targets</pre>
<p class="mt-4">Lists the backends registered in <code>vl-codegen</code>.</p>
