// npm run gen:types — regenerates the committed contract type files from the
// JSON Schemas in schemas/v0.1/. See gen-types-lib.mjs for how each file is
// assembled; see gen-types-check.mjs for the CI drift guard.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { generateContract1, generateContract2 } from "./gen-types-lib.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const typesDir = join(here, "..", "src", "lib", "types");

const contract1 = await generateContract1();
const contract2 = await generateContract2();

writeFileSync(join(typesDir, "contract1.ts"), contract1);
writeFileSync(join(typesDir, "contract2.ts"), contract2);

console.log("wrote src/lib/types/contract1.ts");
console.log("wrote src/lib/types/contract2.ts");
