(() => {
  "use strict";

  const keywords = new Set([
    "as", "break", "comptime", "const", "continue", "defer", "else",
    "enum", "extern", "fn", "for", "from", "if", "impl", "import",
    "in", "let", "loop", "match", "mut", "pub", "return", "self",
    "static", "struct", "super", "trait", "unsafe", "use", "where",
    "while",
  ]);
  const types = new Set([
    "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128",
    "isize", "never", "str", "u8", "u16", "u32", "u64", "u128", "usize",
  ]);
  const literals = new Set(["true", "false", "None", "Some", "Ok", "Err"]);
  const tokenPattern = /\/\/[^\n]*|\/\*[\s\S]*?\*\/|(?:f|c)?"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])'|@[A-Za-z_][A-Za-z0-9_]*|(?:0[xX][0-9A-Fa-f_]+|0[bB][01_]+|0[oO][0-7_]+|\b\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d[\d_]*)?)|\b[A-Za-z_][A-Za-z0-9_]*\b/g;

  function escapeHtml(value) {
    return value
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;");
  }

  function tokenClass(token, source, end) {
    if (token.startsWith("//") || token.startsWith("/*")) return "rm-comment";
    if (token.startsWith('"') || token.startsWith("f\"") || token.startsWith("c\"") || token.startsWith("'")) return "rm-string";
    if (token.startsWith("@")) return "rm-attribute";
    if (/^(?:\d|0[xXbBoO])/.test(token)) return "rm-number";
    if (keywords.has(token)) return "rm-keyword";
    if (types.has(token)) return "rm-type";
    if (literals.has(token)) return "rm-literal";
    if (/^\s*\(/.test(source.slice(end))) return "rm-function";
    return "";
  }

  function highlight(code) {
    if (code.dataset.reimerHighlighted === "true") return;
    const source = code.textContent ?? "";
    let cursor = 0;
    let rendered = "";

    for (const match of source.matchAll(tokenPattern)) {
      const index = match.index ?? 0;
      rendered += escapeHtml(source.slice(cursor, index));
      const token = match[0];
      const className = tokenClass(token, source, index + token.length);
      const escaped = escapeHtml(token);
      rendered += className ? `<span class="${className}">${escaped}</span>` : escaped;
      cursor = index + token.length;
    }
    rendered += escapeHtml(source.slice(cursor));
    code.innerHTML = rendered;
    code.dataset.reimerHighlighted = "true";
  }

  function initialize() {
    document.querySelectorAll("code.language-reimer").forEach(highlight);

    const title = document.querySelector(".menu-title");
    if (title && !title.querySelector(".reimer-release-mark")) {
      const mark = document.createElement("span");
      mark.className = "reimer-release-mark";
      mark.textContent = "EXPERIMENTAL";
      title.append(mark);
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", initialize, { once: true });
  } else {
    initialize();
  }
})();
