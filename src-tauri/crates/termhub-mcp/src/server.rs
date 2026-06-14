//! The MCP server loop: read JSON-RPC messages on stdin, dispatch the three
//! methods we serve, write responses on stdout.
//!
//! Methods:
//!   - `initialize` → advertise protocol version + `tools` capability + server
//!     info. (We accept any client protocol version and echo a supported one.)
//!   - `notifications/initialized` → a notification; ack silently (no response).
//!   - `tools/list` → the static [`crate::tools`] catalog.
//!   - `tools/call` → validate the tool name, then forward
//!     `{command, args}` to the running app via [`crate::control_client`] and
//!     wrap the JSON result as MCP tool-call content.
//!
//! Anything else gets a JSON-RPC `method not found`.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::control_client::{self, ControlEndpoint};
use crate::protocol::{self, codes, Outbound, RpcRequest};
use crate::tools;

/// The MCP protocol version this server speaks. MCP is versioned by date string;
/// we echo a known-good one in `initialize`.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the stdio server loop until stdin closes. `reader`/`writer` are injected
/// so the loop is testable against in-memory buffers (the binary passes real
/// stdin/stdout).
pub fn run<R: BufRead, W: Write>(mut reader: R, mut writer: W) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break; // EOF: client closed the pipe.
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: RpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                // We can't correlate a malformed line to an id; emit a null-id
                // error so a strict client still sees the failure.
                let out = Outbound::Err(protocol::error(
                    Value::Null,
                    codes::INTERNAL_ERROR,
                    format!("malformed JSON-RPC: {e}"),
                ));
                write_line(&mut writer, &out)?;
                continue;
            }
        };

        // Notifications expect no response (notably notifications/initialized).
        if req.is_notification() {
            continue;
        }

        let id = req.id.clone().unwrap_or(Value::Null);
        let out = dispatch(&req, id);
        write_line(&mut writer, &out)?;
    }
    Ok(())
}

/// Serialize + write one outbound message as an NDJSON line.
fn write_line<W: Write>(writer: &mut W, out: &Outbound) -> std::io::Result<()> {
    writer.write_all(out.to_line().as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Dispatch one request to its handler, producing an outbound response.
fn dispatch(req: &RpcRequest, id: Value) -> Outbound {
    match req.method.as_str() {
        "initialize" => Outbound::Ok(protocol::success(id, initialize_result())),
        "tools/list" => Outbound::Ok(protocol::success(id, tools_list_result())),
        "tools/call" => tools_call(req, id),
        // `ping` is a common MCP keepalive; answer with an empty result.
        "ping" => Outbound::Ok(protocol::success(id, json!({}))),
        other => Outbound::Err(protocol::error(
            id,
            codes::METHOD_NOT_FOUND,
            format!("method not found: {other}"),
        )),
    }
}

/// The `initialize` result: protocol version, capabilities, server info.
fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            // We expose tools; we do not (yet) offer resources/prompts.
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "termhub-mcp",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "TermHub MCP server. Read tools (list_terminals, get_status, \
            supervision_tree, wsl_health, search_files, list_tabs, read_terminal) are \
            allowed. Organization tools (focus_session, move_tile, rename_tab, new_tab, \
            focus_tab, open_file) are audited. Process-changing tools (spawn_terminal, \
            send_text, send_keys, close_terminal) require confirmation. Calls are \
            forwarded to the running TermHub app over a local control channel."
    })
}

/// The `tools/list` result built from the static catalog.
fn tools_list_result() -> Value {
    let tools: Vec<Value> = tools::catalog().iter().map(|t| t.to_mcp()).collect();
    json!({ "tools": tools })
}

/// `tools/call`: validate the tool name + forward to the app.
///
/// MCP `tools/call` params are `{ "name": <tool>, "arguments": <object> }`. We
/// validate `name` against the catalog (rejecting unknown tools with a clear
/// error), resolve the control endpoint, forward `{command: name, args:
/// arguments}`, and wrap the result. Tool-level failures (e.g. the app gating a
/// process-changing tool, or TermHub not running) are returned as MCP tool
/// results with `isError: true` rather than transport errors, which is how MCP
/// surfaces tool failures to the model.
fn tools_call(req: &RpcRequest, id: Value) -> Outbound {
    let name = match req.params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return Outbound::Err(protocol::error(
                id,
                codes::INVALID_PARAMS,
                "tools/call requires a 'name'",
            ));
        }
    };

    // Validate against the catalog so unknown tools fail fast and clearly.
    if tools::find(name).is_none() {
        return Outbound::Err(protocol::error(
            id,
            codes::INVALID_PARAMS,
            format!("unknown tool: {name}"),
        ));
    }

    let arguments = req
        .params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    // Resolve the control channel; if TermHub isn't running, surface it as a
    // tool error (isError) so the model gets a readable message.
    let endpoint: ControlEndpoint = match control_client::resolve_endpoint() {
        Ok(ep) => ep,
        Err(e) => return Outbound::Ok(protocol::success(id, tool_error(&e))),
    };

    match control_client::call(&endpoint, name, &arguments) {
        Ok(result) => Outbound::Ok(protocol::success(id, tool_ok(&result))),
        Err(e) => Outbound::Ok(protocol::success(id, tool_error(&e))),
    }
}

/// Wrap a successful command result as MCP tool-call content. We return the JSON
/// both as pretty text (the human/model-readable block MCP requires) and as a
/// `structuredContent` object so a structured client can use it directly.
fn tool_ok(result: &Value) -> Value {
    let text = serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string());
    json!({
        "content": [ { "type": "text", "text": text } ],
        "structuredContent": result,
        "isError": false
    })
}

/// Wrap a tool failure as MCP tool-call content with `isError: true`.
fn tool_error(message: &str) -> Value {
    json!({
        "content": [ { "type": "text", "text": message } ],
        "isError": true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Drive the server with one or more request lines and collect the response
    /// lines (parsed as JSON).
    fn run_lines(input: &str) -> Vec<Value> {
        let reader = Cursor::new(input.as_bytes().to_vec());
        let mut out: Vec<u8> = Vec::new();
        run(reader, &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect()
    }

    #[test]
    fn initialize_advertises_tools_capability() {
        let resp = run_lines(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        assert_eq!(resp.len(), 1);
        let r = &resp[0]["result"];
        assert_eq!(r["protocolVersion"], PROTOCOL_VERSION);
        assert!(r["capabilities"]["tools"].is_object());
        assert_eq!(r["serverInfo"]["name"], "termhub-mcp");
    }

    #[test]
    fn initialized_notification_gets_no_response() {
        let resp = run_lines(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert!(resp.is_empty(), "notifications must not produce a response");
    }

    #[test]
    fn tools_list_returns_full_catalog() {
        let resp = run_lines(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let tools = resp[0]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), tools::catalog().len());
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"list_terminals"));
        assert!(names.contains(&"spawn_terminal"));
        assert!(names.contains(&"get_theme"));
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let resp = run_lines(r#"{"jsonrpc":"2.0","id":3,"method":"does/not/exist"}"#);
        assert_eq!(resp[0]["error"]["code"], codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn ping_is_answered() {
        let resp = run_lines(r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#);
        assert!(resp[0]["result"].is_object());
        assert!(resp[0].get("error").is_none());
    }

    #[test]
    fn tools_call_without_name_is_invalid_params() {
        let resp =
            run_lines(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"arguments":{}}}"#);
        assert_eq!(resp[0]["error"]["code"], codes::INVALID_PARAMS);
    }

    #[test]
    fn tools_call_unknown_tool_is_invalid_params() {
        let resp = run_lines(
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"bogus","arguments":{}}}"#,
        );
        assert_eq!(resp[0]["error"]["code"], codes::INVALID_PARAMS);
        assert!(resp[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown tool"));
    }

    #[test]
    fn tools_call_known_tool_without_app_returns_tool_error() {
        // Point discovery at a nonexistent handshake + clear env overrides so the
        // call cannot reach an app; the result must be a tool-level isError, not
        // a JSON-RPC transport error (so the model sees a readable message).
        std::env::set_var("TERMHUB_CONTROL_FILE", "/nonexistent/th-control.json");
        std::env::remove_var("TERMHUB_CONTROL_ADDR");
        std::env::remove_var("TERMHUB_CONTROL_TOKEN");

        let resp = run_lines(
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"list_terminals","arguments":{}}}"#,
        );
        // It's a success envelope at the RPC layer, with isError in the content.
        assert!(resp[0].get("error").is_none());
        assert_eq!(resp[0]["result"]["isError"], true);
        let text = resp[0]["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("control channel not found"), "text: {text}");

        std::env::remove_var("TERMHUB_CONTROL_FILE");
    }

    #[test]
    fn multiple_requests_on_one_stream() {
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
        );
        let resp = run_lines(input);
        // initialize + tools/list respond; the notification does not.
        assert_eq!(resp.len(), 2);
        assert_eq!(resp[0]["id"], 1);
        assert_eq!(resp[1]["id"], 2);
    }
}
