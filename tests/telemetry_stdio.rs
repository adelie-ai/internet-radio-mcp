#![deny(warnings)]

// Acceptance tests for the telemetry internet-radio-mcp inherits from
// mcp-core's `run`: the stdio transport keeps stdout clean at any log
// level, and every tool's caller-chosen content -- a station name, a search
// query, or a stream URL -- stays off an INFO line (D10, the level
// contract).
//
// Table-driven over the server's full tool list (mcp-core#40, lesson 8): a
// tool added without a case in `support::sentinel_arguments_by_tool` fails
// here by name, not silently.
//
// Each test spawns the real binary. Only a real process proves what reaches
// file descriptor 1 and what the installed subscriber really writes to
// stderr; an in-process capturing layer only proves what a test told a
// layer to do. `radio_search` needs the Radio Browser directory, which the
// child process is pointed at a local mock server for via
// `INTERNET_RADIO_MCP_RADIO_BROWSER_BASE` -- never the live directory
// (mcp-core#40: no test may reach a live directory or stream).

mod support;

use httpmock::Method::GET;
use httpmock::MockServer;
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Child, Command, Output, Stdio};

use support::{SENTINEL, assert_tool_coverage_is_complete, sentinel_arguments_by_tool};

fn station_fixture() -> Value {
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

fn spawn_with_log_level(level: &str, radio_browser_base: &str) -> Child {
    let exe = env!("CARGO_BIN_EXE_internet-radio-mcp");
    Command::new(exe)
        .args(["serve", "--transport", "stdio"])
        .env("RUST_LOG", level)
        .env("INTERNET_RADIO_MCP_RADIO_BROWSER_BASE", radio_browser_base)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn internet-radio-mcp serve --transport stdio")
}

fn run_requests(level: &str, radio_browser_base: &str, requests: &[Value]) -> Output {
    let mut child = spawn_with_log_level(level, radio_browser_base);
    {
        let stdin = child.stdin.as_mut().expect("child has a piped stdin");
        for request in requests {
            writeln!(stdin, "{request}").expect("write jsonrpc line");
        }
    }
    drop(child.stdin.take());
    child.wait_with_output().expect("child must exit")
}

/// The level word `tracing_subscriber`'s default console formatter writes as
/// the second whitespace-separated token, right after the timestamp. Reading
/// it this way (rather than a substring search for "INFO") does not confuse
/// a level word for content that happens to contain the same letters.
fn line_level(line: &str) -> Option<&str> {
    line.split_whitespace()
        .nth(1)
        .filter(|token| matches!(*token, "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE"))
}

/// Build the initialize/tool-calls/shutdown request sequence for every
/// registered case, and the number of requests in it that carry an `id`
/// (and so expect a reply).
fn requests_for_all_tools(cases: &[(&'static str, Value)]) -> (Vec<Value>, usize) {
    let mut requests = vec![json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {}},
    })];
    requests.push(json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}));
    for (i, (name, args)) in cases.iter().enumerate() {
        requests.push(json!({
            "jsonrpc": "2.0",
            "id": i + 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": args},
        }));
    }
    let shutdown_id = cases.len() + 2;
    requests.push(json!({"jsonrpc": "2.0", "id": shutdown_id, "method": "shutdown", "params": {}}));
    (requests, cases.len() + 2)
}

#[test]
fn stdout_carries_only_jsonrpc_at_trace_level() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET).path("/json/stations/search");
        then.status(200).json_body(station_fixture());
    });
    let base_url = format!("{}/json", server.base_url());

    let cases = sentinel_arguments_by_tool();
    assert_tool_coverage_is_complete(&cases);
    let (requests, expected_replies) = requests_for_all_tools(&cases);

    let output = run_requests("trace", &base_url, &requests);
    assert!(
        output.status.success(),
        "internet-radio-mcp must exit cleanly, otherwise an empty stdout proves nothing: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let mut replies = 0;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!("every stdout line must be JSON-RPC, but {line:?} is not: {e}")
        });
        assert_eq!(
            value.get("jsonrpc").and_then(Value::as_str),
            Some("2.0"),
            "every stdout line must carry the JSON-RPC envelope: {line:?}"
        );
        replies += 1;
    }
    assert_eq!(
        replies, expected_replies,
        "expected one reply per request that carried an id"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("INFO") || stderr.contains("DEBUG") || stderr.contains("TRACE"),
        "at RUST_LOG=trace the subscriber must be installed and log to stderr; stderr was: \
         {stderr:?}"
    );
}

/// AC (mcp-core#40, epic D10): no tool's sentinel-bearing content reaches an
/// INFO-or-louder line, for any tool the server exposes.
#[test]
fn no_sentinel_reaches_an_info_line() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET).path("/json/stations/search");
        then.status(200).json_body(station_fixture());
    });
    let base_url = format!("{}/json", server.base_url());

    let cases = sentinel_arguments_by_tool();
    assert_tool_coverage_is_complete(&cases);
    let (requests, _) = requests_for_all_tools(&cases);

    let output = run_requests("trace", &base_url, &requests);
    assert!(
        output.status.success(),
        "internet-radio-mcp must exit cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    let mut saw_sentinel_at_debug = false;
    for line in stderr.lines() {
        if !line.contains(SENTINEL) {
            continue;
        }
        let level = line_level(line);
        assert!(
            matches!(level, Some("DEBUG") | Some("TRACE")),
            "the sentinel reached a line at level {level:?}, at or above INFO: {line:?}"
        );
        if level == Some("DEBUG") {
            saw_sentinel_at_debug = true;
        }
    }
    assert!(
        saw_sentinel_at_debug,
        "the sentinel must still be reachable at DEBUG, or this test cannot tell a real fix \
         from a line that was simply deleted; stderr was: {stderr:?}"
    );
}
