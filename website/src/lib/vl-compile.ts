// Shared client for the hosted VL compiler service.
//
// Used by both the index Playground and the per-snippet Run buttons so every
// "Run" does the same thing: POST the source to `<vlc>/v1/compile` and report
// the build result. VM execution stays stubbed until the WASM VM lands — a
// successful build only reports the artifact, it never tries to execute it.

export interface VlCompileOk {
  ok: true;
  target: string;
  bytes: Uint8Array;
  diagnostics: string;
}

export interface VlCompileErr {
  ok: false;
  error: string;
  diagnostics: string;
}

export type VlCompileResult = VlCompileOk | VlCompileErr;

export function compilerUrl(): string {
  return import.meta.env.PUBLIC_VLC_URL ?? 'https://vlc.nara-lang.org';
}

export async function compileVl(source: string, filename = 'snippet.vl'): Promise<VlCompileResult> {
  const response = await fetch(`${compilerUrl()}/v1/compile`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ source, filename }),
  });
  const body = await response.json();
  if (!response.ok || !body.ok) {
    return {
      ok: false,
      error: body.error ?? `HTTP ${response.status}`,
      diagnostics: body.diagnostics ?? '',
    };
  }
  const bytes = Uint8Array.from(atob(body.bytecode_base64), (char) => char.charCodeAt(0));
  return { ok: true, target: body.target, bytes, diagnostics: body.diagnostics ?? '' };
}

/// Format a compile result exactly like the index Playground does:
/// one status line, the Naravm-ready stub line, then diagnostics (if any).
/// No execution is attempted — the WASM VM is still a stub.
export function formatCompileLines(result: VlCompileResult): string[] {
  if (!result.ok) {
    return [
      `build failed — ${result.error}`,
      ...(result.diagnostics ? result.diagnostics.split('\n') : []),
    ];
  }
  return [
    `build ok — ${result.target}, ${result.bytes.byteLength} byte vmfile`,
    'The artifact is ready for Naravm.',
    ...(result.diagnostics ? result.diagnostics.split('\n') : []),
  ];
}
