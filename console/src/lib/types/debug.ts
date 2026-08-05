// Hand-written contract types mirroring DATA-CONTRACT.md §2.1-2.7 field-for-
// field. Not generated: these describe the console's own `/debug/*` HTTP/SSE
// surface, which has no JSON Schema of its own (the schemas in schemas/v0.1/
// describe Contract #1/#2, the spool event and control-plane record shapes —
// reused here rather than re-declared, per DATA-CONTRACT §0: "the console
// computes nothing", including duplicating field lists it can import.
//
// This file is imported by console/src/lib client code AND by console/dev/
// (scenario.ts, mock-plugin.ts) — it must never import from console/dev/.
import type { ActionSpan, FactEvent } from "./contract1";
import type { ImpactEstimate, ImpactJoin, Impacts } from "./contract2";

/** DATA-CONTRACT §2.1 GET /debug/session — bootstrap.
 *
 * `?session_id=` addresses one session; without it the server answers with
 * the latest-active one (greatest `t_last`). */
export interface SessionInfo {
  session_id: string;
  /** Contract #1 session_meta payload, verbatim. */
  session_meta: Extract<FactEvent, { type: "session_meta" }>["payload"];
  t_start: string;
  /** Latest event ts the session's spool recorded — the ordering key for
   * "latest active". Absent on servers predating multi-session. */
  t_last?: string;
  /** Total events the session has produced. Absent on older servers. */
  events?: number;
  attribution_policy: AttributionPolicy;
  methodology: {
    /** Real-server gap (docs/design-log.md, "af watch resident mode…"):
     * before the first llm_call is estimated, the only methodology artifact
     * this build carries (the estimator sidecar's ecologits pin) isn't
     * known yet, so this reads literally as
     * `"unknown until the first estimate"` rather than a fabricated
     * version string. */
    version: string;
    source: string;
    /** Omitted entirely (not `""`) until an estimate has run — the real
     * server never guesses these. */
    ecologits_version?: string;
    codecarbon_version?: string;
  };
  grid: {
    zone: string;
    /** `null` without an estimator sidecar to resolve a zone's electricity
     * mix — a defaulted grid intensity is exactly the invented number the
     * project forbids (docs/design-log.md). `source` says which case this
     * is; render as "n/a · {source}", never as 0. */
    g_co2e_per_kwh: number | null;
    source: string;
  };
  state_dir: string;
  schema_version: string;
  mode: string;
}

/** Reuse of Contract #2's attribution policy union — never redeclared. */
export type AttributionPolicy = ImpactJoin["attribution_policy"];

/** One `action_span` fact, narrowed by discriminant. */
export type ActionSpanEvent = Extract<FactEvent, { type: "action_span" }>;

/** DATA-CONTRACT §2.2: "action_spans with no t_end yet." A currently-running
 * span reported by the control plane's live span table — deliberately NOT
 * `ActionSpanEvent`: `t_end` is omitted rather than guessed, because it
 * isn't known yet (never fabricate it as "now" or duplicate the eventual
 * close value early). */
export type OpenActionSpanEvent = Omit<ActionSpanEvent, "payload"> & {
  payload: Omit<ActionSpanEvent["payload"], "t_end">;
};

/** DATA-CONTRACT §2.3 `decision` frame. `kind` maps 1:1 onto the four
 * `docs/design-log.md` stderr prefixes: ingest=[ingest], span_open=[span
 * open], attr=[attr], orphan=[orphan]. Keep the two vocabularies aligned. */
export interface DecisionFrame {
  kind: "ingest" | "span_open" | "attr" | "orphan";
  ts: string;
  text: string;
  /** event_id or span_id the line is about, when there is one — makes the log clickable. */
  ref?: string;
}

/** DATA-CONTRACT §2.3 `reject` frame — a quarantined spool line. */
export interface RejectFrame {
  ts: string;
  reason: string;
  origin: string;
  line: number;
  byte_offset: number;
  raw: string;
}

/** DATA-CONTRACT §2.3 `gap` frame / §2.2 `coverage_gaps[]` entry. Must come
 * from the control plane — never inferred client-side from missing samples. */
export interface GapFrame {
  t_start: string;
  t_end: string;
  reason: string;
  collector: string;
}

/** DATA-CONTRACT §2.5 watchdog entry. */
export interface WatchdogEntry {
  pid: number;
  span_id: string;
  cmd: string;
  cpu_pct: number;
  rss_bytes: number;
  state: "open" | "orphaned" | "agent";
  orphaned_since?: string;
  outlived_span_by_ms?: number;
}

/** DATA-CONTRACT §2.3 `watchdog` SSE frame — the table specifies its `data:`
 * as `{ pids: [...] }`, an object wrapping the full-replacement array. This
 * differs from `Snapshot.watchdog` (§2.2, a bare `WatchdogEntry[]`) and from
 * `WatchdogEntry` itself (§2.5) — an asymmetry in the doc, not a typo; kept
 * as-is because §2.3's example is unambiguous about the wrapper. */
export interface WatchdogFrame {
  pids: WatchdogEntry[];
}

/** One component's measured/modeled energy within an allocation trace's interval. */
export interface AllocationComponent {
  kind: "cpu" | "dram" | "gpu" | "total" | "other";
  label?: string;
  energy_j: number;
  method: "rapl" | "powermetrics" | "nvml" | "tdp_model" | "other";
}

/** One span's row within an allocation trace. */
export interface AllocationRow {
  span_id: string;
  tool_name: string;
  execution_locus: ActionSpan["execution_locus"];
  overlap_ms: number;
  cpu_delta_ms: number;
  share: number;
  allocated_j: number;
  /** L1 (wall-clock share) shadow allocation — never rendered as the real allocation, gap #5. */
  l1_allocated_j: number;
  /** True for execution_locus: remote rows — they appear (so overlap is visible) but carry 0 joules. */
  excluded: boolean;
  excluded_reason: string | null;
}

/** DATA-CONTRACT §2.4 — the core payload: which span got which joules. */
export interface AllocationTrace {
  sample_event_id: string;
  /** Which session the apportioned sample belongs to. Absent on servers
   * predating multi-session. */
  session_id?: string;
  t_start: string;
  t_end: string;
  total_j: number;
  components: AllocationComponent[];
  attribution_policy: AttributionPolicy;
  /** Machine cpu-time over the interval — NOT the sum of watched trees. Dividing
   * by Σ watched would make attributed+agent ≡ 100%, destroying the explicit
   * baseline/idle remainder that is L2's whole point (DATA-CONTRACT §2.4). */
  denominator_cpu_ms: number;
  /** Present on the real server (crates/af-cli/src/cmd/debug_frames.rs):
   * explains, in prose, exactly what `denominator_cpu_ms` is and is not —
   * absent from the mock, which relies on this file's own doc comment
   * instead. Optional so both are valid. */
  denominator_note?: string;
  rows: AllocationRow[];
  agent_process: {
    pid: number;
    cpu_delta_ms: number;
    allocated_j: number;
    /** Present on the real server: explains that this bucket is the orphan
     * bucket (`l2_cpu_time/v1` has no separate agent-process bucket — see
     * this file's `AllocationTrace` doc comment and docs/design-log.md).
     * Render as the row's secondary line when present. */
    note?: string;
  };
  baseline: {
    allocated_j: number;
    share: number;
    label: string;
  };
  /** >1.0 means the L1 (wall-clock) shadow policy would over-attribute — the
   * clearest demonstration that L2 (cpu-time) apportionment is necessary. */
  l1_shadow_sum_share: number;
}

/** Contract #2's estimation_status values, reused rather than redeclared. */
export type EstimationStatus = ImpactEstimate["estimation_status"];

/** This pipeline's sixth status (docs/design-log.md, "af watch resident
 * mode…"): a `llm_call` with no token count to estimate from never reaches
 * ecologits and is recorded as `missing_usage` directly — outside Contract
 * #2's own five-value union, and folded into the report histogram only
 * when it actually occurs (see `DebugReport.estimation_status_histogram`). */
export type ReportEstimationStatus = EstimationStatus | "missing_usage";

/** One model's slice of the per-model impact-estimate table (§2.6). */
export interface ModelImpactGroup {
  model_id: string;
  estimates: ImpactEstimate[];
  /** Server-aggregated impacts across `estimates` for this model. */
  impacts: Impacts;
}

/** One row of GET /debug/sessions — the picker's list, latest-active
 * first. A summary, not a full SessionInfo: methodology/grid arrive with
 * the per-session `session` frame or `GET /debug/session?session_id=`. */
export interface SessionSummary {
  session_id: string;
  agent_app: { name: string; version?: string } | null;
  t_start: string;
  t_last: string;
  events: number;
}

/** DATA-CONTRACT §2.6 GET /debug/report?level=session|task|tool. */
export interface DebugReport {
  level: "session" | "task" | "tool";
  /** Which session this report is about. Absent on servers predating
   * multi-session — those only ever serve one session's report. */
  session_id?: string;
  impact_join: ImpactJoin;
  by_model: ModelImpactGroup[];
  /** Counts by estimation_status. The five Contract #2 statuses are always
   * zero-filled by the server, so an empty category reads as "zero
   * occurrences" rather than "not reported" — but `missing_usage` (see
   * `ReportEstimationStatus`) is added only when it occurs, hence
   * `Partial`: its absence must render as "0 occurrences" too, never as an
   * error or a blank. */
  estimation_status_histogram: Partial<Record<ReportEstimationStatus, number>>;
}

/** One collector row in the health payload's collector table. */
export interface CollectorHealth {
  name: string;
  /** Which session the row counts. Absent on servers predating
   * multi-session (rows were per collector×session there too, just
   * unlabeled — and last-writer-wins across sessions). */
  session_id?: string;
  version: string;
  transport: string;
  spool_file?: string;
  byte_offset?: number;
  events: number;
  /** `null` on the real server (docs/design-log.md: "a rate over a
   * session's whole span is not the rate anyone reads it as") — render
   * "—", never a fabricated rate. */
  events_per_s: number | null;
  rejected: number;
  last_seen: string;
  emits: string[];
}

/** One schema-conformance counter row (gap #9 — a design proposal, not a confirmed feature). */
export interface ConformanceRow {
  field: string;
  present: number;
  total: number;
  note?: string;
}

/** One `af python doctor` result row. */
export interface PythonDoctorRow {
  key: string;
  value: string;
  status: string;
}

/** DATA-CONTRACT §2.7 GET /debug/health.
 *
 * `conformance` is intentionally optional: its ABSENCE is meaningful (the
 * team declined gap #9's counters), and must never be rendered as an empty
 * table — that would misreport "counted zero" instead of "not counted". */
export interface HealthPayload {
  collectors: CollectorHealth[];
  otlp_receiver: {
    /** e.g. "127.0.0.1:4318" — http/json only; 4317 is the gRPC port and is
     * wrong here. `null` when no OTLP receiver is running in this `af watch`
     * process at all (e.g. `--no-otlp`, or the bind failed and was reported
     * rather than fatal — docs/design-log.md) — an e2e-verified real-server
     * case beyond DATA-CONTRACT's own example, not just this task's
     * `--no-otlp` test invocation: `--otlp-addr`/`--no-otlp` are a real,
     * user-facing choice. `note` explains which case it is. */
    endpoint: string | null;
    protocol: string;
    logs_accepted: number;
    metrics_discarded: number;
    note?: string;
  };
  conformance?: ConformanceRow[];
  rejected: RejectFrame[];
  /** Real-server-only counters (crates/af-cli/src/cmd/watch.rs's `publish("health", ...)`
   * payload, verified read-only — this mock never sends them, hence
   * optional): every one of these is a running, all-time count, never
   * decremented. */
  /** Every record this process has lost, spool-quarantined + OTLP-dropped
   * combined — the one number that answers "how much am I missing", since
   * `rejected[]` alone only shows the spool's own (windowed) share of it. */
  rejected_total?: number;
  /** `rejected_total`'s spool-quarantine component alone (same count `rejected[]`'s own entries are drawn from, but all-time rather than windowed). */
  rejected_spool?: number;
  /** `rejected_total`'s OTLP-receiver component: bodies the receiver itself dropped/quarantined before they ever reached the spool. */
  rejected_otlp?: number;
  /** Records the process gave up on outright (neither quarantined to `rejected/` nor forwarded) — a third, distinct loss bucket from the two above. */
  rejected_dropped?: number;
  python: PythonDoctorRow[];
}

/** DATA-CONTRACT §2.2 GET /debug/snapshot?window=Ns — backfill. */
export interface Snapshot {
  events: FactEvent[];
  allocations: AllocationTrace[];
  coverage_gaps: GapFrame[];
  open_spans: OpenActionSpanEvent[];
  watchdog: WatchdogEntry[];
  /** Sequence number of the last frame played into this snapshot — the
   * client uses it verbatim as `Last-Event-ID` when it subscribes to /debug/stream. */
  as_of_seq: number;
}

/** Named SSE event kinds streamed by GET /debug/stream (DATA-CONTRACT §2.3 table),
 * plus `reset`: a protocol-level frame (not part of the §2.3 data table) the
 * server sends when a client's Last-Event-ID is older than the replay buffer. */
export type SseEventName =
  | "fact"
  | "decision"
  | "alloc"
  | "reject"
  | "gap"
  | "watchdog"
  | "report"
  | "health"
  | "session"
  | "reset";

/** Empty payload for the `reset` frame — a signal, not data. */
export type ResetFrame = Record<string, never>;

/** Maps each SSE event name to its `data:` payload type (§2.3 table). */
export interface SseFrameDataMap {
  fact: FactEvent;
  decision: DecisionFrame;
  alloc: AllocationTrace;
  reject: RejectFrame;
  gap: GapFrame;
  /** Full replacement of the watchdog list, ~1 Hz. Wrapped per §2.3's table
   * (`{ pids: [...] }`) — NOT the same shape as `Snapshot.watchdog`, which is
   * a bare `WatchdogEntry[]` per §2.2. See WatchdogFrame's own doc comment. */
  watchdog: WatchdogFrame;
  report: DebugReport;
  health: HealthPayload;
  /** Per-session replace-on-arrival, one frame per session per pass — the
   * picker follows sessions live through these. */
  session: SessionInfo;
  reset: ResetFrame;
}

/** One frame in the scenario/replay log or over the wire: `{atMs, event, data}`
 * with `data` narrowed to match `event` via SseFrameDataMap. */
export type SseFrame = {
  [K in SseEventName]: { atMs: number; event: K; data: SseFrameDataMap[K] };
}[SseEventName];
