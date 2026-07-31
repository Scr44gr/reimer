import { cp, copyFile, mkdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const extensionDirectory = path.dirname(scriptDirectory);
const repositoryDirectory = path.resolve(extensionDirectory, "..", "..");
const executable = process.platform === "win32" ? "reimer-lsp.exe" : "reimer-lsp";
const source = path.join(repositoryDirectory, "target", "release", executable);
const destinationDirectory = path.join(extensionDirectory, "server");
const destination = path.join(destinationDirectory, executable);
const standardLibrarySource = path.join(repositoryDirectory, "std");
const standardLibraryDestination = path.join(destinationDirectory, "std");

await mkdir(destinationDirectory, { recursive: true });
await copyFile(source, destination);
await cp(standardLibrarySource, standardLibraryDestination, {
  force: true,
  recursive: true,
});
