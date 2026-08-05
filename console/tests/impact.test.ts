// @vitest-environment happy-dom
//
// Tests for console/src/lib/selectors/impact.ts (Task 8 brief), plus a
// mounted-component regression test for the ImpactCards badge cascade bug
// (see the "ImpactCards badge alarm styling" describe block at the bottom —
// same technique as timeline.test.ts's own hidden-tab-discipline suite:
// happy-dom + Svelte's own `mount`/`unmount`/`flushSync`). The environment
// directive above applies to the whole file, but every OTHER test here is
// plain logic unaffected by which environment runs it.
//
// Unlike attribution.test.ts/inspector.test.ts, the selector tests below
// need no vi.resetModules()/fresh-module-graph dance: selectors/impact.ts
// imports no store singletons — every selector here is a pure function of
// the `DebugReport | undefined` (+ `SessionInfo | null` for the aside) the
// caller passes in, so plain fixture objects are enough.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import type { Criterion, ImpactEstimate, ImpactJoin, Impacts } from "../src/lib/types/contract2";
import type { DebugReport, ModelImpactGroup, SessionInfo } from "../src/lib/types/debug";
import { fmtGridIntensity, fmtJoules, fmtPct, fmtRange, fmtStatuslineFloat } from "../src/lib/format";
import {
  CRITERIA,
  selectCriteriaTable,
  selectImpactAside,
  selectImpactCards,
  selectPerModel,
} from "../src/lib/selectors/impact";
import type { ImpactCardModel } from "../src/lib/selectors/impact";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

function criterion(unit: string, min: number, max: number, extra: Partial<Criterion> = {}): Criterion {
  return { unit, total: { min, max }, ...extra };
}

function sessionFixture(overrides: Partial<SessionInfo> = {}): SessionInfo {
  return {
    session_id: "ses_test",
    session_meta: { agent_app: { name: "claude-code" } } as unknown as SessionInfo["session_meta"],
    t_start: "2026-07-25T09:40:12.004Z",
    attribution_policy: "l2_cpu_time",
    methodology: { version: "v2026.06.1", source: "bundled", ecologits_version: "0.7.1", codecarbon_version: "3.0.4" },
    grid: { zone: "FRA", g_co2e_per_kwh: 56, source: "codecarbon data v2026.06" },
    state_dir: "/tmp/af",
    schema_version: "0.1.0",
    mode: "watch --debug",
    ...overrides,
  };
}

/** A fully-populated join: local + remote + combined all present for
 * energy/gwp, remote also carrying adpe/pe/water — the "everything measured"
 * case, used as the baseline for provenance/exact-format assertions. */
function fullJoin(overrides: Partial<ImpactJoin> = {}): ImpactJoin {
  return {
    unit: { level: "session", session_id: "ses_test" },
    t_start: "2026-07-25T09:40:12.004Z",
    t_end: "2026-07-25T09:45:12.004Z",
    attribution_policy: "l2_cpu_time",
    local_measured: {
      energy: criterion("kWh", 0.012, 0.012),
      gwp: criterion("kgCO2eq", 0.0008, 0.0008),
      baseline_share_excluded: true,
      coverage: 0.86,
    },
    remote_estimated: {
      impacts: {
        energy: criterion("kWh", 0.002, 0.004),
        gwp: criterion("kgCO2eq", 0.001, 0.002),
        adpe: criterion("kgSbeq", 0.000001, 0.000002),
        pe: criterion("MJ", 0.02, 0.04),
        water: criterion("L", 0.002, 0.003),
      },
      llm_calls: 3,
    },
    combined_total: {
      energy: criterion("kWh", 0.014, 0.016),
      gwp: criterion("kgCO2eq", 0.0018, 0.0026),
    },
    unmeasured_remote_spans: 2,
    ...overrides,
  };
}

function fullReport(overrides: Partial<DebugReport> = {}): DebugReport {
  return {
    level: "session",
    impact_join: fullJoin(),
    by_model: [],
    estimation_status_histogram: { ok: 2, unknown_model: 1, missing_zone: 0, pending: 0, error: 0 },
    ...overrides,
  };
}

function modelGroup(modelId: string, estimates: ImpactEstimate[], impacts: Impacts | undefined = undefined): ModelImpactGroup {
  return { model_id: modelId, estimates, impacts: impacts ?? {} };
}

function okEstimate(eventId: string, impacts: Impacts): ImpactEstimate {
  return { event_id: eventId, estimation_status: "ok", impacts, methodology: { version: "v2026.06.1", source: "bundled", ecologits_version: "0.7.1" } };
}

function unknownEstimate(eventId: string): ImpactEstimate {
  return { event_id: eventId, estimation_status: "unknown_model", methodology: { version: "ecologits-unknown", source: "bundled" } };
}

// ---------------------------------------------------------------------------
// selectImpactCards
// ---------------------------------------------------------------------------

describe("selectImpactCards", () => {
  it("returns [] when no report has arrived yet", () => {
    expect(selectImpactCards(undefined)).toEqual([]);
  });

  it("returns exactly three cards with the right keys/swatches, in order", () => {
    const cards = selectImpactCards(fullReport());
    expect(cards.map((c) => c.key)).toEqual(["local", "remote", "combined"]);
    expect(cards.map((c) => c.swatch)).toEqual(["local", "remote", "combined"]);
    expect(cards[0].eyebrow).toBe("local · measured");
    expect(cards[1].eyebrow).toBe("remote · modelled");
    expect(cards[2].eyebrow).toBe("combined · cross-paradigm");
  });

  it("renders 'not measured' (never '0') for a criterion with no local measurement, and marks it unmeasured", () => {
    const report = fullReport({ impact_join: fullJoin({ local_measured: undefined, combined_total: undefined }) });
    const cards = selectImpactCards(report);
    const local = cards.find((c) => c.key === "local")!;
    const combined = cards.find((c) => c.key === "combined")!;
    expect(local.valueLabel).toBe("not measured");
    expect(local.measured).toBe(false);
    expect(local.valueLabel).not.toContain("0");
    expect(combined.valueLabel).toBe("not measured");
    expect(combined.measured).toBe(false);
  });

  it("marks a present criterion as measured, with a verbatim value+unit label", () => {
    const cards = selectImpactCards(fullReport());
    const local = cards.find((c) => c.key === "local")!;
    expect(local.measured).toBe(true);
    expect(local.valueLabel).toBe(`${fmtRange({ min: 0.012, max: 0.012 })} kWh`);
  });

  it("is memoisation-stable: same report reference in -> same array reference out; a new report -> a new array", () => {
    const report = fullReport();
    const a = selectImpactCards(report);
    const b = selectImpactCards(report);
    expect(b).toBe(a);
    const c = selectImpactCards(fullReport());
    expect(c).not.toBe(a);
  });
});

// ---------------------------------------------------------------------------
// selectCriteriaTable
// ---------------------------------------------------------------------------

describe("selectCriteriaTable", () => {
  it("returns [] when no report has arrived yet", () => {
    expect(selectCriteriaTable(undefined)).toEqual([]);
  });

  it("returns exactly one row per Contract #2 criterion, in the declared order", () => {
    const rows = selectCriteriaTable(fullReport());
    expect(rows.map((r) => r.criterion)).toEqual([...CRITERIA]);
  });

  it("adpe/pe/water rows (never locally measured) read 'not measured' in the local column and 'not measured · remote only' in combined — never '0'", () => {
    const rows = selectCriteriaTable(fullReport());
    for (const key of ["adpe", "pe", "water"] as const) {
      const row = rows.find((r) => r.criterion === key)!;
      expect(row.localLabel).toBe("not measured");
      expect(row.localMeasured).toBe(false);
      expect(row.combinedLabel).toBe("not measured · remote only");
      expect(row.localLabel).not.toContain("0");
    }
  });

  it("a criterion missing on BOTH sides reads plain 'not measured' in combined, no '· remote only' claim", () => {
    const report = fullReport({
      impact_join: fullJoin({
        remote_estimated: { impacts: {}, llm_calls: 0 },
        local_measured: undefined,
        combined_total: undefined,
      }),
    });
    const rows = selectCriteriaTable(report);
    const energyRow = rows.find((r) => r.criterion === "energy")!;
    expect(energyRow.localLabel).toBe("not measured");
    expect(energyRow.remoteLabel).toBe("not measured");
    expect(energyRow.combinedLabel).toBe("not measured");
  });

  it("energy/gwp rows have a fully-measured combined column when combined_total is present", () => {
    const rows = selectCriteriaTable(fullReport());
    const energyRow = rows.find((r) => r.criterion === "energy")!;
    expect(energyRow.combinedMeasured).toBe(true);
    expect(energyRow.combinedLabel).toBe(`${fmtRange({ min: 0.014, max: 0.016 })} kWh`);
  });

  it("split-bar proportions sum to 1 when both sides are present, and are 0/0 when neither is", () => {
    const rows = selectCriteriaTable(fullReport());
    const energyRow = rows.find((r) => r.criterion === "energy")!;
    expect(energyRow.splitLocalFraction + energyRow.splitRemoteFraction).toBeCloseTo(1, 10);
    expect(energyRow.splitLocalFraction).toBeGreaterThan(0);
    expect(energyRow.splitRemoteFraction).toBeGreaterThan(0);

    const report = fullReport({ impact_join: fullJoin({ remote_estimated: { impacts: {}, llm_calls: 0 }, local_measured: undefined, combined_total: undefined }) });
    const noneRow = selectCriteriaTable(report).find((r) => r.criterion === "energy")!;
    expect(noneRow.splitLocalFraction).toBe(0);
    expect(noneRow.splitRemoteFraction).toBe(0);
  });

  it("is memoisation-stable across identical report references", () => {
    const report = fullReport();
    const a = selectCriteriaTable(report);
    const b = selectCriteriaTable(report);
    expect(b).toBe(a);
  });
});

// ---------------------------------------------------------------------------
// selectPerModel — unknown_model magenta + excluded from totals
// ---------------------------------------------------------------------------

describe("selectPerModel", () => {
  it("returns [] rows and a 'not measured' totals row when no report has arrived yet", () => {
    const model = selectPerModel(undefined);
    expect(model.rows).toEqual([]);
    for (const key of CRITERIA) expect(model.totals[key].measured).toBe(false);
  });

  it("flags a group with an unknown_model estimate as isUnknown, with its status carried verbatim", () => {
    const report = fullReport({
      by_model: [
        modelGroup("claude-sonnet-4-5-20250929", [okEstimate("e1", fullJoin().remote_estimated!.impacts!)], fullJoin().remote_estimated!.impacts),
        modelGroup("acme-mystery-7b", [unknownEstimate("e2")]),
      ],
    });
    const model = selectPerModel(report);
    const unknownRow = model.rows.find((r) => r.modelId === "acme-mystery-7b")!;
    expect(unknownRow.isUnknown).toBe(true);
    expect(unknownRow.statusLabel).toBe("unknown_model");
    const okRow = model.rows.find((r) => r.modelId === "claude-sonnet-4-5-20250929")!;
    expect(okRow.isUnknown).toBe(false);
    expect(okRow.statusLabel).toBeUndefined();
  });

  it("totals are the server's OWN remote_estimated.impacts, rendered verbatim — NOT a client sum over model rows (the two ok rows below sum to something different), and structurally cannot include the unknown_model row's (absent) impacts", () => {
    // Two `ok` models with DELIBERATELY DIFFERENT impacts, chosen so their
    // sum disagrees with the server's own totals below — a test that used
    // identical values (or a single model) couldn't distinguish "renders the
    // server's total verbatim" from "sums the rows itself", since both would
    // produce the same label by coincidence.
    const modelAImpacts: Impacts = {
      energy: criterion("kWh", 0.001, 0.0015),
      gwp: criterion("kgCO2eq", 0.0004, 0.0006),
      adpe: criterion("kgSbeq", 0.0000004, 0.0000006),
      pe: criterion("MJ", 0.01, 0.012),
      water: criterion("L", 0.0008, 0.001),
    };
    const modelBImpacts: Impacts = {
      energy: criterion("kWh", 0.0007, 0.0009),
      gwp: criterion("kgCO2eq", 0.0003, 0.0004),
      adpe: criterion("kgSbeq", 0.0000003, 0.0000004),
      pe: criterion("MJ", 0.006, 0.008),
      water: criterion("L", 0.0005, 0.0006),
    };
    // Sum of A+B would be energy {0.0017,0.0024} etc — the server's total
    // below is a round, unrelated number no row-summing could produce.
    const serverImpacts: Impacts = {
      energy: criterion("kWh", 0.05, 0.06),
      gwp: criterion("kgCO2eq", 0.02, 0.03),
      adpe: criterion("kgSbeq", 0.00002, 0.00003),
      pe: criterion("MJ", 0.5, 0.6),
      water: criterion("L", 0.2, 0.3),
    };
    const join = fullJoin({ remote_estimated: { impacts: serverImpacts, llm_calls: 3 } });
    const report = fullReport({
      impact_join: join,
      by_model: [
        modelGroup("model-a", [okEstimate("e1", modelAImpacts)], modelAImpacts),
        modelGroup("model-b", [okEstimate("e2", modelBImpacts)], modelBImpacts),
        modelGroup("acme-mystery-7b", [unknownEstimate("e3")]), // impacts: {} — no data to sum even if this file tried to
      ],
    });
    const model = selectPerModel(report);

    // The totals row equals fmtRange/unit applied DIRECTLY to
    // impact_join.remote_estimated.impacts — never a sum over `model.rows`.
    for (const key of CRITERIA) {
      const c = serverImpacts[key]!;
      const expectedLabel = `${fmtRange(c.total)} ${c.unit}`;
      expect(model.totals[key].label).toBe(expectedLabel);

      // Explicitly discriminate from row-summing: A's + B's own labels are
      // NOT what the totals row shows (they aren't even the same shape as a
      // sum would produce, since this file performs no arithmetic on them).
      expect(model.rows.find((r) => r.modelId === "model-a")!.cells[key].label).not.toBe(expectedLabel);
      expect(model.rows.find((r) => r.modelId === "model-b")!.cells[key].label).not.toBe(expectedLabel);
    }

    // The unknown row carries no impacts to have been summed in the first place.
    const unknownRow = model.rows.find((r) => r.modelId === "acme-mystery-7b")!;
    for (const key of CRITERIA) expect(unknownRow.cells[key].measured).toBe(false);
  });

  it("is memoisation-stable across identical report references", () => {
    const report = fullReport({ by_model: [modelGroup("m1", [unknownEstimate("e1")])] });
    const a = selectPerModel(report);
    const b = selectPerModel(report);
    expect(b).toBe(a);
  });
});

// ---------------------------------------------------------------------------
// selectImpactAside — histogram, statusline preview (exact string), aside rows
// ---------------------------------------------------------------------------

describe("selectImpactAside", () => {
  it("returns empty misses/histogram and an all-nan statusline preview when no report has arrived yet", () => {
    const model = selectImpactAside(undefined, null);
    expect(model.tokenOnlyMisses).toEqual([]);
    expect(model.histogram).toEqual([]);
    expect(model.statuslineLines).toEqual(["gwp wcf energy adpe pe", "nan nan nan nan nan"]);
    expect(model.methodology).toBeUndefined();
  });

  it("histogram counts match the fixture exactly, zero-filling the four canonical statuses the brief names plus 'error', and folding in missing_usage only when present", () => {
    const report = fullReport({ estimation_status_histogram: { ok: 2, unknown_model: 1, missing_zone: 0, pending: 0, error: 0 } });
    const model = selectImpactAside(report, null);
    expect(model.histogram).toEqual([
      { status: "ok", count: 2 },
      { status: "pending", count: 0 },
      { status: "unknown_model", count: 1 },
      { status: "missing_zone", count: 0 },
      { status: "error", count: 0 },
    ]);

    const withMissingUsage = fullReport({ estimation_status_histogram: { ok: 1, unknown_model: 0, missing_zone: 0, pending: 0, error: 0, missing_usage: 3 } });
    const model2 = selectImpactAside(withMissingUsage, null);
    expect(model2.histogram.find((r) => r.status === "missing_usage")).toEqual({ status: "missing_usage", count: 3 });
  });

  it("all six 'what token-only misses' rows are present, verbatim report fields, honestly 'not reported'/'not measured' when absent — never fabricated as 0", () => {
    const model = selectImpactAside(fullReport(), null);
    expect(model.tokenOnlyMisses).toHaveLength(6);
    const byLabel = Object.fromEntries(model.tokenOnlyMisses.map((r) => [r.label, r.value]));
    expect(byLabel["local energy"]).toBe(`${fmtRange({ min: 0.012, max: 0.012 })} kWh`);
    expect(byLabel["coverage"]).toBe(fmtPct(0.86));
    expect(byLabel["baseline share excluded"]).toBe("yes");
    // breakdown_j is a real-server-only extra property this mock fixture
    // doesn't carry — must read as honestly absent, never as "0 J".
    expect(byLabel["orphaned compute"]).toBe("not reported");
    expect(byLabel["agent's own share"]).toBe("not reported");
    expect(byLabel["unmeasured remote spans"]).toBe("2 spans");
  });

  it("'unmeasured remote spans' reports a genuine 0 as a real count (not 'not measured') and carries no alarm tone at 0", () => {
    const report = fullReport({ impact_join: fullJoin({ unmeasured_remote_spans: 0 }) });
    const model = selectImpactAside(report, null);
    const row = model.tokenOnlyMisses.find((r) => r.label === "unmeasured remote spans")!;
    expect(row.value).toBe("0 spans");
    expect(row.tone).toBeUndefined();
  });

  it("a report with no local_measured at all reads 'not measured'/'not reported' for every local-derived row, never '0'", () => {
    const report = fullReport({ impact_join: fullJoin({ local_measured: undefined }) });
    const model = selectImpactAside(report, null);
    const byLabel = Object.fromEntries(model.tokenOnlyMisses.map((r) => [r.label, r.value]));
    expect(byLabel["local energy"]).toBe("not measured");
    expect(byLabel["coverage"]).toBe("not reported");
    expect(byLabel["baseline share excluded"]).toBe("not reported");
    for (const v of Object.values(byLabel)) expect(v).not.toBe("0");
  });

  it("statusline preview: exact two-line design-log format, values = range means, computed only here", () => {
    const report = fullReport();
    const model = selectImpactAside(report, null);
    const join = report.impact_join;
    const gwpMean = (join.combined_total!.gwp!.total.min + join.combined_total!.gwp!.total.max) / 2;
    const waterMean = (join.remote_estimated!.impacts!.water!.total.min + join.remote_estimated!.impacts!.water!.total.max) / 2;
    const energyMean = (join.combined_total!.energy!.total.min + join.combined_total!.energy!.total.max) / 2;
    const adpeMean = (join.remote_estimated!.impacts!.adpe!.total.min + join.remote_estimated!.impacts!.adpe!.total.max) / 2;
    const peMean = (join.remote_estimated!.impacts!.pe!.total.min + join.remote_estimated!.impacts!.pe!.total.max) / 2;
    const expectedLine2 = [gwpMean, waterMean, energyMean, adpeMean, peMean].map(fmtStatuslineFloat).join(" ");

    expect(model.statuslineLines).toEqual(["gwp wcf energy adpe pe", expectedLine2]);
    // Assert the exact literal string too (brief: "assert exact string") —
    // clean fixture decimals chosen so the expected string is hand-verifiable:
    // gwp = combined_total.gwp mean = (0.0018+0.0026)/2 = 0.0022
    // wcf(water) = remote water mean = (0.002+0.003)/2 = 0.0025
    // energy = combined_total.energy mean = (0.014+0.016)/2 = 0.015
    // adpe = remote adpe mean = (0.000001+0.000002)/2 = 0.0000015
    // pe = remote pe mean = (0.02+0.04)/2 = 0.03
    expect(model.statuslineLines[0]).toBe("gwp wcf energy adpe pe");
    expect(model.statuslineLines[1]).toBe("0.0022 0.0025 0.015 0.0000015 0.03");
  });

  it("statusline preview: 'nan' (not 0, not a crash) for a wholly-unmeasured criterion", () => {
    const report = fullReport({
      impact_join: fullJoin({ local_measured: undefined, combined_total: undefined, remote_estimated: { impacts: {}, llm_calls: 0 } }),
    });
    const model = selectImpactAside(report, null);
    expect(model.statuslineLines[1]).toBe("nan nan nan nan nan");
  });

  it("gwp does NOT fall back to the local-only measurement (design-log: a local-only gwp reads as 'session emitted almost nothing')", () => {
    const report = fullReport({ impact_join: fullJoin({ combined_total: undefined, remote_estimated: { impacts: {}, llm_calls: 0 } }) });
    const model = selectImpactAside(report, null);
    // gwp: no combined_total, no remote gwp -> nan, even though local_measured.gwp IS present.
    const gwpToken = model.statuslineLines[1].split(" ")[0];
    expect(gwpToken).toBe("nan");
    // energy DOES fall back to local_measured.energy, so it's finite.
    const energyToken = model.statuslineLines[1].split(" ")[2];
    expect(energyToken).not.toBe("nan");
  });

  it("methodology block renders session fields verbatim, including a null grid factor honestly (never 0)", () => {
    const session = sessionFixture({ grid: { zone: "WOR", g_co2e_per_kwh: null, source: "default" }, methodology: { version: "unknown until the first estimate", source: "bundled" } });
    const model = selectImpactAside(fullReport(), session);
    expect(model.methodology).toEqual({
      versionLabel: "unknown until the first estimate",
      sourceLabel: "bundled",
      ecologitsLabel: "not yet known",
      codecarbonLabel: "not yet known",
      gridZoneLabel: "WOR",
      gridIntensityLabel: fmtGridIntensity(null, "default"),
    });
    expect(model.methodology!.gridIntensityLabel).not.toContain("0 gCO2e");
  });

  it("is memoisation-stable across identical (report, session) references", () => {
    const report = fullReport();
    const session = sessionFixture();
    const a = selectImpactAside(report, session);
    const b = selectImpactAside(report, session);
    expect(b).toBe(a);
    const c = selectImpactAside(fullReport(), session);
    expect(c).not.toBe(a);
  });
});

// ---------------------------------------------------------------------------
// Provenance discipline — every J/kJ/%/kWh/kgCO2eq/... numeric-ish token this
// file's selectors render must trace to `format.ts` applied to a real
// impact_join/by_model field, or to EXACTLY the two sanctioned derived
// values named in selectors/impact.ts's header comment: split-bar
// proportions (never rendered as text — asserted separately above) and the
// statusline preview's range means (its own exact-string test above). Same
// technique as attribution.test.ts's/inspector.test.ts's provenance sweep.
// ---------------------------------------------------------------------------

describe("provenance discipline", () => {
  // Broad enough to catch every unit this file emits (J/kJ/%, plus the
  // impact criteria's own units) without being so broad it also matches
  // prose. fmtRange's dash-joined numbers are matched as a whole "X–Y" token
  // so both endpoints are visible in one match (see the fmtRange-specific
  // test below for the never-a-single-number assertion).
  const NUMERIC_TOKEN_RE = /-?\d[\d,]*(?:\.\d+)?(?:–-?\d[\d,]*(?:\.\d+)?)?\s?(?:kJ|J|%|kWh|kgCO2eq|kgSbeq|MJ|L)\b/g;

  function extractTokens(strings: readonly string[]): string[] {
    const out: string[] = [];
    for (const s of strings) {
      const m = s.match(NUMERIC_TOKEN_RE);
      if (m) out.push(...m);
    }
    return out;
  }

  it("selectImpactCards + selectCriteriaTable + selectPerModel + selectImpactAside render no such token that isn't format.ts applied to a real report field", () => {
    const join = fullJoin();
    const report = fullReport({
      impact_join: join,
      by_model: [
        modelGroup("claude-sonnet-4-5-20250929", [okEstimate("e1", join.remote_estimated!.impacts!)], join.remote_estimated!.impacts),
        modelGroup("acme-mystery-7b", [unknownEstimate("e2")]),
      ],
    });
    const session = sessionFixture();

    const cards = selectImpactCards(report);
    const criteriaRows = selectCriteriaTable(report);
    const perModel = selectPerModel(report);
    const aside = selectImpactAside(report, session);

    const allowedTokens = new Set<string>();
    const registerCriterion = (c: Criterion | undefined) => {
      if (!c) return;
      allowedTokens.add(`${fmtRange(c.total)} ${c.unit}`);
      allowedTokens.add(fmtRange(c.total)); // criteria-table cells render the range alone (unit is a separate column)
      if (c.usage) allowedTokens.add(`usage ${fmtRange(c.usage)}`);
      if (c.embodied) allowedTokens.add(`embodied ${fmtRange(c.embodied)}`);
    };
    registerCriterion(join.local_measured?.energy);
    registerCriterion(join.local_measured?.gwp);
    registerCriterion(join.remote_estimated?.impacts?.energy);
    registerCriterion(join.remote_estimated?.impacts?.gwp);
    registerCriterion(join.remote_estimated?.impacts?.adpe);
    registerCriterion(join.remote_estimated?.impacts?.pe);
    registerCriterion(join.remote_estimated?.impacts?.water);
    registerCriterion(join.combined_total?.energy);
    registerCriterion(join.combined_total?.gwp);
    allowedTokens.add(`gwp ${fmtRange(join.local_measured!.gwp!.total)} ${join.local_measured!.gwp!.unit}`);
    allowedTokens.add(`gwp ${fmtRange(join.combined_total!.gwp!.total)} ${join.combined_total!.gwp!.unit}`);
    allowedTokens.add(`adpe ${fmtRange(join.remote_estimated!.impacts!.adpe!.total)} ${join.remote_estimated!.impacts!.adpe!.unit}`);
    allowedTokens.add(`pe ${fmtRange(join.remote_estimated!.impacts!.pe!.total)} ${join.remote_estimated!.impacts!.pe!.unit}`);
    allowedTokens.add(`water ${fmtRange(join.remote_estimated!.impacts!.water!.total)} ${join.remote_estimated!.impacts!.water!.unit}`);
    allowedTokens.add(fmtPct(join.local_measured!.coverage!));
    allowedTokens.add(fmtJoules(0)); // never actually emitted by this fixture (no breakdown_j) — placeholder kept out of the allow-list on purpose; see assertion below instead.

    const renderedStrings: string[] = [
      ...cards.flatMap((c) => [c.valueLabel, c.rangeLine, c.secondaryLine, ...c.badges.map((b) => b.label)]),
      ...criteriaRows.flatMap((r) => [r.localLabel, r.remoteLabel, r.combinedLabel]),
      ...perModel.rows.flatMap((r) => CRITERIA.map((k) => r.cells[k].label)),
      ...CRITERIA.map((k) => perModel.totals[k].label),
      ...aside.tokenOnlyMisses.map((r) => r.value),
    ].filter((v): v is string => typeof v === "string");

    const tokens = extractTokens(renderedStrings);
    expect(tokens.length).toBeGreaterThan(0); // sanity: the test isn't vacuously true
    for (const token of tokens) {
      expect(allowedTokens.has(token)).toBe(true);
    }

    // The statusline preview is the ONE place a mean appears — its tokens
    // are plain decimals with NO unit suffix (fmtStatuslineFloat never
    // appends one), so the regex above (which requires a unit) can't even
    // match them; asserted here explicitly instead, mirroring the
    // dedicated exact-string test above.
    expect(aside.statuslineLines[1]).not.toMatch(/kJ|J|%|kWh|kgCO2eq|kgSbeq|MJ|L\b/);
  });

  it("fmtRange outputs contain '–' and both endpoints — never a single averaged number — everywhere this file renders a range, StatuslinePreview excluded", () => {
    const join = fullJoin();
    const report = fullReport({ impact_join: join });
    const cards = selectImpactCards(report);
    const criteriaRows = selectCriteriaTable(report);

    const rangeBearingStrings = [
      cards.find((c) => c.key === "local")!.valueLabel,
      cards.find((c) => c.key === "remote")!.valueLabel,
      cards.find((c) => c.key === "combined")!.valueLabel,
      ...criteriaRows.map((r) => r.localLabel).filter((l) => l !== "not measured"),
      ...criteriaRows.map((r) => r.remoteLabel).filter((l) => l !== "not measured"),
      ...criteriaRows.map((r) => r.combinedLabel).filter((l) => !l.startsWith("not measured")),
    ];
    expect(rangeBearingStrings.length).toBeGreaterThan(0);
    for (const s of rangeBearingStrings) {
      expect(s).toContain("–");
      // Never collapsed to one number: splitting on the en dash must leave
      // two distinct numeric segments (both endpoints present, verbatim).
      const [minPart, maxPart] = s.split("–");
      expect(minPart.length).toBeGreaterThan(0);
      expect(maxPart.length).toBeGreaterThan(0);
    }

    // StatuslinePreview is the sanctioned exception — its values are means,
    // formatted with NO dash at all.
    const aside = selectImpactAside(report, null);
    expect(aside.statuslineLines[1]).not.toContain("–");
  });
});

// ---------------------------------------------------------------------------
// ImpactCards badge alarm styling — cascade specificity regression.
//
// Code review caught a real bug: ImpactCards.svelte's scoped
// `.impact-card__badge { color: var(--color-neutral-700); }` outranked the
// global `.status-alarm` utility (console.css) that `class:status-alarm`
// toggles on an alarm badge, because Svelte injects a component's own
// scoped <style> AFTER the app's global stylesheets — same class of cascade
// bug as the CriteriaTable split-bar hatch fix earlier in this task.
//
// True computed-style testing turned out to be genuinely awkward, not just
// in theory: this project's vitest setup does not process CSS at all in
// tests (Vitest's `test.css` defaults to `false`) — confirmed empirically
// mounting ImpactCards under `@vitest-environment happy-dom` and inspecting
// `document.styleSheets`/`querySelectorAll("style")`: BOTH are empty, for
// the imported global stylesheets AND for the component's own Svelte-scoped
// style. `getComputedStyle(...).color` can therefore only ever resolve to
// `""` here regardless of which rule "should" win — asserting on it would
// be a test that can never fail for the right reason. So this uses the two
// checks the review explicitly sanctioned as the fallback, together:
//   1. A mounted-component class-presence check (Svelte's own
//      `mount`/`unmount`/`flushSync`, matching timeline.test.ts's
//      hidden-tab-discipline technique) — proves the `class:status-alarm`
//      binding itself is wired correctly per badge.
//   2. A static-source regression guard on the ACTUAL rule the bug was —
//      no selector matching bare `.impact-card__badge` (unqualified by
//      `:not(.status-alarm)` or similar) may declare `color`, which is
//      exactly the property that silently overrode the alarm utility.
//      This is what would have caught the original bug and would catch a
//      reintroduction, since (1) alone provably cannot (the class was
//      already being applied correctly before the fix — only ITS VISUAL
//      EFFECT was cancelled by a rule that (1) never inspects).
// ---------------------------------------------------------------------------

describe("ImpactCards badge alarm styling — cascade specificity regression", () => {
  it("mounted: the alarm badge and the non-alarm badge each carry exactly the class:status-alarm binding predicts", async () => {
    const { mount, unmount, flushSync } = await import("svelte");
    const { default: ImpactCards } = await import("../src/lib/components/ImpactCards.svelte");

    const cards: ImpactCardModel[] = [
      {
        key: "combined",
        eyebrow: "combined · cross-paradigm",
        swatch: "combined",
        valueLabel: "1–2 kWh",
        measured: true,
        badges: [
          { label: "2 unmeasured remote spans", tone: "alarm" },
          { label: "gwp 1–2 kgCO2eq" }, // no tone — the non-alarm control
        ],
      },
    ];

    const target = document.createElement("div");
    document.body.appendChild(target);
    const instance = mount(ImpactCards, { target, props: { cards } });
    flushSync();

    const badgeEls = target.querySelectorAll<HTMLElement>(".impact-card__badge");
    expect(badgeEls.length).toBe(2);
    const alarmBadge = Array.from(badgeEls).find((el) => el.textContent?.includes("unmeasured"))!;
    const plainBadge = Array.from(badgeEls).find((el) => el.textContent?.includes("gwp"))!;

    expect(alarmBadge.classList.contains("status-alarm")).toBe(true);
    expect(plainBadge.classList.contains("status-alarm")).toBe(false);

    unmount(instance);
    document.body.removeChild(target);
  });

  it("static source: no bare `.impact-card__badge` rule (unqualified by :not(.status-alarm)) declares `color` — the exact rule that silently cancelled the alarm utility", () => {
    // `process.cwd()` is `console/` under vitest (same base every other
    // relative import in this test suite is written against) — avoids
    // `import.meta.url`, which vitest's own module loader does not always
    // give a `file:` scheme for.
    const svelteSource = readFileSync(join(process.cwd(), "src/lib/components/ImpactCards.svelte"), "utf-8");
    const styleBlock = svelteSource.match(/<style[^>]*>([\s\S]*?)<\/style>/)?.[1];
    expect(styleBlock).toBeTruthy();

    // Split into individual `selector { body }` rules (this stylesheet has
    // no nested rules/at-rules, so a naive split on top-level `}` is exact).
    const rules = styleBlock!
      .split("}")
      .map((chunk) => chunk.split("{"))
      .filter((parts): parts is [string, string] => parts.length === 2)
      .map(([selector, body]) => ({ selector: selector.trim(), body }));

    const bareBadgeRules = rules.filter((r) => r.selector.split(",").some((s) => s.trim() === ".impact-card__badge"));
    expect(bareBadgeRules.length).toBeGreaterThan(0); // sanity: the rule still exists at all
    for (const rule of bareBadgeRules) {
      expect(rule.body).not.toMatch(/(?<![-\w])color\s*:/);
    }

    // And the neutral tone must still be declared SOMEWHERE, scoped so it
    // never applies alongside `.status-alarm` — guards against the fix
    // being "delete the neutral tone entirely" rather than "scope it".
    const scopedNeutralRule = rules.find((r) => r.selector.includes(".impact-card__badge") && r.selector.includes(":not(.status-alarm)"));
    expect(scopedNeutralRule).toBeTruthy();
    expect(scopedNeutralRule!.body).toMatch(/color\s*:\s*var\(--color-neutral-700\)/);
  });
});

// ---------------------------------------------------------------------------
// Impact.svelte — mounted-component regressions for Phase 2 Package A's
// "unknown_model per-model rows" and "methodology placeholder" items. Same
// mount/unmount/flushSync + class-presence-check technique as the
// ImpactCards suite above (this file's own comment there explains why: no
// stylesheet is attached under happy-dom, so only class bindings — never
// resolved `color` — can be asserted). `reportStore`/`sessionStore` are
// singletons; each test resets both before AND after itself so no fixture
// leaks into the plain selector tests elsewhere in this file (none of which
// touch these stores) or between these two tests.
// ---------------------------------------------------------------------------

describe("Impact.svelte — per-model 'not measured' convention + methodology placeholder", () => {
  it("a mixed ok+unknown_model row shows the model's actually-measured criteria verbatim (plain, not magenta) and 'not measured'/neutral-600 for the rest — never bare '—'", async () => {
    const { mount, unmount, flushSync } = await import("svelte");
    const { default: Impact } = await import("../src/lib/tabs/Impact.svelte");
    const { reportStore } = await import("../src/lib/stores/reportStore.svelte");
    const { sessionStore } = await import("../src/lib/stores/sessionStore.svelte");

    reportStore.levels = {};
    sessionStore.sessions = {};
    sessionStore.summaries = {};
    sessionStore.pinnedId = null;

    const mixedImpacts: Impacts = { energy: criterion("kWh", 0.001, 0.002) }; // only energy measured for this model
    const report = fullReport({
      by_model: [modelGroup("mixed-model-7b", [okEstimate("e1", mixedImpacts), unknownEstimate("e2")], mixedImpacts)],
    });
    reportStore.set(report);

    const target = document.createElement("div");
    document.body.appendChild(target);
    const instance = mount(Impact, { target });
    flushSync();

    const rows = Array.from(target.querySelectorAll<HTMLElement>(".per-model-table__row"));
    const row = rows.find((r) => r.textContent?.includes("mixed-model-7b"));
    expect(row).toBeTruthy();

    // Magenta is scoped to the model-name cell (isUnknown is true — one of
    // this model's estimates is unknown_model), not the whole row.
    const modelCell = row!.querySelector(".col-model")!;
    expect(modelCell.classList.contains("status-alarm")).toBe(true);
    expect(modelCell.textContent).toContain("unknown_model");

    const criterionCells = Array.from(row!.querySelectorAll<HTMLElement>(".col-criterion"));
    expect(criterionCells.length).toBe(CRITERIA.length);

    const energyCell = criterionCells[CRITERIA.indexOf("energy")];
    expect(energyCell.textContent?.trim()).toBe(`${fmtRange({ min: 0.001, max: 0.002 })} kWh`);
    expect(energyCell.textContent?.trim()).not.toBe("—");
    expect(energyCell.classList.contains("status-neutral")).toBe(false);
    expect(energyCell.classList.contains("status-alarm")).toBe(false); // no magenta bleed onto a genuinely measured cell

    for (const key of CRITERIA.filter((k) => k !== "energy")) {
      const cell = criterionCells[CRITERIA.indexOf(key)];
      expect(cell.textContent?.trim()).toBe("not measured");
      expect(cell.textContent?.trim()).not.toBe("—");
      expect(cell.classList.contains("status-neutral")).toBe(true);
      expect(cell.classList.contains("status-alarm")).toBe(false);
    }

    unmount(instance);
    document.body.removeChild(target);
    reportStore.levels = {};
    sessionStore.sessions = {};
    sessionStore.summaries = {};
    sessionStore.pinnedId = null;
  });

  it("report present but session still null: the Methodology aside shows an explicit placeholder instead of disappearing", async () => {
    const { mount, unmount, flushSync } = await import("svelte");
    const { default: Impact } = await import("../src/lib/tabs/Impact.svelte");
    const { reportStore } = await import("../src/lib/stores/reportStore.svelte");
    const { sessionStore } = await import("../src/lib/stores/sessionStore.svelte");

    reportStore.levels = {};
    sessionStore.sessions = {};
    sessionStore.summaries = {};
    sessionStore.pinnedId = null;
    reportStore.set(fullReport());
    // Deliberately no sessionStore.set(...) — session stays null.

    const target = document.createElement("div");
    document.body.appendChild(target);
    const instance = mount(Impact, { target });
    flushSync();

    const headings = Array.from(target.querySelectorAll<HTMLElement>(".impact__aside .impact__section-heading"));
    expect(headings.some((h) => h.textContent === "Methodology")).toBe(true);
    expect(target.querySelector(".impact__aside")?.textContent).toContain("methodology — not yet available");
    expect(target.querySelector(".methodology")).toBeNull(); // the populated block must not also render

    unmount(instance);
    document.body.removeChild(target);
    reportStore.levels = {};
    sessionStore.sessions = {};
    sessionStore.summaries = {};
    sessionStore.pinnedId = null;
  });
});
