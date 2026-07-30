import * as path from "node:path";
import { existsSync } from "node:fs";

import {
  commands,
  ExtensionContext,
  LogOutputChannel,
  window,
  workspace,
} from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let output: LogOutputChannel | undefined;

export async function activate(context: ExtensionContext): Promise<void> {
  output = window.createOutputChannel("Reimer Language Server", { log: true });
  context.subscriptions.push(output);
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
      await startClient(context);
      window.setStatusBarMessage("Reimer language server restarted", 2500);
    }),
  );

  await startClient(context);
}

export async function deactivate(): Promise<void> {
  await stopClient();
}

async function startClient(context: ExtensionContext): Promise<void> {
  const channel = output;
  if (!channel) {
    throw new Error("Reimer output channel was not initialized");
  }
  const nextClient = createClient(context, channel);
  client = nextClient;
  channel.appendLine("Starting Reimer Language Server...");
  try {
    await nextClient.start();
    channel.appendLine("Reimer Language Server is ready.");
  } catch (error) {
    if (client === nextClient) {
      client = undefined;
    }
    const details = error instanceof Error ? error.stack ?? error.message : String(error);
    channel.appendLine(`Failed to start Reimer Language Server:\n${details}`);
    const action = await window.showErrorMessage(
      "Reimer Language Server could not start. See the output channel for details.",
      "Show Output",
    );
    if (action === "Show Output") {
      channel.show(true);
    }
    throw error;
  }
}

function createClient(
  context: ExtensionContext,
  channel: LogOutputChannel,
): LanguageClient {
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
    outputChannel: channel,
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
