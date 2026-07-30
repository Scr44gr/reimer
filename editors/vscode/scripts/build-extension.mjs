import path from "node:path";
import { fileURLToPath } from "node:url";

import { build } from "esbuild";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const extensionDirectory = path.dirname(scriptDirectory);

await build({
  entryPoints: [path.join(extensionDirectory, "src", "extension.ts")],
  bundle: true,
  external: ["vscode"],
  format: "cjs",
  logLevel: "info",
  outfile: path.join(extensionDirectory, "out", "extension.js"),
  platform: "node",
  sourcemap: true,
  target: "node20",
});
