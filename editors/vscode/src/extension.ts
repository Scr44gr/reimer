import * as path from "node:path";
import { existsSync } from "node:fs";

import {
  commands,
  ExtensionContext,
  window,
  workspace,
} from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export async function activate(context: ExtensionContext): Promise<void> {
  context.subscriptions.push(
    commands.registerCommand("reimer.organizeImports", async () => {
      await commands.executeCommand("editor.action.organizeImports");
    }),
    commands.registerCommand(
      "reimer.showAllocatorEstimate",
      async (details?: unknown) => {
        const message =
          typeof details === "string"
            ? details
            : "The estimate is unavailable for this source snapshot.";
        await window.showInformationMessage(message, { modal: true });
      },
    ),
    commands.registerCommand("reimer.restartServer", async () => {
      await stopClient();
      client = createClient(context);
      await client.start();
      window.setStatusBarMessage("Reimer language server restarted", 2500);
    }),
  );

  client = createClient(context);
  await client.start();
}

export async function deactivate(): Promise<void> {
  await stopClient();
}

function createClient(context: ExtensionContext): LanguageClient {
  const configuration = workspace.getConfiguration("reimer");
  const configuredPath = configuration.get<string>(
    "server.path",
    "",
  );
  const executable = process.platform === "win32" ? "reimer-lsp.exe" : "reimer-lsp";
  const bundledPath = context.asAbsolutePath(path.join("server", executable));
  const command =
    configuredPath.length > 0
      ? path.isAbsolute(configuredPath)
        ? path.normalize(configuredPath)
        : configuredPath
      : existsSync(bundledPath)
        ? bundledPath
        : executable;
  const args = configuration.get<string[]>("server.arguments", []);
  const workingDirectory = workspace.workspaceFolders?.[0]?.uri.fsPath;
  const serverOptions: ServerOptions = {
    command,
    args,
    options: workingDirectory ? { cwd: workingDirectory } : undefined,
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: "file", language: "reimer" },
      { scheme: "untitled", language: "reimer" },
    ],
    synchronize: {
      fileEvents: [
        workspace.createFileSystemWatcher("**/*.reim"),
        workspace.createFileSystemWatcher("**/reimer.toml"),
        workspace.createFileSystemWatcher("**/reimer.lock"),
      ],
      configurationSection: "reimer",
    },
    outputChannelName: "Reimer Language Server",
  };
  return new LanguageClient(
    "reimerLanguageServer",
    "Reimer Language Server",
    serverOptions,
    clientOptions,
  );
}

async function stopClient(): Promise<void> {
  const running = client;
  client = undefined;
  if (running) {
    await running.stop();
  }
}
