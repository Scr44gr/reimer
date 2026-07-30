import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const extensionDirectory = path.dirname(scriptDirectory);
const bundle = await readFile(
  path.join(extensionDirectory, "out", "extension.js"),
  "utf8",
);

assert.ok(
  bundle.length > 100_000,
  "extension bundle should contain the language client implementation",
);
assert.doesNotMatch(
  bundle,
  /require\(["']vscode-languageclient\/node["']\)/,
  "extension bundle must not require an unpackaged language client",
);
assert.match(
  bundle,
  /Reimer Language Server/,
  "extension bundle should contain the language client entry point",
);
