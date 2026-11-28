# Agentic Footprint

Agentic Footprint measures coding-agent workloads and reports their local and
remote environmental impacts without hiding uncertainty or missing data.

It combines:

- native agent telemetry and lightweight hooks;
- one shared CodeCarbon machine sampler;
- per-session process observation with `psutil`;
- CPU-weighted attribution to action spans and tool calls;
- EcoLogits estimates for remote model inference;
- a local Rust control plane, report CLI, and debug console.

## Start here

- Follow the [quickstart](tutorials/quickstart.md) for a first measured session.
- Use the [installation guide](how-to/installation.md) for platform-specific
  receiver and setup options.
- Choose the [Claude Code](how-to/claude-code.md) or
  [Codex](how-to/codex.md) integration guide.
- Read [energy attribution](explanation/energy-attribution.md) to understand how
  machine joules become per-action results.
- To read afoot's results from another program, see
  [Use afoot from another program](how-to/integrate.md).
- Check the [known limitations](reference/known-limitations.md) before
  relying on a figure.

## Design principles

**Raw facts remain raw.** Collectors do not estimate environmental impact.

**Measured and modeled values stay distinguishable.** Missing local energy or
remote estimates remain explicit gaps rather than invented zeroes.

**Machine energy is conserved.** Session and action attribution divide each
measurement window without duplicating its joules.

**Collection must not break the agent.** Hooks degrade safely while control
plane failures remain visible to the operator.

## Project status

Agentic Footprint is alpha software and no release has been published yet;
it is installed from a source checkout. It supports Claude Code, Codex, OpenCode
and Pi.
