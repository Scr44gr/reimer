import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import oniguruma from "vscode-oniguruma";
import textmate from "vscode-textmate";

const { loadWASM, OnigScanner, OnigString } = oniguruma;
const { Registry } = textmate;

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const extensionDirectory = path.dirname(scriptDirectory);
const wasmPath = path.join(
  extensionDirectory,
  "node_modules",
  "vscode-oniguruma",
  "release",
  "onig.wasm",
);
const grammarPath = path.join(
  extensionDirectory,
  "syntaxes",
  "reimer.tmLanguage.json",
);
const languageConfigurationPath = path.join(
  extensionDirectory,
  "language-configuration.json",
);
const manifestPath = path.join(extensionDirectory, "package.json");
const wasm = await readFile(wasmPath);
await loadWASM(wasm.buffer);

const registry = new Registry({
  onigLib: Promise.resolve({
    createOnigScanner: (sources) => new OnigScanner(sources),
    createOnigString: (text) => new OnigString(text),
  }),
  loadGrammar: async (scopeName) => {
    if (scopeName !== "source.reimer") {
      return null;
    }
    return JSON.parse(await readFile(grammarPath, "utf8"));
  },
});
const grammar = await registry.loadGrammar("source.reimer");
assert.ok(grammar, "grammar should load");

assertScope(
  grammar,
  "from std::string import String;",
  "from",
  "keyword.control.import.from.reimer",
);
assertScope(
  grammar,
  "let text = String::from(&allocator, \"á\")?;",
  "from",
  "entity.name.function.call.reimer",
);
assertNotScope(
  grammar,
  "let text = String::from(&allocator, \"á\")?;",
  "from",
  "keyword.control.import.from.reimer",
);
assertScope(
  grammar,
  "fn from(value: str) -> str { value }",
  "from",
  "entity.name.function.reimer",
);
assertScope(
  grammar,
  "fn from(value: str) -> str { value }",
  "->",
  "keyword.operator.reimer",
);
assertScope(
  grammar,
  "let title = c\"window\";",
  "window",
  "string.quoted.double.c.reimer",
);
assertScope(
  grammar,
  "comptime fn factorial(value: usize) -> usize { value }",
  "comptime",
  "storage.modifier.reimer",
);
assertScope(
  grammar,
  "const BYTES: usize = size_of<Header>();",
  "BYTES",
  "constant.other.reimer",
);
assertScope(
  grammar,
  "const BYTES: usize = size_of<Header>();",
  "size_of",
  "support.function.builtin.reimer",
);
for (const tokenText of ["*", "->"]) {
  assertScope(
    grammar,
    "fn pointer() -> *mut i32 { panic(\"unused\") }",
    tokenText,
    "keyword.operator.reimer",
  );
}

const languageConfiguration = JSON.parse(
  await readFile(languageConfigurationPath, "utf8"),
);
assert.deepEqual(languageConfiguration.brackets, [
  ["{", "}"],
  ["[", "]"],
  ["(", ")"],
]);

const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
const editorDefaults = manifest.contributes.configurationDefaults["[reimer]"];
assert.equal(editorDefaults["editor.matchBrackets"], "always");
assert.equal(editorDefaults["editor.inlayHints.enabled"], "on");
assert.equal(editorDefaults["editor.guides.bracketPairs"], "active");

function assertScope(grammar, line, tokenText, expectedScope) {
  const scopes = scopesFor(grammar, line, tokenText);
  assert.ok(
    scopes.includes(expectedScope),
    `expected ${JSON.stringify(tokenText)} to include ${expectedScope}; found ${scopes.join(", ")}`,
  );
}

function assertNotScope(grammar, line, tokenText, unexpectedScope) {
  const scopes = scopesFor(grammar, line, tokenText);
  assert.ok(
    !scopes.includes(unexpectedScope),
    `expected ${JSON.stringify(tokenText)} not to include ${unexpectedScope}; found ${scopes.join(", ")}`,
  );
}

function scopesFor(grammar, line, tokenText) {
  const start = line.indexOf(tokenText);
  assert.notEqual(start, -1, "fixture token should exist");
  const token = grammar
    .tokenizeLine(line)
    .tokens.find((candidate) => candidate.startIndex <= start && candidate.endIndex >= start + tokenText.length);
  assert.ok(token, `token ${JSON.stringify(tokenText)} should be emitted`);
  return token.scopes;
}
