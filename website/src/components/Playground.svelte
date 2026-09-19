<script lang="ts">
  let source = $state('let x = 1 + 2 * 3;\nfn main() { let d = x - 1; d }');
  let output = $state<string[]>(['// press Run — this sketch only pretends. The real pipeline lives in the Rust driver.']);

  function run() {
    // Stub: pretend to lex/check. Real playground will call into vl via WASM or server.
    const lines = source.split('\n').filter((l) => l.trim().length > 0);
    output = [
      `ok — ${lines.length} line(s), 0 errors (sketch)`,
      ...lines.map((l, i) => `  L${i + 1}: ${l.trim().slice(0, 60)}`),
      'Tip: cargo run -- check examples/hello.vl runs the real thing.',
    ];
  }

  function loadSample(kind: 'hello' | 'arith' | 'error') {
    if (kind === 'hello') source = 'fn main() { 42 }';
    if (kind === 'arith') source = 'let x = 1 + 2 * 3;\nfn main() { let d = x - 1; d }';
    if (kind === 'error') source = 'fn main() { undefined_var }';
    output = ['// sample loaded — press Run'];
  }
</script>

<div class="overflow-hidden rounded-[0.9rem] border border-rule bg-plate text-[#efe6d2]">
  <div class="flex flex-wrap items-center justify-between gap-2 border-b border-rule px-5 py-3">
    <div class="flex gap-1 font-mono text-[0.82rem]">
      <button class="rounded-md px-2.5 py-1 text-muted transition hover:bg-wash hover:text-parchment" onclick={() => loadSample('hello')}>hello.vl</button>
      <button class="rounded-md px-2.5 py-1 text-muted transition hover:bg-wash hover:text-parchment" onclick={() => loadSample('arith')}>arith.vl</button>
      <button class="rounded-md px-2.5 py-1 text-muted transition hover:bg-wash hover:text-parchment" onclick={() => loadSample('error')}>err.vl</button>
    </div>
    <button
      class="rounded-full bg-gold px-5 py-1.5 font-display text-[1rem] font-semibold text-night transition hover:bg-gold-bright"
      onclick={run}
    >
      Run
    </button>
  </div>
  <div class="grid md:grid-cols-2">
    <textarea
      bind:value={source}
      spellcheck={false}
      class="min-h-56 resize-y bg-transparent p-5 font-mono text-[0.9rem] leading-[1.75] text-[#efe6d2] outline-none placeholder:text-faint"
      placeholder="write VL here…"
    ></textarea>
    <div class="border-t border-rule p-5 font-mono text-[0.84rem] leading-[1.75] md:border-l md:border-t-0">
      {#each output as line}
        <p class="text-muted">{line}</p>
      {/each}
    </div>
  </div>
</div>
