// Generates the frozen Unicode tables for oxkv-index tokenizer/v1.
//
// Usage:
//   node tools/generate_unicode_tables.mjs <ucd-dir> <output.rs>
//
// The input directory must hold UnicodeData.txt, CaseFolding.txt, and
// DerivedNormalizationProps.txt from one Unicode Character Database release.
// The script runs at development time only. The crate has no I/O.

import fs from "node:fs";
import path from "node:path";

const [inputDir = "tools/ucd", outputPath = "src/tokenizer/tables.rs"] =
  process.argv.slice(2);
const dir = path.resolve(inputDir);
const unicodeData = fs.readFileSync(`${dir}/UnicodeData.txt`, "utf8");
const caseFolding = fs.readFileSync(`${dir}/CaseFolding.txt`, "utf8");
const derived = fs.readFileSync(`${dir}/DerivedNormalizationProps.txt`, "utf8");
const versionMatch = caseFolding.match(/CaseFolding-([0-9.]+)\.txt/);
const unicodeVersion = versionMatch ? versionMatch[1] : "unknown";
if (unicodeVersion !== "16.0.0") {
  throw new Error(
    `expected Unicode data 16.0.0 but the input is ${unicodeVersion}`,
  );
}

const MAX = 0x110000;

// category[i] = two-letter category, or "" for unassigned.
const category = new Array(MAX).fill("");
const ccc = new Uint8Array(MAX);
const decomposition = new Map(); // cp -> array of cp (immediate canonical)

let firstRange = null;
for (const line of unicodeData.split("\n")) {
  if (line === "") continue;
  const f = line.split(";");
  const cp = Number.parseInt(f[0], 16);
  const name = f[1];
  const cat = f[2];
  const combining = Number.parseInt(f[3], 10);
  const decomp = f[5];
  if (name.endsWith(", First>")) {
    firstRange = { cp, cat, combining };
    continue;
  }
  if (name.endsWith(", Last>")) {
    if (firstRange === null) throw new Error("range end without start");
    for (let c = firstRange.cp; c <= cp; c += 1) {
      category[c] = firstRange.cat;
      ccc[c] = firstRange.combining;
    }
    firstRange = null;
    continue;
  }
  category[cp] = cat;
  ccc[cp] = combining;
  if (decomp !== "" && !decomp.startsWith("<")) {
    decomposition.set(cp, decomp.split(" ").map((x) => Number.parseInt(x, 16)));
  }
}
if (firstRange !== null) throw new Error("unterminated range");

const excluded = new Set();
for (const line of derived.split("\n")) {
  const clean = line.split("#")[0].trim();
  if (clean === "") continue;
  const parts = clean.split(";");
  if (parts.length < 2 || parts[1].trim() !== "Full_Composition_Exclusion") continue;
  const range = parts[0].trim();
  if (range.includes("..")) {
    const [a, b] = range.split("..").map((x) => Number.parseInt(x, 16));
    for (let c = a; c <= b; c += 1) excluded.add(c);
  } else {
    excluded.add(Number.parseInt(range, 16));
  }
}

const isLetter = (cat) => cat.startsWith("L");
const isMark = (cat) => cat.startsWith("M");
const isNumber = (cat) => cat.startsWith("N");
const isWordStart = (cp) => {
  const cat = category[cp];
  return isLetter(cat) || isNumber(cat) || cat === "Pc";
};
const isWordContinue = (cp) => isWordStart(cp) || isMark(category[cp]);

function ranges(predicate) {
  const out = [];
  let start = -1;
  for (let cp = 0; cp <= MAX; cp += 1) {
    const inside = cp < MAX && predicate(cp);
    if (inside && start < 0) start = cp;
    if (!inside && start >= 0) {
      out.push([start, cp - 1]);
      start = -1;
    }
  }
  return out;
}

const wordStart = ranges(isWordStart);
const wordContinue = ranges(isWordContinue);

// Full NFD for every non-Hangul code point with a canonical decomposition.
const SBase = 0xac00;
const SCount = 11172;
const nfd = new Map();
function fullDecompose(cp) {
  if (cp >= SBase && cp < SBase + SCount) return [cp];
  const direct = decomposition.get(cp);
  if (direct === undefined) return [cp];
  const out = [];
  for (const part of direct) out.push(...fullDecompose(part));
  return out;
}
for (const cp of decomposition.keys()) {
  const full = fullDecompose(cp);
  nfd.set(cp, full);
}

// Primary composites. The immediate canonical decomposition must have two
// code points. The first code point must be a starter. The composite must not
// be in the full composition exclusion set.
const compose = new Map();
for (const [cp, direct] of decomposition) {
  if (direct.length !== 2) continue;
  if (excluded.has(cp)) continue;
  const [a, b] = direct;
  if (ccc[a] !== 0) continue;
  compose.set(`${a}:${b}`, cp);
}

const fold = new Map();
for (const line of caseFolding.split("\n")) {
  const clean = line.split("#")[0].trim();
  if (clean === "") continue;
  const parts = clean.split(";").map((x) => x.trim());
  const cp = Number.parseInt(parts[0], 16);
  const status = parts[1];
  const mapping = parts[2].split(" ").map((x) => Number.parseInt(x, 16));
  if (status === "F") {
    fold.set(cp, mapping);
  } else if (status === "C" && !fold.has(cp)) {
    fold.set(cp, mapping);
  }
}

function hex(cp) {
  return `0x${cp.toString(16).toUpperCase().padStart(4, "0")}`;
}
function rustString(cps) {
  return JSON.stringify(String.fromCodePoint(...cps));
}

const lines = [];
lines.push("// This file is generated. Do not edit it by hand.");
lines.push("//");
lines.push(`// Source: Unicode Character Database ${unicodeVersion}.`);
lines.push("// Inputs: UnicodeData.txt, CaseFolding.txt, DerivedNormalizationProps.txt.");
lines.push("// The generator lives in `tools/generate_unicode_tables.mjs`.");
lines.push("//");
lines.push("// A canonical decomposition is not NFC by definition. The tokenizer applies");
lines.push("// canonical composition after it reads a table entry.");
lines.push("");
lines.push("#![allow(clippy::unicode_not_nfc)]");
lines.push("");
lines.push("/// Inclusive word-start code point ranges.");
lines.push("pub(crate) static WORD_START_RANGES: &[(u32, u32)] = &[");
for (const [a, b] of wordStart) lines.push(`    (${hex(a)}, ${hex(b)}),`);
lines.push("];");
lines.push("");
lines.push("/// Inclusive word-continue code point ranges.");
lines.push("pub(crate) static WORD_CONTINUE_RANGES: &[(u32, u32)] = &[");
for (const [a, b] of wordContinue) lines.push(`    (${hex(a)}, ${hex(b)}),`);
lines.push("];");
lines.push("");
lines.push("/// Canonical combining class entries. A missing entry has class zero.");
lines.push("pub(crate) static COMBINING_CLASS: &[(u32, u8)] = &[");
for (let cp = 0; cp < MAX; cp += 1) {
  if (ccc[cp] !== 0) lines.push(`    (${hex(cp)}, ${ccc[cp]}),`);
}
lines.push("];");
lines.push("");
lines.push("/// Full canonical decomposition entries. Hangul is handled in code.");
lines.push("pub(crate) static CANONICAL_DECOMPOSITION: &[(u32, &str)] = &[");
for (const cp of [...nfd.keys()].sort((a, b) => a - b)) {
  lines.push(`    (${hex(cp)}, ${rustString(nfd.get(cp))}),`);
}
lines.push("];");
lines.push("");
lines.push("/// Canonical composition pairs. Hangul is handled in code.");
lines.push("pub(crate) static CANONICAL_COMPOSITION: &[(u32, u32, u32)] = &[");
for (const key of [...compose.keys()].sort((a, b) => {
  const [a1, b1] = a.split(":").map(Number);
  const [a2, b2] = b.split(":").map(Number);
  return a1 - a2 || b1 - b2;
})) {
  const [a, b] = key.split(":").map(Number);
  lines.push(`    (${hex(a)}, ${hex(b)}, ${hex(compose.get(key))}),`);
}
lines.push("];");
lines.push("");
lines.push("/// Full case folding entries. A missing entry maps to itself.");
lines.push("pub(crate) static FULL_CASE_FOLD: &[(u32, &str)] = &[");
for (const cp of [...fold.keys()].sort((a, b) => a - b)) {
  lines.push(`    (${hex(cp)}, ${rustString(fold.get(cp))}),`);
}
lines.push("];");
lines.push("");

const out = lines.join("\n");
fs.writeFileSync(path.resolve(outputPath), out);
console.log({
  wordStart: wordStart.length,
  wordContinue: wordContinue.length,
  ccc: [...ccc].filter((x) => x !== 0).length,
  nfd: nfd.size,
  compose: compose.size,
  fold: fold.size,
  bytes: out.length,
});
