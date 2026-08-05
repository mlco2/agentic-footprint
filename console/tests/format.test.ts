// Unit tests for console/src/lib/format.ts — the single point allowed to
// touch numbers (global-constraints.md #1). Every function is pure, so these
// are plain input/output assertions; the interesting property to verify per
// function is the precision-tier boundary (sub-10, sub-1000, above), since
// that's exactly what SCREENS.md calls out as easy to get wrong ("rounding
// to '0 J' hides exactly the small-share rows the L1-vs-L2 comparison exists
// to expose").
import { describe, expect, it } from "vitest";
import { fmtBytes, fmtClock, fmtEventsPerS, fmtGridIntensity, fmtJoules, fmtMs, fmtMsCount, fmtOffset, fmtPct, fmtRange, fmtTokens, fmtWatts } from "../src/lib/format";

describe("fmtClock", () => {
  it("renders hh:mm:ss.SSS from local date components", () => {
    const ms = new Date(2026, 0, 1, 9, 40, 12, 4).getTime();
    expect(fmtClock(ms)).toBe("09:40:12.004");
  });

  it("zero-pads every field", () => {
    const ms = new Date(2026, 0, 1, 0, 0, 0, 0).getTime();
    expect(fmtClock(ms)).toBe("00:00:00.000");
  });

  it("never prints NaN for a non-finite input", () => {
    expect(fmtClock(Number.NaN)).not.toContain("NaN");
  });
});

describe("fmtJoules", () => {
  it("renders exactly 0 as '0 J'", () => {
    expect(fmtJoules(0)).toBe("0 J");
  });

  it("keeps 2 decimals below 10 J — sub-joule shares must stay visible", () => {
    expect(fmtJoules(1.9)).toBe("1.90 J");
    expect(fmtJoules(0.02)).toBe("0.02 J");
  });

  it("uses whole joules between 10 and 1000", () => {
    expect(fmtJoules(84.64)).toBe("85 J");
  });

  it("switches to kJ at 1000 and above, 2 decimals", () => {
    expect(fmtJoules(1234.5)).toBe("1.23 kJ");
  });
});

describe("fmtWatts", () => {
  it("mirrors the joules precision tiers", () => {
    expect(fmtWatts(0)).toBe("0 W");
    expect(fmtWatts(5.678)).toBe("5.68 W");
    expect(fmtWatts(45.2)).toBe("45.2 W");
    expect(fmtWatts(1500)).toBe("1.50 kW");
  });
});

describe("fmtMs", () => {
  it("renders sub-second durations as whole milliseconds", () => {
    expect(fmtMs(420)).toBe("420ms");
  });

  it("keeps 2 decimals of seconds below 10s", () => {
    expect(fmtMs(2100)).toBe("2.10s");
  });

  it("drops to 1 decimal of seconds from 10s to 60s", () => {
    expect(fmtMs(45_678)).toBe("45.7s");
  });

  it("renders minutes+seconds at and above 60s", () => {
    expect(fmtMs(125_000)).toBe("2m5s");
  });
});

describe("fmtMsCount", () => {
  it("renders a raw ms count with its literal unit, never converting to seconds", () => {
    expect(fmtMsCount(716)).toBe("716 ms");
    expect(fmtMsCount(32_000)).toBe("32,000 ms");
  });

  it("never prints NaN for a non-finite input", () => {
    expect(fmtMsCount(Number.NaN)).not.toContain("NaN");
  });
});

describe("fmtOffset", () => {
  it("uses a real minus sign (U+2212) for negative offsets", () => {
    expect(fmtOffset(-2100)).toBe("−2.1s");
  });

  it("uses a plus sign for non-negative offsets", () => {
    expect(fmtOffset(400)).toBe("+0.4s");
    expect(fmtOffset(0)).toBe("+0.0s");
  });
});

describe("fmtTokens", () => {
  it("renders sub-1000 counts verbatim", () => {
    expect(fmtTokens(340)).toBe("340");
  });

  it("renders 1000+ as one-decimal k", () => {
    expect(fmtTokens(1200)).toBe("1.2k");
    expect(fmtTokens(18_420)).toBe("18.4k");
  });
});

describe("fmtRange", () => {
  it("renders min–max with an en dash, never an average", () => {
    expect(fmtRange({ min: 1.9, max: 2 })).toBe("1.9–2");
  });

  it("keeps enough precision that a small criterion doesn't collapse to 0", () => {
    expect(fmtRange({ min: 0.0028, max: 0.0041 })).toBe("0.0028–0.0041");
  });

  it("renders a point value (min === max) as the same number twice", () => {
    expect(fmtRange({ min: 0, max: 0 })).toBe("0–0");
  });

  it("trims to sensible precision for larger values", () => {
    expect(fmtRange({ min: 82, max: 100 })).toBe("82–100");
  });
});

describe("fmtPct", () => {
  it("keeps 1 decimal below 10%", () => {
    expect(fmtPct(0.0224)).toBe("2.2%");
  });

  it("rounds to a whole percent at 10% and above", () => {
    expect(fmtPct(0.969)).toBe("97%");
  });

  it("renders 0 and 1 shares exactly", () => {
    expect(fmtPct(0)).toBe("0%");
    expect(fmtPct(1)).toBe("100%");
  });
});

describe("fmtBytes", () => {
  it("renders sub-1024 as whole bytes", () => {
    expect(fmtBytes(512)).toBe("512 B");
  });

  it("scales through KB/MB/GB at 1024", () => {
    expect(fmtBytes(1500)).toBe("1.46 KB");
    expect(fmtBytes(1024)).toBe("1.00 KB");
  });

  it("renders a large rss reading in GB", () => {
    expect(fmtBytes(1_932_735_283)).toBe("1.80 GB");
  });
});

// Real-server tolerance (docs/design-log.md, "af watch resident mode…"):
// both fields the real `af watch --debug` server sends as `null` render an
// honest, explained string — never a fabricated number, never "0".
describe("fmtEventsPerS", () => {
  it('renders "—" for the real server\'s null rate, never "0/s" or NaN', () => {
    expect(fmtEventsPerS(null)).toBe("—");
  });

  it("renders a real rate with the joules/watts precision tiers", () => {
    expect(fmtEventsPerS(0.14)).toBe("0.14/s");
    expect(fmtEventsPerS(45.2)).toBe("45.2/s");
  });
});

describe("fmtGridIntensity", () => {
  it('renders "n/a · {source}" for a null grid factor, honestly naming why, never "0 gCO2e/kWh"', () => {
    expect(fmtGridIntensity(null, "unavailable — no estimator sidecar or unknown zone")).toBe("n/a · unavailable — no estimator sidecar or unknown zone");
  });

  it("renders a real grid factor with a unit suffix", () => {
    expect(fmtGridIntensity(56, "codecarbon data v2026.06")).toBe("56 gCO2e/kWh");
  });

  it("never prints NaN for a non-finite grid factor", () => {
    expect(fmtGridIntensity(Number.NaN, "test")).not.toContain("NaN");
  });
});
