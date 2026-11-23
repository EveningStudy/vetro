import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import go from "highlight.js/lib/languages/go";
import ini from "highlight.js/lib/languages/ini";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdown from "highlight.js/lib/languages/markdown";
import powershell from "highlight.js/lib/languages/powershell";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

const languages = { bash, css, diff, go, ini, javascript, json, markdown, powershell, python, rust, sql, typescript, xml, yaml };
for (const [name, language] of Object.entries(languages)) hljs.registerLanguage(name, language);
const aliases: Record<string, string> = {
  sh: "bash",
  shell: "bash",
  zsh: "bash",
  console: "bash",
  ps1: "powershell",
  pwsh: "powershell",
  toml: "ini",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  ts: "typescript",
  tsx: "typescript",
  rs: "rust",
  py: "python",
  html: "xml",
  svg: "xml",
  yml: "yaml",
  md: "markdown",
  patch: "diff",
  jsonl: "json",
};

export function highlightCode(code: string, lang: string | undefined) {
  const name = lang ? (aliases[lang.toLowerCase()] ?? lang.toLowerCase()) : undefined;
  if (!name || !hljs.getLanguage(name) || code.length > 60_000) return null;
  try {
    return hljs.highlight(code, { language: name, ignoreIllegals: true }).value;
  } catch {
    return null;
  }
}

