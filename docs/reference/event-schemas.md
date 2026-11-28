# Event schema reference

Collector facts use Contract #1 schemas in
[`schemas/v0.1/`](https://github.com/mlco2/agentic-footprint/tree/main/schemas/v0.1):

- [`events.schema.json`](https://github.com/mlco2/agentic-footprint/blob/main/schemas/v0.1/events.schema.json)
  — Contract #1 raw-fact events emitted by collectors.
- [`derived.schema.json`](https://github.com/mlco2/agentic-footprint/blob/main/schemas/v0.1/derived.schema.json)
  — Contract #2 derived impact records (informative in `v0.1`).

Collectors preserve source facts and never emit impact estimates. Estimation,
correlation, and environmental impact calculation happen in the Rust control
plane after ingestion.

Schema changes must remain additive within a version. Incompatible changes
require a new schema version and migration guidance.

## Envelope and payload types

Every Contract #1 event carries a stable envelope (`schema_version`, `event_id`,
`type`, `ts`, `collector`, `session_id`, optional `attribution`, and `payload`).
The `type` field selects the payload shape:

| `type` | Required payload fields | Purpose |
| --- | --- | --- |
| `session_meta` | `agent_app` | Session context (agent name/version, OS, hardware, user-configured geo zone, power source). |
| `llm_call` | `provider`, `model_id_requested`, `usage`, `usage_source` | One remote LLM inference request with raw token counts and provenance. |
| `action_span` | `span_id`, `tool_name`, `tool_kind`, `execution_locus`, `t_start`, `t_end` | One agent action (tool run, subagent, file operation); overlapping spans are allowed. |
| `energy_sample` | `t_start`, `t_end`, `components` | Machine-scoped local energy interval (`rapl`, `powermetrics`, `nvml`, `tdp_model`, or `other`). |
| `process_sample` | `t_start`, `t_end`, `processes` | Per-process-tree resource deltas used as the weighting signal for CPU-time (L2) attribution. |

## Embedded schemas (`v0.1`)

The schemas below match the files in
[`schemas/v0.1/`](https://github.com/mlco2/agentic-footprint/tree/main/schemas/v0.1)
on GitHub.

??? example "events.schema.json (Contract #1)"

    ```json
    {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "$id": "https://agentic-footprint.dev/schemas/v0.1/events.schema.json",
      "title": "agentic-footprint collector event (Contract #1)",
      "description": "Raw-facts events emitted by collectors to the local JSONL spool. Collectors MUST NOT include impact estimates; all estimation happens in the control plane.",
      "type": "object",
      "required": ["schema_version", "event_id", "type", "ts", "collector", "session_id", "payload"],
      "properties": {
        "schema_version": {
          "type": "string",
          "description": "Semver of this event standard.",
          "pattern": "^0\\.1\\.[0-9]+$"
        },
        "event_id": {
          "type": "string",
          "description": "Collector-generated unique id (ULID or UUID).",
          "minLength": 16
        },
        "type": {
          "enum": ["llm_call", "energy_sample", "action_span", "process_sample", "session_meta"]
        },
        "ts": {
          "type": "string",
          "format": "date-time",
          "description": "Emission time, RFC 3339. Interval payloads carry their own t_start/t_end."
        },
        "collector": {
          "type": "object",
          "required": ["name", "version"],
          "properties": {
            "name": { "type": "string", "description": "e.g. cc-hooks, codecarbon-sampler, litellm-middleware" },
            "version": { "type": "string" }
          }
        },
        "session_id": {
          "type": "string",
          "description": "The only mandatory correlation key. Agent-native session identifier when available."
        },
        "attribution": {
          "type": "object",
          "description": "Optional deepening of correlation to task/tool level. Emit what the collector can observe; omit the rest.",
          "properties": {
            "agent_id": { "type": "string", "description": "Logical agent within the session (e.g. main)." },
            "subagent_id": { "type": "string" },
            "task_id": { "type": "string" },
            "tool_call_id": { "type": "string" }
          },
          "additionalProperties": false
        },
        "payload": { "type": "object" }
      },
      "allOf": [
        { "if": { "properties": { "type": { "const": "llm_call" } } }, "then": { "properties": { "payload": { "$ref": "#/$defs/llm_call" } } } },
        { "if": { "properties": { "type": { "const": "energy_sample" } } }, "then": { "properties": { "payload": { "$ref": "#/$defs/energy_sample" } } } },
        { "if": { "properties": { "type": { "const": "action_span" } } }, "then": { "properties": { "payload": { "$ref": "#/$defs/action_span" } } } },
        { "if": { "properties": { "type": { "const": "process_sample" } } }, "then": { "properties": { "payload": { "$ref": "#/$defs/process_sample" } } } },
        { "if": { "properties": { "type": { "const": "session_meta" } } }, "then": { "properties": { "payload": { "$ref": "#/$defs/session_meta" } } } }
      ],
      "$defs": {
        "llm_call": {
          "type": "object",
          "description": "One remote LLM inference request. Raw facts only. Token field names aligned with ACP session-usage RFD.",
          "required": ["provider", "model_id_requested", "usage", "usage_source"],
          "properties": {
            "provider": { "type": "string", "description": "e.g. anthropic, openai, mistralai, bedrock" },
            "model_id_requested": { "type": "string" },
            "model_id_served": { "type": "string", "description": "If the provider reports a different served model (routing/aliases)." },
            "endpoint": { "type": "string", "description": "Base URL or provider endpoint class, if known. No credentials, no paths with user data." },
            "usage": {
              "type": "object",
              "properties": {
                "input_tokens": { "type": "integer", "minimum": 0 },
                "output_tokens": { "type": "integer", "minimum": 0 },
                "thought_tokens": { "type": "integer", "minimum": 0, "description": "Reasoning/thinking tokens when reported separately." },
                "cached_read_tokens": { "type": "integer", "minimum": 0 },
                "cached_write_tokens": { "type": "integer", "minimum": 0 }
              },
              "additionalProperties": false
            },
            "usage_source": {
              "enum": ["api_response", "agent_telemetry", "transcript", "estimated"],
              "description": "Provenance of the usage numbers, in decreasing reliability order (RFC Annex B). Collectors MUST prefer the highest available: in-band API usage, then agent-native telemetry (OTel export), then local transcripts (known accuracy issues), then estimates."
            },
            "duration_ms": { "type": "integer", "minimum": 0 },
            "status": { "enum": ["ok", "error", "cancelled", "unknown"] },
            "streaming": { "type": "boolean" }
          }
        },
        "energy_sample": {
          "type": "object",
          "description": "Locally measured (or hardware-modeled) energy over an interval. Machine-scoped; attribution to actions is a control-plane concern.",
          "required": ["t_start", "t_end", "components"],
          "properties": {
            "t_start": { "type": "string", "format": "date-time" },
            "t_end": { "type": "string", "format": "date-time" },
            "components": {
              "type": "array",
              "minItems": 1,
              "items": {
                "type": "object",
                "required": ["kind", "energy_j", "method"],
                "properties": {
                  "kind": { "enum": ["cpu", "dram", "gpu", "total", "other"] },
                  "label": { "type": "string", "description": "Device identity, e.g. 'NVIDIA RTX 4090 #0'." },
                  "energy_j": { "type": "number", "minimum": 0, "description": "Joules consumed over the interval." },
                  "method": {
                    "enum": ["rapl", "powermetrics", "nvml", "tdp_model", "other"],
                    "description": "Measured (rapl/powermetrics/nvml) vs modeled (tdp_model) MUST stay distinguishable."
                  }
                }
              }
            },
            "host_id": { "type": "string", "description": "Stable opaque host identifier (hash), for multi-host merging later." }
          }
        },
        "action_span": {
          "type": "object",
          "description": "One agent action (tool run, subagent, file operation). Overlapping spans are legal: concurrency is data, apportionment is control-plane policy.",
          "required": ["span_id", "tool_name", "tool_kind", "execution_locus", "t_start", "t_end"],
          "properties": {
            "span_id": { "type": "string" },
            "tool_name": { "type": "string", "description": "e.g. Bash, Edit, mcp__server__tool" },
            "tool_kind": { "enum": ["bash", "mcp", "file_op", "subagent", "web", "other"] },
            "execution_locus": {
              "enum": ["local", "remote", "hybrid", "unknown"],
              "description": "remote spans are excluded from the local energy join and reported as unmeasured remote activity."
            },
            "t_start": { "type": "string", "format": "date-time" },
            "t_end": { "type": "string", "format": "date-time" },
            "pids": {
              "type": "array",
              "items": { "type": "integer" },
              "description": "Observed process ids of the action's process tree roots, when observable."
            },
            "cgroup": { "type": "string", "description": "cgroup path when the action was wrapped (L3 attribution)." },
            "status": { "enum": ["ok", "error", "cancelled", "unknown"] }
          }
        },
        "process_sample": {
          "type": "object",
          "description": "Per-process-tree resource deltas over an interval: the weighting signal for CPU-time (L2) attribution.",
          "required": ["t_start", "t_end", "processes"],
          "properties": {
            "t_start": { "type": "string", "format": "date-time" },
            "t_end": { "type": "string", "format": "date-time" },
            "processes": {
              "type": "array",
              "items": {
                "type": "object",
                "required": ["pid", "cpu_time_delta_ms"],
                "properties": {
                  "pid": { "type": "integer", "description": "Root pid of a watched tree; deltas aggregate the tree." },
                  "cpu_time_delta_ms": { "type": "integer", "minimum": 0 },
                  "memory_rss_bytes": { "type": "integer", "minimum": 0 },
                  "io_read_bytes": { "type": "integer", "minimum": 0 },
                  "io_write_bytes": { "type": "integer", "minimum": 0 }
                }
              }
            }
          }
        },
        "session_meta": {
          "type": "object",
          "description": "Session context. Geo zone is user-configured, never auto-geolocated. No hostnames, usernames or paths in clear.",
          "required": ["agent_app"],
          "properties": {
            "agent_app": {
              "type": "object",
              "required": ["name"],
              "properties": {
                "name": { "type": "string", "description": "e.g. claude-code, codex-cli" },
                "version": { "type": "string" }
              }
            },
            "os": { "type": "string", "description": "e.g. darwin-25.3.0, linux-6.9" },
            "hardware": {
              "type": "object",
              "properties": {
                "cpu_model": { "type": "string" },
                "gpu_models": { "type": "array", "items": { "type": "string" } },
                "ram_gb": { "type": "number", "minimum": 0 }
              },
              "description": "Used for TDP fallback modeling when counters are unavailable."
            },
            "geo_zone": { "type": "string", "description": "Electricity-mix zone code (user-configured), e.g. FRA, USA-CAL." },
            "power_source": { "enum": ["ac", "battery", "unknown"] }
          }
        }
      }
    }
    ```

??? example "derived.schema.json (Contract #2 — informative in v0.1)"

    ```json
    {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "$id": "https://agentic-footprint.dev/schemas/v0.1/derived.schema.json",
      "title": "agentic-footprint derived records (Contract #2 — informative in v0.1)",
      "description": "Control-plane outputs. This contract is informative in v0.1; it will be made normative in a later RFC aligned with GSF SCI / OTel GenAI semantic conventions.",
      "$defs": {
        "range": {
          "type": "object",
          "description": "Uncertainty range. min == max expresses a point value.",
          "required": ["min", "max"],
          "properties": {
            "min": { "type": "number" },
            "max": { "type": "number" }
          }
        },
        "criterion": {
          "type": "object",
          "description": "One impact criterion with usage/embodied life-cycle split when the methodology provides it.",
          "required": ["unit", "total"],
          "properties": {
            "unit": { "type": "string", "description": "e.g. kWh, kgCO2eq, kgSbeq, MJ, L" },
            "total": { "$ref": "#/$defs/range" },
            "usage": { "$ref": "#/$defs/range" },
            "embodied": { "$ref": "#/$defs/range" }
          }
        },
        "impacts": {
          "type": "object",
          "description": "Full EcoLogits criteria set.",
          "properties": {
            "energy": { "$ref": "#/$defs/criterion" },
            "gwp": { "$ref": "#/$defs/criterion" },
            "adpe": { "$ref": "#/$defs/criterion" },
            "pe": { "$ref": "#/$defs/criterion" },
            "water": { "$ref": "#/$defs/criterion" }
          }
        },
        "impact_estimate": {
          "type": "object",
          "description": "Estimated impacts of one llm_call event (remote inference).",
          "required": ["event_id", "estimation_status", "methodology"],
          "properties": {
            "event_id": { "type": "string", "description": "The llm_call event this estimate derives from." },
            "estimation_status": {
              "enum": ["ok", "unknown_model", "missing_zone", "missing_usage", "pending", "error"],
              "description": "Failure honesty: an unestimable call is surfaced, never skipped or zeroed. `missing_usage` is a call whose llm_call carried no token count — it never reaches the estimator, and reporting it as `error` or `pending` would attribute the gap to the estimator rather than to the collector that could not observe the usage."
            },
            "impacts": { "$ref": "#/$defs/impacts" },
            "methodology": {
              "type": "object",
              "required": ["version", "source"],
              "properties": {
                "version": { "type": "string", "description": "Version of the methodology data artifact used." },
                "source": { "enum": ["bundled", "local_dataset", "ecologits_api", "self_hosted_api"] },
                "ecologits_version": { "type": "string" }
              }
            }
          }
        },
        "impact_join": {
          "type": "object",
          "description": "Joined local-measured + remote-estimated impacts for one attribution unit over an interval. The combined total crosses measurement paradigms (measured + modeled) and MUST be presented as such.",
          "required": ["unit", "t_start", "t_end", "attribution_policy"],
          "properties": {
            "unit": {
              "type": "object",
              "required": ["level"],
              "properties": {
                "level": { "enum": ["session", "task", "tool_call"] },
                "session_id": { "type": "string" },
                "task_id": { "type": "string" },
                "tool_call_id": { "type": "string" }
              }
            },
            "t_start": { "type": "string", "format": "date-time" },
            "t_end": { "type": "string", "format": "date-time" },
            "attribution_policy": {
              "enum": ["l1_wall_clock", "l2_cpu_time", "l3_cgroup"],
              "description": "Policy used to apportion machine energy to this unit. Recorded so results are interpretable and re-computable."
            },
            "local_measured": {
              "type": "object",
              "properties": {
                "energy": { "$ref": "#/$defs/criterion" },
                "gwp": { "$ref": "#/$defs/criterion" },
                "baseline_share_excluded": {
                  "type": "boolean",
                  "description": "True when idle/baseline energy was separated out rather than attributed to actions."
                },
                "coverage": {
                  "type": "number", "minimum": 0, "maximum": 1,
                  "description": "Fraction of the unit's wall time covered by energy samples."
                }
              }
            },
            "remote_estimated": {
              "type": "object",
              "properties": {
                "impacts": { "$ref": "#/$defs/impacts" },
                "llm_calls": { "type": "integer", "minimum": 0 }
              }
            },
            "combined_total": {
              "type": "object",
              "description": "Sum of local measured + remote estimated per criterion, ranges preserved. Cross-paradigm: label accordingly in presentation.",
              "properties": {
                "energy": { "$ref": "#/$defs/criterion" },
                "gwp": { "$ref": "#/$defs/criterion" }
              }
            },
            "unmeasured_remote_spans": {
              "type": "integer", "minimum": 0,
              "description": "Count of execution_locus=remote action spans excluded from the local join and not (yet) estimable."
            }
          }
        }
      }
    }
    ```
