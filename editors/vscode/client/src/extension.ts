import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext): void {
  // `vl` must be on PATH (or set `vl.server.path`); it speaks stdio LSP
  // via `vl lsp` (diagnostics, hover, goto-definition, symbols,
  // formatting, completion).
  const config = vscode.workspace.getConfiguration("vl");
  const serverPath = config.get<string>("server.path", "vl");
  const serverOptions: ServerOptions = {
    command: serverPath,
    args: ["lsp"],
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "vl" }],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.vl"),
    },
  };
  client = new LanguageClient(
    "vl",
    "VL Language Server",
    serverOptions,
    clientOptions,
  );
  client.start();
  context.subscriptions.push({
    dispose: () => {
      void client?.stop();
    },
  });
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
