import { existsSync } from "node:fs";
import { join } from "node:path";
import { commands, type ExtensionContext, workspace } from "vscode";
import { LanguageClient } from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export async function activate(context: ExtensionContext) {
  context.subscriptions.push(
    commands.registerCommand("reylo.restart", restart),
    workspace.onDidChangeConfiguration((change) => {
      if (change.affectsConfiguration("reylo")) void restart();
    }),
  );
  await start();
}

export function deactivate() {
  return client?.stop();
}

async function start() {
  const config = workspace.getConfiguration("reylo");
  const args = ["lsp"];
  const dialect = config.get<string>("dialect");
  if (dialect) args.push("--dialect", dialect);
  if (config.get<boolean>("allowUrlCommand")) args.push("--allow-url-command");
  client = new LanguageClient("reylo", "Reylo", { command: command(config.get<string>("path")), args }, { documentSelector: [{ scheme: "file", language: "sql" }] });
  await client.start();
}

async function restart() {
  await client?.stop();
  await start();
}

/** The setting, else the workspace's own `reylo`, so the server runs the version the project types with, else the one on PATH. */
function command(setting: string | undefined): string {
  if (setting) return setting;
  const bin = process.platform === "win32" ? "reylo.cmd" : "reylo";
  for (const folder of workspace.workspaceFolders ?? []) {
    const local = join(folder.uri.fsPath, "node_modules", ".bin", bin);
    if (existsSync(local)) return local;
  }
  return "reylo";
}
