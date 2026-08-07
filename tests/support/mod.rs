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

/// The value every content test in this crate hunts for: the shape of a
/// caller-chosen station name, search query, or stream URL, which must never
/// leak into a span field or an INFO-or-louder line.
pub const SENTINEL: &str = "MARKER-radio-secret-9f3d1c2a";

/// One argument set per tool the server exposes, each carrying [`SENTINEL`]
/// somewhere a caller genuinely controls -- a search query or a station
/// name, wherever that tool has one.
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
            json!({"url": "ftp://blocked.example.com/stream", "name": SENTINEL}),
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
