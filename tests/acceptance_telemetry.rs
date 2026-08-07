//! Acceptance criteria for internet-radio-mcp's own telemetry (mcp-core#40).
//!
//! Each test is named after the criterion it holds, so a failing run names
//! the unmet requirement rather than a line number.

mod support;

use httpmock::Method::GET;
use httpmock::MockServer;
use mcp_core::telemetry::metrics::{self, Label};
use serde_json::json;
use tracing::Level;

use support::{
    SENTINEL, assert_tool_coverage_is_complete, capture_dispatch_with_base, expected_span_name,
    sentinel_arguments_by_tool,
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

/// AC (epic D10 / mcp-core#40, lesson 8): no station name, search query, or
/// stream URL reaches a span field or an INFO-or-louder line, for every tool
/// the server exposes -- not just the one tool this suite happens to
/// remember to drive. `assert_tool_coverage_is_complete` makes that a
/// property of the test rather than a promise: a tool added without a
/// registered case fails here, by name, instead of shipping silently
/// uncovered.
///
/// The same run proves the positive half too: each call opens its own
/// `exec_radio_*` span, so this test cannot pass simply because nothing was
/// instrumented, and the sentinel is still visible at DEBUG (via mcp-core's
/// own dispatch-layer argument logging), so it cannot pass simply because a
/// line was deleted.
#[test]
fn tool_call_records_no_arguments() {
    let _guard = lock_metrics();
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET).path("/json/stations/search");
        then.status(200).json_body(station_fixture());
    });
    let base_url = format!("{}/json", server.base_url());

    let cases = sentinel_arguments_by_tool();
    assert_tool_coverage_is_complete(&cases);

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

    let recorded = capture_dispatch_with_base(&base_url, &messages);

    for (name, _) in &cases {
        let expected = expected_span_name(name);
        assert!(
            recorded.spans.iter().any(|s| s.name == expected),
            "expected a {expected:?} span for tool {name:?}; spans were {:?}",
            recorded.span_summary()
        );
    }

    for span in &recorded.spans {
        for (key, value) in &span.fields {
            assert!(
                !value.contains(SENTINEL),
                "the sentinel leaked into span {:?} field {key:?}: {value:?}; all spans were {:?}",
                span.name,
                recorded.span_summary()
            );
        }
    }

    for event in &recorded.events {
        // DEBUG/TRACE may legitimately carry tool arguments (D10) -- that is
        // mcp-core's own dispatch layer, inherited rather than added here.
        // Only INFO and louder are checked.
        if event.level > Level::INFO {
            continue;
        }
        for (key, value) in &event.fields {
            assert!(
                !value.contains(SENTINEL),
                "the sentinel leaked into a {} line, field {key:?}: {value:?}; all events were {:?}",
                event.level,
                recorded.event_summary()
            );
        }
    }

    let at_debug = recorded
        .events
        .iter()
        .any(|e| e.level == Level::DEBUG && e.fields.values().any(|v| v.contains(SENTINEL)));
    assert!(
        at_debug,
        "the sentinel must still be reachable at DEBUG, or this test cannot tell a real fix \
         from a line that was simply deleted; events were {:?}",
        recorded.event_summary()
    );
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
