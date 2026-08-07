# internet-radio-mcp

An MCP server that exposes internet radio search and playback as LLM-callable tools.

## Tools

| Tool | Description |
|---|---|
| `radio_search` | Search for stations by name or genre/tag via the [Radio Browser](https://www.radio-browser.info/) API |
| `radio_play` | Start playback of a station (by stream URL or Radio Browser UUID) via `mpv` |
| `radio_stop` | Stop the currently-playing station (terminates the tracked `mpv` process) |
| `radio_now_playing` | Return the name and URL of the currently-playing station |

## Requirements

- **mpv** must be installed and available on `PATH` for playback.
- Network access to `de1.api.radio-browser.info` for station search.

## Usage

### Stdio (VS Code / Claude Desktop)

```bash
cargo build --release
./target/release/internet-radio-mcp serve --mode stdio
```

### WebSocket

```bash
./target/release/internet-radio-mcp serve --mode websocket --host 127.0.0.1 --port 8080
# Connect at ws://127.0.0.1:8080/ws
```

## Example session

```jsonc
// Search for jazz stations
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"radio_search","arguments":{"query":"jazz","by":"tag","limit":5}}}

// Play the top result
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"radio_play","arguments":{"url":"https://stream.example.com/jazz128","name":"Jazz FM"}}}

// Check what's on
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"radio_now_playing","arguments":{}}}

// Stop
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"radio_stop","arguments":{}}}
```

## Logging

`mcp-core`'s `run` installs the process subscriber; this crate calls nothing
to get it. Logs go to stderr, never stdout -- the stdio transport frames
JSON-RPC on stdout, and one log line there would corrupt the protocol
stream. `RUST_LOG` sets the level (default `info`); see `mcp-core`'s own
README for the full level contract, the request/tool-call spans, and the
standard `OTEL_*` environment variables.

What this server adds on top of what it inherits:

- A `debug!` line for each call to the Radio Browser directory (a search, or
  a UUID lookup) and each attempt to start stream playback. A search query,
  a station UUID, and a stream URL are all tool arguments -- what someone is
  choosing to listen to, a preference -- so they stay at DEBUG and never
  reach a span field. `RUST_LOG=debug` is what it takes to see them.
- `radio.upstream_failures`, a counter labelled `tool` and `reason`
  (`directory` for a Radio Browser fault, `player` for an mpv fault), for a
  failure reaching outward. An empty search result ("no stations found") is
  the directory doing its job, not a fault, and is not counted here.
- `mcp-core` already records a tool-call counter and a latency histogram by
  tool and outcome (`mcp.tools.call`, `mcp.tools.call.duration`); this server
  does not duplicate them.

### The `otel` feature

Off by default. A pure passthrough --
`internet-radio-mcp -> mcp-core -> adelie-telemetry` -- so this crate takes
no direct dependency on `adelie-telemetry` or on any opentelemetry crate.
With the feature off, `cargo tree` resolves no opentelemetry crate at all.

```bash
cargo build --features otel
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 \
  ./target/debug/internet-radio-mcp serve --transport stdio
```

With no collector configured, the periodic metrics summary still writes to
stderr, so a default-feature install from `cargo install` gets real numbers
in the journal.

## Notes

- Playback state is in-process and resets on server restart.
- `radio_play` stops the previously-tracked station (if any) before starting a new stream.
- `radio_stop` sends `SIGTERM` to the tracked `mpv` process only; it does not touch other `mpv` instances on the host.
- Audio plays through the speakers of the machine running the server (playback is a local `mpv` process).
