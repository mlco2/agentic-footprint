// Tripwire (not a parser) for global-constraints.md #1 / DATA-CONTRACT §0:
// "the client computes nothing" — no impacts/attribution/apportionment/grid
// conversion/L1/L2 arithmetic in the browser. Two independent checks:
//
//   (a) an arithmetic operator (+, *, /, or a whitespace-padded -) directly
//       adjacent to an identifier matching /(_j\b|share|gwp|watts)/ anywhere
//       under console/src/**, EXCEPT the five files DATA-CONTRACT §0
//       sanctions as display-aggregation sites (format.ts — the only module
//       allowed to touch numbers at all — plus the four selectors modules
//       that do the handful of explicitly-sanctioned display aggregations:
//       selectors/timeline.ts, selectors/attribution.ts,
//       selectors/inspector.ts, selectors/impact.ts). Keep this allowlist in
//       sync with those files' own header comments, not the other way
//       around — they're the source of truth for what's sanctioned.
//   (b) a literal `Date.now(` / `Math.random(` call anywhere under
//       console/src/lib/selectors/ (global-constraints.md #5: "selectors
//       are pure modules ... memoised, no Date.now()/Math.random() inside").
//
// Grep-style on purpose: strips each line's `//` comment tail before
// matching, so doc comments that quote these exact patterns in prose (this
// file's own header included, and several selectors' own "no Date.now()"
// header comments) don't self-trip. It does NOT understand block comments,
// strings, or template literals — calibrated so the current tree passes;
// treat a new failure as "go look", not "definitely a bug".
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL(".", import.meta.url));
const srcDir = join(here, "..", "src");

// Relative to console/src/, using the platform separator so the Set lookup
// below works on both POSIX and Windows checkouts.
const ARITH_ALLOWLIST = new Set(
  ["format.ts", "lib/selectors/timeline.ts", "lib/selectors/attribution.ts", "lib/selectors/inspector.ts", "lib/selectors/impact.ts"].map((p) =>
    p.split("/").join(sep),
  ),
);

const CODE_EXTENSIONS = new Set([".ts", ".svelte"]);

/** @param {string} dir */
function walk(dir) {
  /** @type {string[]} */
  const files = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const stat = statSync(full);
    if (stat.isDirectory()) {
      files.push(...walk(full));
    } else {
      files.push(full);
    }
  }
  return files;
}

/** Everything before a line's first `//` — a deliberately naive comment
 * strip (doesn't know about strings/regex literals containing `//`), good
 * enough for a tripwire scanning selector/format code that has no reason to
 * carry raw URLs (global-constraints.md #4: relative URLs only). */
function stripLineComment(line) {
  const idx = line.indexOf("//");
  return idx === -1 ? line : line.slice(0, idx);
}

// --- check (a): arithmetic adjacent to _j / share / gwp / watts ------------

const SENSITIVE_IDENT = String.raw`[A-Za-z_][A-Za-z0-9_.]*(?:_j\b|share|gwp|watts)[A-Za-z0-9_]*`;
// `+`/`*`/`/` count regardless of spacing; `-` only counts flanked by
// whitespace, so BEM/kebab-case class-name tokens like `detail-row__share`
// (hyphen with no surrounding space, everywhere in this codebase's CSS
// classes) don't false-positive as subtraction.
const ARITH_OP = String.raw`(?:\s*[+*/]\s*|\s-\s)`;
const ARITH_ADJACENT_RE = new RegExp(`(${SENSITIVE_IDENT})${ARITH_OP}[A-Za-z0-9_.(]|[A-Za-z0-9_.)]${ARITH_OP}(${SENSITIVE_IDENT})`);

let violations = 0;

for (const file of walk(srcDir)) {
  const ext = file.slice(file.lastIndexOf("."));
  if (!CODE_EXTENSIONS.has(ext)) continue;
  const relPath = relative(srcDir, file);
  if (ARITH_ALLOWLIST.has(relPath)) continue;

  const lines = readFileSync(file, "utf8").split("\n");
  lines.forEach((rawLine, i) => {
    const line = stripLineComment(rawLine);
    if (ARITH_ADJACENT_RE.test(line)) {
      violations += 1;
      console.error(`${relative(process.cwd(), file)}:${i + 1}: arithmetic adjacent to a _j/share/gwp/watts identifier outside the sanctioned files — ${line.trim()}`);
    }
  });
}

// --- check (b): Date.now()/Math.random() inside selectors ------------------

const selectorsDir = join(srcDir, "lib", "selectors");
const CLOCK_RANDOM_RE = /Date\.now\(|Math\.random\(/;

for (const file of walk(selectorsDir)) {
  if (!file.endsWith(".ts")) continue;

  const lines = readFileSync(file, "utf8").split("\n");
  lines.forEach((rawLine, i) => {
    const line = stripLineComment(rawLine);
    if (CLOCK_RANDOM_RE.test(line)) {
      violations += 1;
      console.error(`${relative(process.cwd(), file)}:${i + 1}: Date.now()/Math.random() inside selectors/ — ${line.trim()}`);
    }
  });
}

if (violations > 0) {
  console.error(`\nlint:arith failed — ${violations} violation(s) found.`);
  process.exit(1);
} else {
  console.log("lint:arith passed — no unsanctioned arithmetic, no clock/random in selectors.");
}
