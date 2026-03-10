# internet-radio-mcp

An MCP server that exposes internet radio search and playback as LLM-callable tools.

## Tools

| Tool | Description |
|---|---|
| `radio_search` | Search for stations by name or genre/tag via the [Radio Browser](https://www.radio-browser.info/) API |
| `radio_play` | Start playback of a station (by stream URL or Radio Browser UUID) via `mpv` |
| `radio_stop` | Stop all currently-playing `mpv` instances |
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

## Notes

- Playback state is in-process and resets on server restart.
- `radio_play` kills any previously-started `mpv` before starting a new stream.
- `radio_stop` uses `pkill mpv`; it will stop *all* `mpv` processes on the host.
