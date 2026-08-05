//! Drives the `python/af_estimator` sidecar over one un-estimated
//! `llm_call` backlog: [`estimate_pending`] pulls
//! [`af_store::Store::llm_calls_without_estimate`], sends one `estimate`
//! request per event, and upserts the sidecar's response verbatim —
//! including failure statuses (`unknown_model`/`missing_zone`/`error`), per
//! the project's failure-honesty rule: an unestimable call is recorded as
//! such, not skipped or silently retried forever.

use af_events::{Envelope, Payload};
use af_sidecar::Sidecar;
use af_store::{ImpactEstimate, Store};
use anyhow::{Context, Result};
use serde_json::{json, Value};

/// Policy for the electricity region used by remote inference estimation.
/// This is intentionally independent from the local machine's grid zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstimationRegion {
    pub id: Option<String>,
    pub source: String,
}

impl EstimationRegion {
    pub fn automatic() -> Self {
        Self {
            id: None,
            source: "estimator_auto".to_string(),
        }
    }

    pub fn explicit(id: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            source: source.into(),
        }
    }

    fn to_json(&self) -> Value {
        match &self.id {
            Some(id) => json!({"id": id, "source": self.source}),
            None => json!({"source": self.source}),
        }
    }
}

/// Tally of one [`estimate_pending`] pass. All five counters are disjoint
/// and sum to the number of `llm_call` events that were pending at the
/// start of the pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EstimationOutcome {
    /// Sidecar returned `status: "ok"` — a real impact estimate was stored.
    pub estimated: usize,
    /// Sidecar returned `status: "unknown_model"`.
    pub unknown_model: usize,
    /// Sidecar returned `status: "missing_zone"`.
    pub missing_zone: usize,
    /// `usage.output_tokens` was `None` — the sidecar was never called (it
    /// has no token count to estimate from); a `{"status":"missing_usage"}`
    /// row was upserted instead.
    pub missing_usage: usize,
    /// Any other status (including `"error"` or a malformed/missing
    /// `status` field) — still upserted, just counted here instead.
    pub errors: usize,
}

/// Sends one `estimate` request per un-estimated `llm_call` event in
/// `store` to `sidecar`, optionally overriding the estimator's remote
/// electricity region, and
/// upserts every response (success or failure status alike) so the event
/// is never re-sent on a later pass. An event with `usage.output_tokens ==
/// None` is never sent to the sidecar at all — there is no token count to
/// estimate from — and instead gets a `{"status":"missing_usage"}` row
/// upserted directly (see [`EstimationOutcome::missing_usage`]).
///
/// `methodology_version` on the upserted row is
/// `format!("ecologits-{version}")` where `version` is
/// `response.methodology.ecologits_version` when present, or `"unknown"`
/// for responses that don't carry a `methodology` object (the
/// `unknown_model`/`missing_zone` statuses never do); a `missing_usage` row
/// gets `"none"` instead, since it never reached the sidecar.
///
/// Every upserted row is stamped with remote-region provenance so an
/// explicit later override can detect estimates computed under another one.
pub fn estimate_pending(
    store: &mut Store,
    sidecar: &mut Sidecar,
    region: &EstimationRegion,
) -> Result<EstimationOutcome> {
    estimate_pending_limit(store, sidecar, region, None)
}

/// Resident-worker variant of [`estimate_pending`]. The database remains the
/// durable queue; `limit` only bounds how long one wake can monopolize the
/// estimator thread.
pub fn estimate_pending_limit(
    store: &mut Store,
    sidecar: &mut Sidecar,
    region: &EstimationRegion,
    limit: Option<usize>,
) -> Result<EstimationOutcome> {
    let events = store.llm_calls_without_estimate_limit(limit)?;
    estimate_events(store, sidecar, region, &events)
}

/// Estimates the supplied pending events in order. The caller may query and
/// retain their session ids before processing, which is how the resident
/// worker reports exactly which joins became dirty without copying envelopes
/// through a channel.
pub fn estimate_events(
    store: &mut Store,
    sidecar: &mut Sidecar,
    region: &EstimationRegion,
    events: &[Envelope],
) -> Result<EstimationOutcome> {
    let mut outcome = EstimationOutcome::default();

    for envelope in events {
        let Payload::LlmCall(call) = &envelope.payload else {
            // `llm_calls_without_estimate` already filters to type =
            // 'llm_call' at the SQL level; a mismatch here would mean
            // on-disk corruption, not a normal condition worth failing
            // the whole batch over. Skip defensively.
            continue;
        };

        let Some(output_tokens) = call.usage.output_tokens else {
            // No token count means the sidecar has nothing to estimate
            // from — calling it with a fabricated `0` would silently
            // manufacture a plausible-looking zero-impact "ok" estimate.
            // Record the gap honestly instead.
            outcome.missing_usage += 1;
            store.upsert_estimate(&ImpactEstimate {
                event_id: envelope.event_id.clone(),
                methodology_version: "none".to_string(),
                json: stamp_region(json!({"status": "missing_usage"}), region),
            })?;
            continue;
        };

        let mut request = estimate_request(
            &call.provider,
            &call.model_id_requested,
            output_tokens,
            region,
        );
        if let Some(duration_ms) = call.duration_ms {
            request["request_latency"] = json!(duration_ms as f64 / 1000.0);
        }

        let response = sidecar
            .request(&request)
            .with_context(|| format!("estimate request for event {}", envelope.event_id))?;

        match response.get("status").and_then(Value::as_str) {
            Some("ok") => outcome.estimated += 1,
            Some("unknown_model") => outcome.unknown_model += 1,
            Some("missing_zone") => outcome.missing_zone += 1,
            _ => outcome.errors += 1,
        }

        store.upsert_estimate(&ImpactEstimate {
            event_id: envelope.event_id.clone(),
            methodology_version: methodology_version(&response),
            json: stamp_region(response, region),
        })?;
    }

    Ok(outcome)
}

fn estimate_request(
    provider: &str,
    model_name: &str,
    output_tokens: u64,
    region: &EstimationRegion,
) -> Value {
    let mut request = json!({
        "op": "estimate",
        "provider": provider,
        "model_name": model_name,
        "output_token_count": output_tokens,
    });
    if let Some(region_id) = &region.id {
        request["electricity_mix_zone"] = json!(region_id);
    }
    request
}

impl EstimationOutcome {
    pub fn processed(self) -> usize {
        self.estimated + self.unknown_model + self.missing_zone + self.missing_usage + self.errors
    }
}

/// Stamps the remote-region policy onto a stored estimate blob. A future
/// estimator may return a more precise `remote_region`; its statement wins.
fn stamp_region(mut response: Value, region: &EstimationRegion) -> Value {
    if let Some(obj) = response.as_object_mut() {
        obj.entry("remote_region")
            .or_insert_with(|| region.to_json());
    }
    response
}

/// Extracts `ecologits-<version>` from a sidecar response's
/// `methodology.ecologits_version`, falling back to `"ecologits-unknown"`
/// for responses (failure statuses) that carry no `methodology` object.
fn methodology_version(response: &Value) -> String {
    response
        .get("methodology")
        .and_then(|m| m.get("ecologits_version"))
        .and_then(Value::as_str)
        .map(|v| format!("ecologits-{v}"))
        .unwrap_or_else(|| "ecologits-unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methodology_version_extracts_from_ok_response() {
        let resp = json!({
            "status": "ok",
            "methodology": {"ecologits_version": "0.11.1", "source": "bundled"}
        });
        assert_eq!(methodology_version(&resp), "ecologits-0.11.1");
    }

    #[test]
    fn methodology_version_falls_back_when_methodology_absent() {
        let resp = json!({"status": "unknown_model"});
        assert_eq!(methodology_version(&resp), "ecologits-unknown");
    }

    #[test]
    fn stamp_region_records_an_explicit_override() {
        let stamped = stamp_region(
            json!({"status": "ok"}),
            &EstimationRegion::explicit("FRA", "flag"),
        );
        assert_eq!(
            stamped,
            json!({"status": "ok", "remote_region": {"id": "FRA", "source": "flag"}})
        );
    }

    #[test]
    fn stamp_region_records_automatic_detection_without_inventing_an_id() {
        let stamped = stamp_region(json!({"status": "ok"}), &EstimationRegion::automatic());
        assert_eq!(
            stamped["remote_region"],
            json!({"source": "estimator_auto"})
        );
    }

    #[test]
    fn stamp_region_never_overwrites_the_sidecar() {
        let stamped = stamp_region(
            json!({"status": "ok", "remote_region": {"id": "DEU", "source": "provider"}}),
            &EstimationRegion::explicit("FRA", "flag"),
        );
        assert_eq!(stamped["remote_region"]["id"], json!("DEU"));
    }

    #[test]
    fn automatic_region_omits_the_estimator_override() {
        let request = estimate_request("anthropic", "claude", 42, &EstimationRegion::automatic());
        assert!(request.get("electricity_mix_zone").is_none());
    }

    #[test]
    fn explicit_region_sets_the_estimator_override() {
        let request = estimate_request(
            "anthropic",
            "claude",
            42,
            &EstimationRegion::explicit("FRA", "flag"),
        );
        assert_eq!(request["electricity_mix_zone"], "FRA");
    }
}
