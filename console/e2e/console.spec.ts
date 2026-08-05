// Playwright e2e smoke suite (fix-wave finding #1) — runs against
// `vite preview` + the deterministic, seeded mock scenario
// (console/dev/scenario.ts, served by console/dev/mock-plugin.ts). See
// console/playwright.config.ts's header for why `webServer` doesn't build.
//
// The mock's virtual clock is anchored to the wall-clock moment the preview
// server process started (mock-plugin.ts's `SERVER_START_WALL_MS`), and a
// handful of the scenario's more interesting facts (the coverage gap at
// scenario-relative 24s, the first span to close at 30s) only become visible
// once that much real wall time has elapsed since the server booted — the
// mock has no fast-forward. Tests that need those are ordered LAST in this
// serial suite specifically so the earlier tests' own setup/assertion time
// counts toward that wait instead of adding a fresh one on top; they still
// carry generous explicit timeouts rather than relying on that ordering for
// correctness.
//
// Health tab data (collectors/conformance/quarantined lines, `GET
// /debug/health`) and the session/report fetches are NOT time-gated — the
// mock serves them synchronously regardless of elapsed wall time — so those
// assertions stay fast.
import { test, expect, type Page } from "@playwright/test";

const TABS = ["timeline", "stream", "attribution", "impact", "health"] as const;

function tabButton(page: Page, tab: string) {
  return page.locator(".tabbar .tab").filter({ hasText: new RegExp(`^${tab}$`, "i") });
}

async function gotoTab(page: Page, tab: string): Promise<void> {
  await tabButton(page, tab).click();
  await expect(tabButton(page, tab)).toHaveClass(/is-active/);
}

test.describe.configure({ mode: "serial" });

test.describe("af debug console", () => {
  let page: Page;
  const consoleErrors: string[] = [];

  test.beforeAll(async ({ browser }) => {
    page = await browser.newPage();
    page.on("pageerror", (err) => consoleErrors.push(String(err)));
    page.on("console", (msg) => {
      if (msg.type() === "error") consoleErrors.push(msg.text());
    });
    await page.goto("/");
  });

  test.afterAll(async () => {
    await page.close();
  });

  // (a) All five tabs render their masthead/tab-bar and switch without
  // console errors; the Inspector's "no record selected" empty state (which
  // depends only on `uiStore.selectedId === null`, not on any data having
  // arrived yet) is visible pre-click on both Layout-A tabs.
  test("all five tabs render and switch without console errors", async () => {
    await expect(page.locator(".masthead__title")).toHaveText("af · debug console");
    await expect(page.locator(".tabbar .tab")).toHaveCount(TABS.length);

    for (const tab of TABS) {
      await gotoTab(page, tab);
    }

    // Nothing has been clicked yet anywhere in the app — Timeline's and
    // Stream's Inspector panes are both honestly empty.
    await gotoTab(page, "timeline");
    await expect(page.locator(".timeline__inspector .empty-state__eyebrow")).toHaveText("Inspector");
    await gotoTab(page, "stream");
    await expect(page.locator(".stream__inspector .empty-state__eyebrow")).toHaveText("Inspector");

    expect(consoleErrors, `console errors during tab switching: ${consoleErrors.join("; ")}`).toEqual([]);
  });

  // (b) Connection reaches "live" and Stream shows rows. `session_meta` is
  // the scenario's very first fact (scenario-relative 0s), so this needs no
  // real wait beyond the initial bootstrap round-trip.
  test("connection reaches live and Stream shows rows", async () => {
    await expect(page.locator(".masthead__dot--live")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".masthead__meta").filter({ hasText: "SSE live" })).toBeVisible();

    await gotoTab(page, "stream");
    await expect(page.locator(".event-table__row").first()).toBeVisible({ timeout: 15_000 });
  });

  // (f) Pause freezes the clock and buffers frames instead of dropping the
  // SSE connection; resume goes live again. Energy/process-sample bursts
  // land roughly every 2 scenario-seconds, so a few real seconds paused is
  // enough to see the buffered count tick up from 0.
  test("pause buffers frames, resume goes live again", async () => {
    const liveToggle = page.locator(".masthead__live-toggle");
    await expect(liveToggle).toHaveText("live");

    await liveToggle.click();
    await expect(liveToggle).toHaveText(/SSE paused · buffering \(\d+\)/);
    await expect(liveToggle).toHaveText(/SSE paused · buffering \([1-9]\d*\)/, { timeout: 10_000 });

    await liveToggle.click();
    await expect(liveToggle).toHaveText("live");
    await expect(page.locator(".masthead__dot--live")).toBeVisible();
  });

  // (e) Health tab (closes Task 4b's deferred items): the collector table,
  // conformance area and quarantined-line panel all come from `GET
  // /debug/health`, served synchronously and in full regardless of elapsed
  // wall time — fast, no scenario-clock wait needed.
  test("Health tab: collectors, pending-decision conformance, quarantined entry with byte offset", async () => {
    await gotoTab(page, "health");

    // Fixture has exactly 3 collectors (claude-code, codecarbon-sampler, otlp-cc).
    await expect(page.locator(".collector-table__row")).toHaveCount(3);

    // gap #9 is deferred by decision — the mock's health payload carries no
    // `conformance` key, so this is the honest "pending" empty state, not a
    // fallback.
    await expect(page.locator(".conformance .empty-state__message")).toHaveText("conformance counters: pending team decision");

    // The fixture's one quarantined line, with its byte offset rendered
    // (never silently dropped).
    const quarantined = page.locator(".rejected-row");
    await expect(quarantined).toHaveCount(1);
    await expect(quarantined.locator(".rejected-row__origin")).toContainText(/byte [\d,]+/);
  });

  // (d) Timeline shows the scenario's coverage-gap band — a server `gap`
  // frame, never client-inferred (global-constraints.md #6) — rendered with
  // the same magenta-alarm hatch as every other alarm element
  // (global-constraints.md #3). The gap closes at scenario-relative 24s, so
  // this is the suite's first real wait; placed here (after the fast tests
  // above) so their own runtime already counts toward it.
  test("Timeline shows the coverage-gap band as an alarm element", async () => {
    test.setTimeout(90_000);
    await gotoTab(page, "timeline");

    const gapBar = page.locator('.timeline__chart [title*="NO COVERAGE"]');
    await expect(gapBar).toHaveCount(1, { timeout: 75_000 });
    await expect(gapBar).toHaveClass(/hatch-alarm/);

    // Same alarm styling is what the rest of "an orphan/alarm element"
    // assertion is about (global-constraints.md #3: magenta = alarm only,
    // orphans AND coverage gaps share it) — confirm the chart actually
    // paints at least one.
    await expect(page.locator(".timeline__chart .hatch-alarm").first()).toBeVisible();
  });

  // (c) Click-path convergence (closes Task 6's deferred ruling): selecting
  // the long-running `spn_0001` span by its `span_id` (a Timeline bar click)
  // and selecting the SAME span by its closing fact's `event_id` (a Stream
  // row click) must converge on an identical Inspector model. `spn_0001`
  // closes at scenario-relative 32s — by far this suite's longest wait, so
  // it runs last to make the most of time already spent above.
  test("Timeline span selection converges with the same event's Stream row", async () => {
    test.setTimeout(90_000);
    await gotoTab(page, "timeline");

    const bar = page.locator('.timeline__chart [title*="spn_0001"]');
    await expect(bar).toHaveCount(1, { timeout: 75_000 });
    await bar.click();

    // The bar is clickable (and selectable) the whole time spn_0001 is
    // running, but the Inspector model for a still-OPEN span (sub ends
    // "· open") is a genuinely different shape from the one for its closed
    // fact (sub ends "· <duration>") — both correct, but not the pair this
    // test is about. Wait for the reactive re-render once the closing fact
    // lands (scenario-relative 32s) before capturing what "converges" means.
    await expect(page.locator(".inspector__sub")).not.toHaveText(/· open$/, { timeout: 75_000 });

    const title1 = await page.locator(".inspector__title").innerText();
    const sub1 = await page.locator(".inspector__sub").innerText();
    expect(sub1).toContain("spn_0001");

    await gotoTab(page, "stream");

    // `uiStore.selectedId` is shared across tabs, so switching tabs alone
    // would already show the same selection — select something else first
    // so the row click below is a genuine, exercised selection change.
    // Stream rows sort newest-first (selectors/stream.ts), and spn_0001's
    // closing fact is the newest thing in the scenario at this point, so
    // `.first()` would just re-click it — `.last()` (session_meta, always
    // ts=0, the scenario's very first fact) is guaranteed different.
    await page.locator(".event-table__row").last().click();
    await expect(page.locator(".inspector__sub")).not.toHaveText(sub1);

    // spn_0001 doesn't close (and so doesn't get a Stream row — Stream only
    // ever shows facts, never still-open spans) until scenario-relative
    // 32s; the earlier tests' own runtime already ate into that wait, but
    // this is still the suite's longest remaining one.
    const row = page.locator(".event-table__row").filter({ hasText: "cargo test" });
    await expect(row).toBeVisible({ timeout: 60_000 });
    await row.click();

    const title2 = await page.locator(".inspector__title").innerText();
    const sub2 = await page.locator(".inspector__sub").innerText();
    expect(title2).toBe(title1);
    expect(sub2).toBe(sub1);
  });
});
