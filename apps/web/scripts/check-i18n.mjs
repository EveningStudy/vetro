import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const dir = join(dirname(fileURLToPath(import.meta.url)), "../src/i18n");
const files = readdirSync(dir)
  .filter((name) => name.endsWith(".ts") && name !== "index.ts")
  .sort((a, b) => (a === "en.ts" ? -1 : b === "en.ts" ? 1 : a.localeCompare(b)));

if (!files.includes("en.ts")) {
  console.error("Missing base locale file: en.ts");
  process.exit(1);
}

function readStringLiteral(source, start) {
  const quote = source[start];
  let out = "";
  let i = start + 1;
  while (i < source.length) {
    const ch = source[i];
    if (ch === "\\") {
      const next = source[i + 1];
      if (next === undefined) throw new Error(`Unterminated escape at ${i}`);
      if (next === "n") out += "\n";
      else if (next === "r") out += "\r";
      else if (next === "t") out += "\t";
      else out += next;
      i += 2;
      continue;
    }
    if (ch === quote) return { value: out, next: i + 1 };
    out += ch;
    i += 1;
  }
  throw new Error(`Unterminated string literal at ${start}`);
}

function skipWsAndComments(source, pos) {
  let i = pos;
  while (i < source.length) {
    if (/\s/.test(source[i])) {
      i += 1;
    } else if (source[i] === "/" && source[i + 1] === "/") {
      const nl = source.indexOf("\n", i + 2);
      i = nl < 0 ? source.length : nl + 1;
    } else if (source[i] === "/" && source[i + 1] === "*") {
      const end = source.indexOf("*/", i + 2);
      i = end < 0 ? source.length : end + 2;
    } else {
      break;
    }
  }
  return i;
}

function readLocale(filePath) {
  const source = readFileSync(filePath, "utf8");
  const assign = source.indexOf("=");
  const brace = assign >= 0 ? source.indexOf("{", assign) : -1;
  if (brace < 0) throw new Error(`Could not find locale object in ${filePath}`);

  const map = new Map();
  const duplicates = [];
  let i = brace + 1;

  while (i < source.length) {
    i = skipWsAndComments(source, i);
    if (source[i] === "}") break;

    let key;
    if (source[i] === '"' || source[i] === "'") {
      const parsed = readStringLiteral(source, i);
      key = parsed.value;
      i = parsed.next;
    } else {
      const match = /^[A-Za-z_$][\w$]*/.exec(source.slice(i));
      if (!match) throw new Error(`Unexpected token at offset ${i} in ${filePath}`);
      key = match[0];
      i += key.length;
    }

    i = skipWsAndComments(source, i);
    if (source[i] !== ":") throw new Error(`Expected ':' after "${key}" in ${filePath}`);
    i = skipWsAndComments(source, i + 1);

    if (source[i] !== '"' && source[i] !== "'" && source[i] !== "`") {
      throw new Error(`Expected string literal for "${key}" in ${filePath}`);
    }
    const parsed = readStringLiteral(source, i);
    i = skipWsAndComments(source, parsed.next);
    if (source[i] === ",") i += 1;

    if (map.has(key)) duplicates.push(key);
    map.set(key, parsed.value);
  }
  return { map, duplicates };
}

function placeholders(text) {
  return [...new Set([...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]))].sort();
}

const errors = [];
const base = readLocale(join(dir, "en.ts"));
if (base.duplicates.length) errors.push(`en.ts: duplicate keys: ${base.duplicates.join(", ")}`);
for (const [key, value] of base.map) {
  if (!value.trim()) errors.push(`en.ts: empty value for "${key}"`);
}

for (const file of files) {
  if (file === "en.ts") continue;
  const locale = readLocale(join(dir, file));
  if (locale.duplicates.length) errors.push(`${file}: duplicate keys: ${locale.duplicates.join(", ")}`);
  for (const key of base.map.keys()) {
    if (!locale.map.has(key)) {
      errors.push(`${file}: missing key "${key}"`);
      continue;
    }
    const value = locale.map.get(key);
    if (!value.trim()) errors.push(`${file}: empty value for "${key}"`);
    const expected = placeholders(base.map.get(key));
    const actual = placeholders(value);
    if (JSON.stringify(expected) !== JSON.stringify(actual)) {
      errors.push(`${file}: placeholder mismatch for "${key}" (expected [${expected.join(", ")}], got [${actual.join(", ")}])`);
    }
  }
  for (const key of locale.map.keys()) {
    if (!base.map.has(key)) errors.push(`${file}: extra key "${key}" not present in en.ts`);
  }
}

if (errors.length) {
  for (const err of errors) console.error(err);
  process.exit(1);
}

const names = files.map((file) => file.replace(/\.ts$/, ""));
console.log(`i18n check passed: ${base.map.size} keys across [${names.join(", ")}]`);
