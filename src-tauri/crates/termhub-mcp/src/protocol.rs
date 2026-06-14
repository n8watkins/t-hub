//! Minimal JSON-RPC 2.0 framing for MCP over stdio.
//!
//! MCP (the Model Context Protocol) is JSON-RPC 2.0. A stdio MCP server reads
//! one JSON-RPC message per line on stdin and writes one JSON-RPC message per
//! line on stdout (newline-delimited; stderr is for human logs). We implement
//! only the request/response + notification shapes the three methods we serve
//! need (`initialize`, `tools/list`, `tools/call`) — small enough to do directly
//! rather than pull an SDK.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A decoded inbound JSON-RPC message. We only distinguish what we must: an `id`
/// (present ⇒ request expecting a response; absent ⇒ notification we ack
/// silently), the `method`, and the `params`.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: Option<String>,
    /// Present for requests, absent for notifications. May be a number or string
    /// per JSON-RPC; we echo it back verbatim.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl RpcRequest {
    /// True when this is a notification (no `id` ⇒ no response expected).
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// A JSON-RPC success response.
#[derive(Debug, Clone, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub result: Value,
}

/// A JSON-RPC error response.
#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub error: RpcErrorBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcErrorBody {
    pub code: i64,
    pub message: String,
}

/// Standard JSON-RPC error codes we use.
pub mod codes {
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// Build a success response for `id`.
pub fn success(id: Value, result: Value) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result,
    }
}

/// Build an error response for `id`.
pub fn error(id: Value, code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        jsonrpc: "2.0",
        id,
        error: RpcErrorBody {
            code,
            message: message.into(),
        },
    }
}

/// Either kind of outbound message, so the server loop can return one type.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Outbound {
    Ok(RpcResponse),
    Err(RpcError),
}

impl Outbound {
    /// Serialize to a single NDJSON line (no trailing newline; the writer adds
    /// it). Falls back to a hand-written internal-error line if serialization
    /// somehow fails (it shouldn't for these types).
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"serialize failed"}}"#
                .to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_request_with_id() {
        let req: RpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert_eq!(req.method, "tools/list");
        assert!(!req.is_notification());
        assert_eq!(req.id, Some(json!(1)));
    }

    #[test]
    fn parses_notification_without_id() {
        let req: RpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(req.is_notification());
    }

    #[test]
    fn success_line_shape() {
        let line = Outbound::Ok(success(json!(7), json!({"ok": true}))).to_line();
        assert!(line.contains("\"id\":7"));
        assert!(line.contains("\"result\""));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn error_line_shape() {
        let line = Outbound::Err(error(json!("x"), codes::METHOD_NOT_FOUND, "nope")).to_line();
        assert!(line.contains("\"code\":-32601"));
        assert!(line.contains("\"message\":\"nope\""));
    }
}
