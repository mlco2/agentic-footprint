//! The derived-view rebuild writes only what changed: a second rebuild over
//! the same raw facts must not rewrite a single join row. This is the
//! regression test for the write amplification that cost 3.4 GB/day.

use std::collections::BTreeSet;
use std::path::Path;

use afoot_core::{rebuild_derived_stored, EstimationRegion, Zone};
use afoot_events::{fixtures, Payload};
use afoot_store::Store;

const SESSION: &str = "sess-rebuild";

fn seeded_store() -> Store {
    let mut store = Store::open(Path::new(":memory:")).expect("open");
    store
        .insert_events(&[
            fixtures::envelope(
                "evt-span-000000001",
                SESSION,
                "2026-07-25T12:00:10Z",
                Payload::ActionSpan(fixtures::action_span(
                    "span-a",
                    "2026-07-25T12:00:00Z",
                    "2026-07-25T12:00:10Z",
                )),
            ),
            fixtures::envelope(
                "evt-llm-0000000001",
                SESSION,
                "2026-07-25T12:00:05Z",
                Payload::LlmCall(fixtures::llm_call()),
            ),
        ])
        .expect("seed");
    store
}

#[test]
fn rebuilding_unchanged_facts_writes_no_join_rows() {
    let mut store = seeded_store();
    let only: BTreeSet<String> = [SESSION.to_string()].into();
    let region = EstimationRegion::automatic();

    let first = rebuild_derived_stored(
        &mut store,
        Zone::unresolved("WOR", "default"),
        &region,
        Some(&only),
    )
    .expect("first rebuild");
    assert!(first.joins > 0, "the seeded session must produce joins");
    assert_eq!(first.joins_written, first.joins);

    let second = rebuild_derived_stored(
        &mut store,
        Zone::unresolved("WOR", "default"),
        &region,
        Some(&only),
    )
    .expect("second rebuild");
    assert_eq!(second.joins, first.joins);
    assert_eq!(
        second.joins_written, 0,
        "identical inputs must not rewrite any join"
    );
}
