// npm run gen:types:check — CI drift guard. Regenerates contract1.ts/
// contract2.ts to a temp directory and diffs them against the committed
// files under src/lib/types/. Non-zero exit on any drift (including if the
// committed file is missing).
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { generateContract1, generateContract2 } from "./gen-types-lib.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const typesDir = join(here, "..", "src", "lib", "types");

const tmp = mkdtempSync(join(tmpdir(), "af-console-gen-types-check-"));

let drifted = false;
try {
  const generated = {
    "contract1.ts": await generateContract1(),
    "contract2.ts": await generateContract2(),
  };

  for (const [name, freshContent] of Object.entries(generated)) {
    const tmpPath = join(tmp, name);
    writeFileSync(tmpPath, freshContent);

    const committedPath = join(typesDir, name);
    let committedContent;
    try {
      committedContent = readFileSync(committedPath, "utf8");
    } catch {
      console.error(`gen:types:check: ${name} is missing at ${committedPath}`);
      drifted = true;
      continue;
    }

    if (committedContent !== freshContent) {
      console.error(`gen:types:check: ${name} is out of date — run \`npm run gen:types\`.`);
      console.error(`  committed: ${committedPath}`);
      console.error(`  fresh:     ${tmpPath}`);
      drifted = true;
    }
  }
} finally {
  if (!drifted) rmSync(tmp, { recursive: true, force: true });
}

if (drifted) {
  console.error("\ngen:types:check FAILED — committed types have drifted from the schemas.");
  process.exit(1);
} else {
  console.log("gen:types:check passed — contract1.ts/contract2.ts match the schemas.");
}
