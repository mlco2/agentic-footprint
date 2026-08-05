// Shared generation logic for contract1.ts / contract2.ts, used by both
// `gen-types.mjs` (writes the committed files) and `gen-types-check.mjs`
// (regenerates to a temp location and diffs — the CI drift guard).
//
// json-schema-to-typescript compiles one named (sub)schema at a time. Neither
// schema exposes a single reachable root that pulls in everything we need
// (events.schema.json discriminates `payload` by `type` via allOf/if/then,
// which json-schema-to-typescript does not follow; derived.schema.json has no
// root object at all, only `$defs`), so this module compiles each relevant
// subschema individually and assembles/dedupes the result. That assembly is
// the "generation" — nothing here hand-encodes a field the schema doesn't
// declare.
import { compile } from "json-schema-to-typescript";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
export const SCHEMAS_DIR = join(here, "..", "..", "schemas", "v0.1");

const COMPILE_OPTS = { bannerComment: "" };

function toPascalCase(key) {
  return key
    .split("_")
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join("");
}

function readSchema(file) {
  return JSON.parse(readFileSync(join(SCHEMAS_DIR, file), "utf8"));
}

/**
 * Split compiled TS output into its top-level declaration blocks (leading
 * JSDoc comment, if any, plus the `export interface`/`export type`). Splits
 * only on column-0 boundaries so nested/property-level JSDoc comments
 * (always indented) are never mistaken for a new top-level declaration.
 */
function splitDeclarations(ts) {
  const rawParts = ts
    .split(/\n(?=\/\*\*|export (?:interface|type) )/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0);

  const blocks = [];
  let pendingComment = null;
  for (const part of rawParts) {
    if (part.startsWith("export ")) {
      blocks.push((pendingComment ? pendingComment + "\n" : "") + part + "\n");
      pendingComment = null;
    } else {
      // A bare leading comment: hold it until the following declaration.
      pendingComment = part;
    }
  }
  return blocks;
}

function declarationName(block) {
  const m = block.match(/export (?:interface|type) (\w+)/);
  if (!m) throw new Error(`could not find a declaration name in block:\n${block}`);
  return m[1];
}

/** Merge declaration blocks from multiple compile() calls, first-seen wins, in order. */
function dedupeBlocks(blockLists) {
  const seen = new Set();
  const out = [];
  for (const blocks of blockLists) {
    for (const block of blocks) {
      const name = declarationName(block);
      if (seen.has(name)) continue;
      seen.add(name);
      out.push(block);
    }
  }
  return out;
}

function fileHeader(sourceFile) {
  return [
    "// GENERATED FILE — do not hand-edit.",
    `// Source: schemas/v0.1/${sourceFile}`,
    "// Regenerate with `npm run gen:types` (console/scripts/gen-types.mjs).",
    "// `npm run gen:types:check` fails if this file drifts from a fresh regeneration.",
    "",
  ].join("\n");
}

/**
 * contract1.ts — Contract #1 (raw collector events).
 *
 * events.schema.json's root schema describes the envelope (schema_version,
 * event_id, type, ts, collector, session_id, attribution) plus a generic
 * `payload`; the five concrete payload shapes live under `$defs` and are
 * selected by `type` via `allOf`/`if`/`then`, which json-schema-to-typescript
 * does not resolve into a discriminated union. So: compile the envelope
 * (with `type`/`payload` removed) and each `$defs` payload separately, then
 * compose the discriminated union `FactEvent` from the event schema's own
 * `type` enum — the enum values are the only hand-touched strings here, and
 * they are read out of the schema, not retyped.
 */
export async function generateContract1() {
  const schema = readSchema("events.schema.json");
  const typeEnum = schema.properties.type.enum;
  const defs = schema.$defs;

  const envelopeSchema = {
    ...schema,
    properties: Object.fromEntries(
      Object.entries(schema.properties).filter(([k]) => k !== "type" && k !== "payload"),
    ),
    required: schema.required.filter((k) => k !== "type" && k !== "payload"),
  };
  delete envelopeSchema.allOf;
  delete envelopeSchema.$defs;
  delete envelopeSchema.title; // let the explicit "EventEnvelopeBase" name below win
  delete envelopeSchema.$id;

  const envelopeTs = await compile(envelopeSchema, "EventEnvelopeBase", COMPILE_OPTS);

  const payloadBlockLists = [];
  const unionArms = [];
  for (const typeName of typeEnum) {
    const tsName = toPascalCase(typeName);
    const ts = await compile(defs[typeName], tsName, COMPILE_OPTS);
    payloadBlockLists.push(splitDeclarations(ts));
    unionArms.push(`  | (EventEnvelopeBase & { type: "${typeName}"; payload: ${tsName} })`);
  }

  const payloadBlocks = dedupeBlocks(payloadBlockLists);
  const envelopeBlocks = splitDeclarations(envelopeTs);

  const union = [
    "/**",
    " * One Contract #1 event, envelope plus its discriminated payload.",
    " * `type` narrows `payload` to the matching $defs shape.",
    " */",
    "export type FactEvent =",
    unionArms.join("\n") + ";",
    "",
  ].join("\n");

  return (
    fileHeader("events.schema.json") +
    "\n" +
    envelopeBlocks.join("\n") +
    "\n" +
    payloadBlocks.join("\n") +
    "\n" +
    union
  );
}

/**
 * contract2.ts — Contract #2 (derived/control-plane records, informative in v0.1).
 *
 * derived.schema.json has no root object, only `$defs` cross-referencing each
 * other (`impact_join` -> `impacts` -> `criterion` -> `range`, etc). Compile
 * the two "entry point" defs the console needs (`impact_join`,
 * `impact_estimate`) with the full `$defs` map attached so internal $refs
 * resolve, then dedupe the transitively-pulled-in declarations.
 */
export async function generateContract2() {
  const schema = readSchema("derived.schema.json");
  const defs = schema.$defs;
  const entryPoints = ["impact_join", "impact_estimate"];

  const blockLists = [];
  for (const key of entryPoints) {
    const sub = { ...defs[key], $defs: defs };
    const ts = await compile(sub, toPascalCase(key), COMPILE_OPTS);
    blockLists.push(splitDeclarations(ts));
  }

  const blocks = dedupeBlocks(blockLists);
  // Order low-level (Range) to high-level (ImpactJoin) for readability.
  const order = ["Range", "Criterion", "Impacts", "ImpactEstimate", "ImpactJoin"];
  blocks.sort((a, b) => order.indexOf(declarationName(a)) - order.indexOf(declarationName(b)));

  return fileHeader("derived.schema.json") + "\n" + blocks.join("\n");
}
