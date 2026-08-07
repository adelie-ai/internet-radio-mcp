//! Acceptance criteria for internet-radio-mcp's own telemetry (mcp-core#40).
//!
//! Each test is named after the criterion it holds, so a failing run names
//! the unmet requirement rather than a line number.
//!
//! Content-leak coverage (mcp-core#40, lessons 8 and 9): table-driven over
//! every tool, and every scenario below is run against a success, a
//! failure, and (for `radio_search`) an empty upstream response. An error
//! type's `Display` is written to be helpful, and helpful means quoting
//! what failed -- `RadioError::NoStationsFound`'s message embeds the raw
//! query, and the "Station UUID not found" message embeds the raw uuid --
//! so the failure branch is exactly where a leak most naturally hides, and
//! a suite that only ever mocks success never runs that code at all.

mod support;

use httpmock::Method::GET;
use httpmock::MockServer;
use mcp_core::telemetry::metrics::{self, Label};
use serde_json::json;

use support::{
    Recorded, assert_no_leak, assert_reachable_at_debug, assert_tool_coverage_is_complete,
    capture_dispatch_with_base, sentinel_arguments_by_tool, sentinels_present_in, uuid_lookup_case,
};

/// The metrics registry [`mcp_core::telemetry::metrics`] records into is
/// process-global, and `cargo test` runs a file's tests concurrently by
/// default (mcp-core#40, lesson 6). Every test below that touches it is
/// serialised behind this mutex; it holds no data of its own.
static METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_metrics() -> std::sync::MutexGuard<'static, ()> {
    METRICS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A minimal, schema-accurate Radio Browser station: only the fields
/// `Station` reads. Shape verified 2026-08-07 against the live
/// `/json/stations/search` endpoint; the name and URL here are placeholders,
/// never the real directory's data (rule 5.3).
fn station_fixture() -> serde_json::Value {
    json!([{
        "stationuuid": "00000000-0000-4000-8000-000000000001",
        "name": "Test Jazz Radio",
        "url_resolved": "https://stream.example.com/jazz128",
        "country": "Testland",
        "tags": "jazz,test",
        "bitrate": 128,
        "codec": "MP3",
        "votes": 42
    }])
}

/// Drive `cases` (initialize, then one `tools/call` per case) against a
/// `RadioService` pointed at `base_url`, and assert the standard
/// leak-freedom properties: every named tool opens its own span, no span
/// field or INFO-or-louder event carries a sentinel, and every sentinel the
/// cases actually sent is still reachable at DEBUG.
fn run_leak_check(base_url: &str, cases: &[(&'static str, serde_json::Value)]) -> Recorded {
    let mut messages =
        vec![json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})];
    for (i, (name, args)) in cases.iter().enumerate() {
        messages.push(json!({
            "jsonrpc": "2.0",
            "id": i + 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": args},
        }));
    }

    let recorded = capture_dispatch_with_base(base_url, &messages);

    let expected_tools: Vec<&str> = cases.iter().map(|(name, _)| *name).collect();
    assert_no_leak(&recorded, &expected_tools);
    assert_reachable_at_debug(&recorded, &sentinels_present_in(cases));

    recorded
}

/// AC (epic D10 / mcp-core#40, lesson 8): no station name, search query, or
/// stream URL reaches a span field or an INFO-or-louder line, for every tool
/// the server exposes, on the *success* path -- not just the one tool this
/// suite happens to remember to drive. `assert_tool_coverage_is_complete`
/// makes that a property of the test rather than a promise: a tool added
/// without a registered case fails here, by name, instead of shipping
/// silently uncovered.
#[test]
fn tool_call_records_no_arguments_on_search_success() {
    let _guard = lock_metrics();
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET).path("/json/stations/search");
        then.status(200).json_body(station_fixture());
    });
    let base_url = format!("{}/json", server.base_url());

    let cases = sentinel_arguments_by_tool();
    assert_tool_coverage_is_complete(&cases);

    run_leak_check(&base_url, &cases);
}

/// AC (mcp-core#40, lesson 9): the same property holds when the directory
/// call fails outright. `RadioError::ApiError`'s message embeds the HTTP
/// status but not the query, so this is mostly a proof that the failure
/// branch is exercised at all -- the sharper case is the empty-result and
/// uuid-not-found scenarios below, whose messages do embed a caller value.
#[test]
fn tool_call_records_no_arguments_on_search_failure() {
    let _guard = lock_metrics();
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET).path("/json/stations/search");
        then.status(503);
    });
    let base_url = format!("{}/json", server.base_url());

    let cases = sentinel_arguments_by_tool();
    assert_tool_coverage_is_complete(&cases);

    run_leak_check(&base_url, &cases);
}

/// AC (mcp-core#40, lesson 9): an empty search result builds
/// `RadioError::NoStationsFound(query)`, whose `Display` embeds the raw
/// query text -- the query itself, quoted back in a message that becomes
/// the tool's `CallError::Tool`. That message still must not reach a span
/// field or an INFO-or-louder line.
#[test]
fn tool_call_records_no_arguments_on_search_empty_result() {
    let _guard = lock_metrics();
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET).path("/json/stations/search");
        then.status(200).json_body(json!([]));
    });
    let base_url = format!("{}/json", server.base_url());

    let cases = sentinel_arguments_by_tool();
    assert_tool_coverage_is_complete(&cases);

    run_leak_check(&base_url, &cases);
}

/// AC (mcp-core#40, lesson 9): `radio_play`'s uuid-lookup branch, driven
/// separately from its baseline (url-based) case. When the uuid resolves to
/// no station, `exec_radio_play` builds `"Station UUID not found: {uuid}"`
/// -- a message that embeds the raw uuid -- so this is the sharpest leak
/// check in the suite: a real, present-day message-construction site that
/// quotes a caller value back.
#[test]
fn tool_call_records_no_arguments_on_play_by_uuid_not_found() {
    let _guard = lock_metrics();
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/json/stations/byuuid/{}", support::UUID_SENTINEL));
        then.status(200).json_body(json!([]));
    });
    let base_url = format!("{}/json", server.base_url());

    run_leak_check(&base_url, &[uuid_lookup_case()]);
}

/// AC (mcp-core#40, lesson 9): the uuid-lookup branch when the directory
/// call itself fails, rather than returning an empty result.
#[test]
fn tool_call_records_no_arguments_on_play_by_uuid_upstream_failure() {
    let _guard = lock_metrics();
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/json/stations/byuuid/{}", support::UUID_SENTINEL));
        then.status(503);
    });
    let base_url = format!("{}/json", server.base_url());

    run_leak_check(&base_url, &[uuid_lookup_case()]);
}

/// AC (mcp-core#40): a Radio Browser directory fault increments
/// `radio.upstream_failures`, labelled `tool=radio_search, reason=directory`.
#[test]
fn radio_search_records_directory_upstream_failure() {
    let _guard = lock_metrics();
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET).path("/json/stations/search");
        then.status(503);
    });
    let base_url = format!("{}/json", server.base_url());

    let labels = [
        Label::new("tool", "radio_search"),
        Label::new("reason", "directory"),
    ];
    let before = counter_total("radio.upstream_failures", &labels);

    capture_dispatch_with_base(
        &base_url,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "radio_search", "arguments": {"query": "jazz"}},
            }),
        ],
    );

    assert_eq!(
        counter_total("radio.upstream_failures", &labels),
        before + 1,
        "a directory HTTP fault must increment the counter"
    );
}

/// AC (mcp-core#40, rule 8.2): an empty directory result ("no stations
/// found") is a normal decline, not a fault, so it must not move the
/// upstream-failure counter.
#[test]
fn radio_search_empty_result_does_not_count_as_upstream_failure() {
    let _guard = lock_metrics();
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET).path("/json/stations/search");
        then.status(200).json_body(json!([]));
    });
    let base_url = format!("{}/json", server.base_url());

    let labels = [
        Label::new("tool", "radio_search"),
        Label::new("reason", "directory"),
    ];
    let before = counter_total("radio.upstream_failures", &labels);

    capture_dispatch_with_base(
        &base_url,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "radio_search", "arguments": {"query": "no-such-genre"}},
            }),
        ],
    );

    assert_eq!(
        counter_total("radio.upstream_failures", &labels),
        before,
        "an empty ('no stations found') result must not count as an upstream failure"
    );
}

fn counter_total(name: &str, labels: &[Label]) -> u64 {
    metrics::global()
        .snapshot()
        .counters
        .iter()
        .find(|counter| counter.name == name && same_labels(&counter.labels, labels))
        .map_or(0, |counter| counter.total)
}

fn same_labels(recorded: &[Label], wanted: &[Label]) -> bool {
    recorded.len() == wanted.len()
        && wanted.iter().all(|want| {
            recorded
                .iter()
                .any(|have| have.key() == want.key() && have.value() == want.value())
        })
}
