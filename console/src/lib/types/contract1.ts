// GENERATED FILE — do not hand-edit.
// Source: schemas/v0.1/events.schema.json
// Regenerate with `npm run gen:types` (console/scripts/gen-types.mjs).
// `npm run gen:types:check` fails if this file drifts from a fresh regeneration.

/**
 * Raw-facts events emitted by collectors to the local JSONL spool. Collectors MUST NOT include impact estimates; all estimation happens in the control plane.
 */
export interface EventEnvelopeBase {
  /**
   * Semver of this event standard.
   */
  schema_version: string;
  /**
   * Collector-generated unique id (ULID or UUID).
   */
  event_id: string;
  /**
   * Emission time, RFC 3339. Interval payloads carry their own t_start/t_end.
   */
  ts: string;
  collector: {
    /**
     * e.g. cc-hooks, codecarbon-sampler, litellm-middleware
     */
    name: string;
    version: string;
    [k: string]: unknown;
  };
  /**
   * The only mandatory correlation key. Agent-native session identifier when available.
   */
  session_id: string;
  /**
   * Optional deepening of correlation to task/tool level. Emit what the collector can observe; omit the rest.
   */
  attribution?: {
    /**
     * Logical agent within the session (e.g. main).
     */
    agent_id?: string;
    subagent_id?: string;
    task_id?: string;
    tool_call_id?: string;
  };
  [k: string]: unknown;
}

/**
 * One remote LLM inference request. Raw facts only. Token field names aligned with ACP session-usage RFD.
 */
export interface LlmCall {
  /**
   * e.g. anthropic, openai, mistralai, bedrock
   */
  provider: string;
  model_id_requested: string;
  /**
   * If the provider reports a different served model (routing/aliases).
   */
  model_id_served?: string;
  /**
   * Base URL or provider endpoint class, if known. No credentials, no paths with user data.
   */
  endpoint?: string;
  usage: {
    input_tokens?: number;
    output_tokens?: number;
    /**
     * Reasoning/thinking tokens when reported separately.
     */
    thought_tokens?: number;
    cached_read_tokens?: number;
    cached_write_tokens?: number;
  };
  /**
   * Provenance of the usage numbers, in decreasing reliability order (RFC Annex B). Collectors MUST prefer the highest available: in-band API usage, then agent-native telemetry (OTel export), then local transcripts (known accuracy issues), then estimates.
   */
  usage_source: "api_response" | "agent_telemetry" | "transcript" | "estimated";
  duration_ms?: number;
  status?: "ok" | "error" | "cancelled" | "unknown";
  streaming?: boolean;
  [k: string]: unknown;
}

/**
 * Locally measured (or hardware-modeled) energy over an interval. Machine-scoped; attribution to actions is a control-plane concern.
 */
export interface EnergySample {
  t_start: string;
  t_end: string;
  /**
   * @minItems 1
   */
  components: [
    {
      kind: "cpu" | "dram" | "gpu" | "total" | "other";
      /**
       * Device identity, e.g. 'NVIDIA RTX 4090 #0'.
       */
      label?: string;
      /**
       * Joules consumed over the interval.
       */
      energy_j: number;
      /**
       * Measured (rapl/powermetrics/nvml) vs modeled (tdp_model) MUST stay distinguishable.
       */
      method: "rapl" | "powermetrics" | "nvml" | "tdp_model" | "other";
      [k: string]: unknown;
    },
    ...{
      kind: "cpu" | "dram" | "gpu" | "total" | "other";
      /**
       * Device identity, e.g. 'NVIDIA RTX 4090 #0'.
       */
      label?: string;
      /**
       * Joules consumed over the interval.
       */
      energy_j: number;
      /**
       * Measured (rapl/powermetrics/nvml) vs modeled (tdp_model) MUST stay distinguishable.
       */
      method: "rapl" | "powermetrics" | "nvml" | "tdp_model" | "other";
      [k: string]: unknown;
    }[]
  ];
  /**
   * Stable opaque host identifier (hash), for multi-host merging later.
   */
  host_id?: string;
  [k: string]: unknown;
}

/**
 * One agent action (tool run, subagent, file operation). Overlapping spans are legal: concurrency is data, apportionment is control-plane policy.
 */
export interface ActionSpan {
  span_id: string;
  /**
   * e.g. Bash, Edit, mcp__server__tool
   */
  tool_name: string;
  tool_kind: "bash" | "mcp" | "file_op" | "subagent" | "web" | "other";
  /**
   * remote spans are excluded from the local energy join and reported as unmeasured remote activity.
   */
  execution_locus: "local" | "remote" | "hybrid" | "unknown";
  t_start: string;
  t_end: string;
  /**
   * Observed process ids of the action's process tree roots, when observable.
   */
  pids?: number[];
  /**
   * cgroup path when the action was wrapped (L3 attribution).
   */
  cgroup?: string;
  status?: "ok" | "error" | "cancelled" | "unknown";
  [k: string]: unknown;
}

/**
 * Per-process-tree resource deltas over an interval: the weighting signal for CPU-time (L2) attribution.
 */
export interface ProcessSample {
  t_start: string;
  t_end: string;
  processes: {
    /**
     * Root pid of a watched tree; deltas aggregate the tree.
     */
    pid: number;
    cpu_time_delta_ms: number;
    memory_rss_bytes?: number;
    io_read_bytes?: number;
    io_write_bytes?: number;
    [k: string]: unknown;
  }[];
  [k: string]: unknown;
}

/**
 * Session context. Geo zone is user-configured, never auto-geolocated. No hostnames, usernames or paths in clear.
 */
export interface SessionMeta {
  agent_app: {
    /**
     * e.g. claude-code, codex-cli
     */
    name: string;
    version?: string;
    [k: string]: unknown;
  };
  /**
   * e.g. darwin-25.3.0, linux-6.9
   */
  os?: string;
  /**
   * Used for TDP fallback modeling when counters are unavailable.
   */
  hardware?: {
    cpu_model?: string;
    gpu_models?: string[];
    ram_gb?: number;
    [k: string]: unknown;
  };
  /**
   * Electricity-mix zone code (user-configured), e.g. FRA, USA-CAL.
   */
  geo_zone?: string;
  power_source?: "ac" | "battery" | "unknown";
  [k: string]: unknown;
}

/**
 * One Contract #1 event, envelope plus its discriminated payload.
 * `type` narrows `payload` to the matching $defs shape.
 */
export type FactEvent =
  | (EventEnvelopeBase & { type: "llm_call"; payload: LlmCall })
  | (EventEnvelopeBase & { type: "energy_sample"; payload: EnergySample })
  | (EventEnvelopeBase & { type: "action_span"; payload: ActionSpan })
  | (EventEnvelopeBase & { type: "process_sample"; payload: ProcessSample })
  | (EventEnvelopeBase & { type: "session_meta"; payload: SessionMeta });
