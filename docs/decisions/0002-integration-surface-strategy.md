# RFC 0002 — Integration surface strategy: four positions, one pairing principle

!!! note "Historical decision record"
    This document records integration strategy during development. The first
    release supports Claude Code and Codex; experimental integrations remain
    outside the default binary.

- **Status:** Draft — deliberately provisional. This note exists to be
  challenged: the first scheduled challenge is the iteration that discusses,
  tests, and amends the OTel-first-class position against real enterprise
  tooling. Amend it with evidence, not opinions.
- **Date:** 2026-07-26
- **Audience:** internal
- **Prior art:** RFC 0001 (architecture & contracts);
  `docs/archive/2026-07-26/research-coding-agent-protocol-affinity-2026-07-26.md`
  (per-tool affinity labels A–E and the 2026-07-26 addendum §7.5).

## 1. Purpose

The affinity research graded tools A–E by how well their surfaces map to
Contract #1. That grading is descriptive: it tells us what a tool offers
after we research it. This RFC adds the predictive layer: *why* tools offer
what they offer, what that predicts about integration cost, reliability, and
maintenance, and how it should drive prioritization.

The origin observation (2026-07-26): tools cluster by organizational
incentive, not by technical accident.

## 2. The four positions

**P1 — Enterprise-OTel natives.** Tools backed by large engineering
organizations selling to enterprises, where observability is a checkbox on
procurement lists: native OTLP export exists, is documented, and is
versioned. Examples: Claude Code, Codex CLI. Integration mode: consume the
export with our existing receiver; the tax is configuration delivery (env
blocks, config tables, per-home quirks), not collection code.

**P2 — Extensible platforms.** Tools whose architecture is a platform:
plugin APIs, lifecycle hooks, typed event buses. Telemetry may be absent or
immature, but the tool *invites* in-process extension, so monitoring can be
added without fighting the framework. Examples: OpenCode (plugin `event`
hook), Pi (in-process extension API); generalist agent frameworks
(LangChain-style) largely belong here too — most already ship OTel GenAI
semconv or callback systems, so they are P2 with better standards
discipline, not P4. Integration mode: a dumb in-process forwarder handing
raw events to `af`, normalization staying in Rust.

**P3 — Middleware chokepoints.** Not agents: proxies and gateways that see
every provider call across all agents routed through them. Examples:
LiteLLM, OpenRouter, AI gateways. Maximal usage coverage, amortized across
every tool behind them; zero tool/session/process context. Long-term,
enterprise-leaning. Integration mode: one collector per gateway, usage-only
by construction.

**P4 — Closed products.** Consumer tools whose only surfaces are the
product UI and private state on disk (transcripts, sqlite). Examples:
Cursor, Copilot CLI. Integration mode, if ever: replay/cross-check only,
never a primary live collector — per the affinity doc's standing principle
that a collector working only because a vendor happens to serialize private
state in a particular shape is a fallback, not an integration.

## 3. Principles the positions do not change

**Positions are per-surface, not per-tool.** OpenCode alone spans P1
(experimental native OTel), P2 (plugin hook, typed SSE), and P4 (private
sqlite). Claude Code is P1 *and* P2 (hooks). Grade surfaces, not logos.

**The pairing principle.** A complete integration pairs one
usage-authoritative surface with one lifecycle/process surface — they
usually come from different positions of the same tool. Claude Code is the
proof: OTLP for usage facts, hooks for spans and PIDs, correlated by
session. Codex currently lacks the second half (no child PIDs) and that is
a recorded gap, not a solved problem.

**Enterprise-shaped telemetry is not attribution-shaped.** P1 telemetry
answers "what did my org spend": aggregated usage, batched export, no
process identity. Local energy attribution needs PIDs, timestamps, and tool
spans that P1 exports rarely carry. Expect every P1 integration to need a
P2-style supplement for attribution, and treat its absence as an honest,
labeled gap.

**Provenance belongs in the data.** Contract #1 already carries
`usage_source` (`agent_telemetry` today). Every position lands differently:
P1 facts are vendor-exported, P2 facts are forwarded in-process, P3 facts
are middleware-observed, P4 facts are scraped at rest. Reports must be able
to distinguish these without reading design docs. Extend the `usage_source`
vocabulary as positions ship; never let a scraped fact masquerade as a
measured one.

**P2's tax is installation, not collection.** The forwarder is small; the
product work is making it *magically present* — `af setup` writing configs
and dropping plugins into the homes tools actually read (the Codex per-home
`CODEX_HOME` episode of 2026-07-26 is the canonical example). Budget P2
work accordingly: collection days, setup-UX weeks.

**Maintenance profiles differ by position.** P1 breaks rarely but opaquely
(the vendor ships, we diagnose from the outside). P2 breaks at plugin-API
majors — pin versions and keep sanitized fixtures, as the repo already
does. P4 breaks silently on any release. P3 sits with P1. Prioritize
accordingly, and never promise P4-grade sources the same SLA as P1/P2.

## 4. Current state against the map

| Tool | Position(s) used | Status |
|---|---|---|
| Claude Code | P1 (OTLP) + P2 (hooks) | Shipped, reference pairing |
| Codex CLI | P1 (native OTel) | Shipped; pairing half missing (no PIDs) |
| OpenCode | P2 (SSE, server mode) shipped; P2 (plugin hook, default TUI) designed; P1 (native OTel) blocked on upstream (#25839, #33101, #30087); ACP proxy = future editor persona | In progress |
| Pi | P2 (in-process extension) | Deferred pending adoption |
| LiteLLM / gateways | P3 | Not started; long-term |
| Cursor, Copilot CLI, … | P4 | Explicitly not planned as live collectors |

## 5. Expected amendments

1. **The P1 challenge (first iteration):** testing enterprise-OTel tools
   beyond Claude Code/Codex will stress the claim that P1 usage facts are
   consumable as-is (schema drift across vendors, GenAI semconv dialects,
   auth/export quirks). Amend §2/§3 with what actually breaks.
2. **Generalist frameworks:** the P2 classification of agent frameworks is
   an educated guess; the first framework integration must confirm whether
   callback surfaces carry enough lifecycle to honor the pairing principle.
3. **`usage_source` vocabulary:** to be specified in the events contract
   when the second position ships a collector.
4. **P3 scope:** whether gateway-observed usage should join per-session
   reports at all, or only fleet-level reporting, is an open product
   question.
