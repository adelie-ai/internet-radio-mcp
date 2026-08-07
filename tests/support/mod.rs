//! A capturing `tracing` layer, and a driver that runs internet-radio-mcp's
//! real service under it.
//!
//! The telemetry criteria are about what the dispatch and handler paths
//! emit, so a test has to read the spans and events back rather than assert
//! a constant against itself. Each test file gets its own copy of this
//! module (adapted from mcp-core's own test support), so not every item is
//! reached from every file.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use internet_radio_mcp::RadioService;
use mcp_core::{McpService, ServerCore, Session};
use serde_json::{Value, json};
use tracing::Level;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

/// One span, as the subscriber saw it. A span whose fields are recorded after
/// creation appears a second time, carrying only what was recorded then.
#[derive(Clone, Debug)]
pub struct RecordedSpan {
    /// The span's name.
    pub name: &'static str,
    /// Field name to its rendered value.
    pub fields: BTreeMap<String, String>,
}

/// One event, as the subscriber saw it.
#[derive(Clone, Debug)]
pub struct RecordedEvent {
    /// The level the event was emitted at.
    pub level: Level,
    /// Field name to its rendered value. The message is the `message` field.
    pub fields: BTreeMap<String, String>,
}

/// Everything one captured run produced.
#[derive(Clone, Debug, Default)]
pub struct Recorded {
    /// Spans, in the order they opened.
    pub spans: Vec<RecordedSpan>,
    /// Events, in the order they were emitted.
    pub events: Vec<RecordedEvent>,
}

impl Recorded {
    /// A short rendering for an assertion message.
    pub fn span_summary(&self) -> Vec<String> {
        self.spans
            .iter()
            .map(|span| format!("{}{:?}", span.name, span.fields))
            .collect()
    }

    /// A short rendering for an assertion message.
    pub fn event_summary(&self) -> Vec<String> {
        self.events
            .iter()
            .map(|event| format!("{}{:?}", event.level, event.fields))
            .collect()
    }
}

/// Run `body` with a capturing subscriber installed on this thread, and
/// return what it emitted.
pub fn capture<F, Fut>(body: F) -> Recorded
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::with_default(subscriber, || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(body());
    });
    capture.take()
}

/// A shared core over internet-radio-mcp's real, zero-config service.
pub fn demo_core() -> Arc<ServerCore> {
    let service = internet_radio_mcp::build_service();
    ServerCore::new(internet_radio_mcp::server_config(), Arc::new(service))
}

/// A shared core over a `RadioService` pointed at a custom Radio Browser base
/// URL -- a local mock server in every test that uses this, never the live
/// directory (mcp-core#40).
pub fn demo_core_with_base(base_url: &str) -> Arc<ServerCore> {
    let service = RadioService::with_radio_browser_base(base_url.to_string());
    ServerCore::new(internet_radio_mcp::server_config(), Arc::new(service))
}

/// Drive `messages` through one session over the real, zero-config service,
/// capturing what the dispatch and handler paths emitted.
pub fn capture_dispatch(messages: &[Value]) -> Recorded {
    let messages = messages.to_vec();
    capture(|| async move {
        let mut session = Session::new(demo_core());
        for message in messages {
            session.handle_message(message).await;
        }
    })
}

/// As [`capture_dispatch`], but over a `RadioService` pointed at `base_url`.
pub fn capture_dispatch_with_base(base_url: &str, messages: &[Value]) -> Recorded {
    let messages = messages.to_vec();
    let base_url = base_url.to_string();
    capture(|| async move {
        let mut session = Session::new(demo_core_with_base(&base_url));
        for message in messages {
            session.handle_message(message).await;
        }
    })
}

// ── the sentinel and the full-tool-list table (mcp-core#40, lesson 8) ──────
//
// The span-field test and the console test both proved the mechanism works
// on one tool and missed a leak on the others (mcp-core#40, lesson 8's
// account of what happened elsewhere in this epic). This table is the fix:
// one list, shared by every content test in this crate, and a completeness
// check that fails the moment a tool has no entry.

/// Coverage has three dimensions: every tool, every path, and every
/// content-bearing argument within each call (mcp-core#40, lesson 13). A
/// tool with three arguments and a sentinel in only one of them is
/// one-third covered, and nothing says so unless each argument gets its own
/// value -- a shared sentinel across two fields cannot name which one
/// leaked when they leak together as one opaque blob field (the common
/// shape of a leak: `#[instrument]` without `skip_all` capturing the whole
/// arguments map as a single `args` field).
///
/// `radio_search`'s query.
pub const SENTINEL: &str = "MARKER-radio-secret-9f3d1c2a";

/// `radio_play`'s display name.
pub const NAME_SENTINEL: &str = "MARKER-radio-name-7b2e4f1d";

/// `radio_play`'s stream url, embedded as a path segment after a scheme
/// `validate_stream_url` rejects outright (see [`sentinel_arguments_by_tool`]).
pub const URL_SENTINEL: &str = "MARKER-radio-url-3c9a8e60";

/// `radio_play`'s station uuid, shaped like a valid one (36 hex-or-hyphen
/// characters). [`SENTINEL`] and friends cannot stand in for a uuid
/// argument: their letters are not all hex digits, so `validate_uuid` would
/// reject one before it ever reached the uuid-lookup path this is meant to
/// exercise.
pub const UUID_SENTINEL: &str = "00000000-0000-4000-8000-c0ffeec0ffee";

/// Every sentinel a content test in this crate might hunt for, in one place
/// so a new content-bearing argument gets its own entry here too.
pub const ALL_SENTINELS: &[&str] = &[SENTINEL, NAME_SENTINEL, URL_SENTINEL, UUID_SENTINEL];

/// One argument set per tool the server exposes, each carrying a distinct
/// sentinel in every content-bearing argument that tool has.
///
/// `radio_play`'s url uses a scheme `validate_stream_url` rejects outright,
/// so the call is safe to drive against the live spawned binary too, not
/// only the in-process mock (mcp-core#40: no test may reach a live directory
/// or stream). `radio_stop` and `radio_now_playing` take no arguments, so
/// their case is empty -- there is nothing for them to leak, but they still
/// have to open their own span.
pub fn sentinel_arguments_by_tool() -> Vec<(&'static str, Value)> {
    vec![
        ("radio_search", json!({"query": SENTINEL})),
        (
            "radio_play",
            json!({
                "url": format!("ftp://blocked.example.com/{URL_SENTINEL}"),
                "name": NAME_SENTINEL,
            }),
        ),
        ("radio_stop", json!({})),
        ("radio_now_playing", json!({})),
    ]
}

/// The `#[tracing::instrument]`-ed span a tool's handler opens, by this
/// crate's own naming convention (`radio_search` -> `exec_radio_search`).
pub fn expected_span_name(tool: &str) -> String {
    format!("exec_{tool}")
}

/// Assert every tool `RadioService` actually exposes has a case in `cases`,
/// and every case in `cases` names a real tool.
///
/// This is what makes "adding a tool without listing it" fail the test
/// (mcp-core#40, lesson 8) instead of silently shipping uncovered: the
/// comparison is against the service's own live `tools()` list, not a second
/// hand-maintained count.
pub fn assert_tool_coverage_is_complete(cases: &[(&'static str, Value)]) {
    let real_tools: std::collections::BTreeSet<String> = RadioService::new()
        .tools()
        .into_iter()
        .map(|t| t.name)
        .collect();
    let case_names: std::collections::BTreeSet<&str> =
        cases.iter().map(|(name, _)| *name).collect();

    let missing: Vec<_> = real_tools
        .iter()
        .filter(|name| !case_names.contains(name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "tool(s) {missing:?} have no sentinel test case -- add one to \
         sentinel_arguments_by_tool so a new tool cannot ship uncovered"
    );

    let stale: Vec<_> = case_names
        .iter()
        .filter(|name| !real_tools.contains(**name))
        .collect();
    assert!(
        stale.is_empty(),
        "sentinel test case(s) {stale:?} do not correspond to a real tool -- fix or remove them"
    );
}

// ── driving failure and empty branches, not only success (lesson 9) ───────
//
// Covering every tool is not covering every path: an error type's `Display`
// is written to be helpful, and helpful means quoting what failed, so the
// failure branch is exactly where a station name or a stream URL is most
// likely to end up embedded in a message that also reaches a log field. A
// content test that only ever drives a mocked upstream that succeeds never
// runs that code at all.

/// `radio_play`'s uuid-lookup path, alongside its baseline (url-based) case
/// in [`sentinel_arguments_by_tool`]. Not part of the completeness net --
/// this exercises an additional branch within a tool the net already
/// covers, not an additional tool -- so it lives in its own list rather
/// than growing the per-tool table into a per-branch one.
pub fn uuid_lookup_case() -> (&'static str, Value) {
    ("radio_play", json!({"uuid": UUID_SENTINEL}))
}

/// Which of [`ALL_SENTINELS`] actually appear in `cases`' arguments, so a
/// positive-control check only demands what a scenario actually sent.
pub fn sentinels_present_in(cases: &[(&'static str, Value)]) -> Vec<&'static str> {
    let haystack: String = cases.iter().map(|(_, args)| args.to_string()).collect();
    ALL_SENTINELS
        .iter()
        .copied()
        .filter(|sentinel| haystack.contains(sentinel))
        .collect()
}

/// Assert that every tool named in `expected_tools` opened its own span
/// (the positive half: this cannot pass simply because nothing was
/// instrumented), and that no sentinel in [`ALL_SENTINELS`] reached a span
/// field or an INFO-or-louder event, anywhere in `recorded`. Each argument's
/// own distinct sentinel means a failure names which one leaked.
pub fn assert_no_leak(recorded: &Recorded, expected_tools: &[&str]) {
    for name in expected_tools {
        let expected = expected_span_name(name);
        assert!(
            recorded.spans.iter().any(|s| s.name == expected),
            "expected a {expected:?} span for tool {name:?}; spans were {:?}",
            recorded.span_summary()
        );
    }

    for span in &recorded.spans {
        for (key, value) in &span.fields {
            for sentinel in ALL_SENTINELS {
                assert!(
                    !value.contains(sentinel),
                    "sentinel {sentinel:?} leaked into span {:?} field {key:?}: {value:?}; all \
                     spans were {:?}",
                    span.name,
                    recorded.span_summary()
                );
            }
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
            for sentinel in ALL_SENTINELS {
                assert!(
                    !value.contains(sentinel),
                    "sentinel {sentinel:?} leaked into a {} line, field {key:?}: {value:?}; all \
                     events were {:?}",
                    event.level,
                    recorded.event_summary()
                );
            }
        }
    }
}

/// Assert each of `sentinels` is still reachable at DEBUG somewhere in
/// `recorded` -- the positive control that keeps [`assert_no_leak`] from
/// passing simply because a line was deleted rather than lowered.
pub fn assert_reachable_at_debug(recorded: &Recorded, sentinels: &[&str]) {
    for sentinel in sentinels {
        let at_debug = recorded
            .events
            .iter()
            .any(|e| e.level == Level::DEBUG && e.fields.values().any(|v| v.contains(sentinel)));
        assert!(
            at_debug,
            "sentinel {sentinel:?} must still be reachable at DEBUG, or this test cannot tell a \
             real fix from a line that was simply deleted; events were {:?}",
            recorded.event_summary()
        );
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Recorded>>);

impl Capture {
    fn take(self) -> Recorded {
        self.0
            .lock()
            .expect("the capture lock is only held to push one record")
            .clone()
    }
}

impl<S> Layer<S> for Capture
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut fields = BTreeMap::new();
        attrs.record(&mut Collector(&mut fields));
        self.0
            .lock()
            .expect("the capture lock is only held to push one record")
            .spans
            .push(RecordedSpan {
                name: attrs.metadata().name(),
                fields,
            });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let name = ctx.span(id).map_or("<closed>", |span| span.name());
        let mut fields = BTreeMap::new();
        values.record(&mut Collector(&mut fields));
        self.0
            .lock()
            .expect("the capture lock is only held to push one record")
            .spans
            .push(RecordedSpan { name, fields });
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = BTreeMap::new();
        event.record(&mut Collector(&mut fields));
        self.0
            .lock()
            .expect("the capture lock is only held to push one record")
            .events
            .push(RecordedEvent {
                level: *event.metadata().level(),
                fields,
            });
    }
}

struct Collector<'a>(&'a mut BTreeMap<String, String>);

impl Visit for Collector<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}
