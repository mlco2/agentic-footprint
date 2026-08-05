// Fails if a raw hex colour literal (#abc, #aabbcc, #aabbccdd, ...) appears
// anywhere under console/src, outside the one file allowed to carry them:
// the verbatim-vendored Broadsheet stylesheet. Every colour in the console
// must come from a Broadsheet CSS variable (global-constraints.md #2).
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL(".", import.meta.url));
const srcDir = join(here, "..", "src");
const allowFile = join(srcDir, "styles", "broadsheet.css");

const HEX_RE = /#[0-9a-fA-F]{3,8}\b/g;

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

let violations = 0;

for (const file of walk(srcDir)) {
  if (file === allowFile) continue;

  const text = readFileSync(file, "utf8");
  const lines = text.split("\n");
  lines.forEach((line, i) => {
    const matches = line.match(HEX_RE);
    if (matches) {
      violations += matches.length;
      console.error(
        `${relative(process.cwd(), file)}:${i + 1}: raw hex ${matches.join(", ")}`,
      );
    }
  });
}

if (violations > 0) {
  console.error(
    `\nlint:hex failed — ${violations} raw hex value(s) found outside broadsheet.css.`,
  );
  process.exit(1);
} else {
  console.log("lint:hex passed — no raw hex outside broadsheet.css.");
}
