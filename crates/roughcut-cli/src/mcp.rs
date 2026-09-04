//! The same commands, served over MCP on standard input and output.
//!
//! There is no second definition of anything here. The tool list is generated
//! from `spec::COMMANDS`, a call is validated by the same `Args`, and the work
//! is done by the same `ops::run`. What this file knows that the command line
//! does not is JSON-RPC framing, and that is all it knows.

use crate::ops;
use crate::spec::{self, Kind};
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};

/// Protocol revisions this server behaves correctly under. The newest is
/// offered when a client asks for something not on the list.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// A command, as an MCP tool.
fn tool(cmd: &spec::Cmd) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for p in cmd.params {
        let mut schema = match p.kind {
            Kind::Int => json!({ "type": "integer" }),
            Kind::Bool => json!({ "type": "boolean" }),
            Kind::Choice(all) => json!({ "type": "string", "enum": all }),
            Kind::Text | Kind::Path => json!({ "type": "string" }),
        };
        if p.many {
            schema = json!({ "type": "array", "items": schema });
        }
        schema
            .as_object_mut()
            .expect("built as an object")
            .insert("description".into(), Value::String(p.help.to_string()));
        properties.insert(p.name.to_string(), schema);
        if p.required {
            required.push(Value::String(p.name.to_string()));
        }
    }
    json!({
        "name": cmd.name,
        "description": cmd.help,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        },
        "annotations": {
            "readOnlyHint": !cmd.writes,
            // Every edit is reversible by another edit and nothing is deleted
            // from disk — except `rotate`, which rewrites a source file, and
            // `render`, which writes one.
            "destructiveHint": matches!(cmd.name, "rotate"),
        },
    })
}

fn tools() -> Value {
    json!({ "tools": spec::COMMANDS.iter().map(tool).collect::<Vec<_>>() })
}

/// Serve until standard input closes.
pub fn serve() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        if stdin.lock().read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(&line) {
            serde_json::to_writer(&mut stdout, &reply)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
}

/// One request in, at most one response out.
///
/// `None` for a notification, which by the protocol is never answered — and
/// answering one is the mistake that makes a client hang up on you.
pub fn handle(line: &str) -> Option<Value> {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        // No id to answer with, so this is as far as it goes.
        Err(e) => return Some(error(Value::Null, -32700, &format!("unparseable JSON: {e}"))),
    };
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    let Some(id) = id else {
        // A notification. `notifications/initialized` is the one that matters
        // and there is nothing to do about it.
        return None;
    };

    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked
                .filter(|v| PROTOCOL_VERSIONS.contains(v))
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            Some(ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "roughcut", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }),
            ))
        }
        "ping" => Some(ok(id, json!({}))),
        "tools/list" => Some(ok(id, tools())),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let fields = match params.get("arguments") {
                Some(Value::Object(map)) => map.clone(),
                None | Some(Value::Null) => Map::new(),
                Some(other) => {
                    return Some(ok(id, failed(&format!("arguments must be an object, not {other}"))))
                }
            };
            // A failed command is a result with `isError`, not a protocol
            // error: the model is supposed to read what went wrong and try
            // again, and a JSON-RPC error is not shown to it.
            Some(ok(
                id,
                match ops::call(&name, fields) {
                    Ok(value) => content(&pretty(&value), false),
                    Err(e) => failed(&format!("{e:#}")),
                },
            ))
        }
        // Declared unsupported in `initialize`, but clients ask anyway.
        "resources/list" => Some(ok(id, json!({ "resources": [] }))),
        "prompts/list" => Some(ok(id, json!({ "prompts": [] }))),
        other => Some(error(id, -32601, &format!("no method called {other}"))),
    }
}

const INSTRUCTIONS: &str = "\
Roughcut assembles a rough cut from video and photographs and exports MLT XML \
for Shotcut, or an MP4. This server edits a .roughcut project file directly.

Positions are frame numbers everywhere — never seconds, never timecode. \
Frame ranges are inclusive: a range from 100 to 199 is 100 frames long. Output \
carries a timecode beside each frame for reading, but only the frame number is \
ever accepted back.

Set ROUGHCUT_PROJECT, or pass `project` to every call. Start with `info` to see \
what is there. `clips` lists the bin; a clip can be named by any unambiguous \
part of its file name rather than by id. `search` finds spoken words across the \
transcripts already made and hands back the exact frames to cut, which `cut-words` \
then puts on the timeline. Group several edits into `batch` so they are written \
back once and cannot half-apply.

If the project is open in the Roughcut window at the same time, it notices the \
file changing and offers to reload.";

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

fn content(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn failed(message: &str) -> Value {
    content(message, true)
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, params: Value) -> Value {
        let line = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        handle(&line.to_string()).expect("a request with an id is always answered")
    }

    #[test]
    fn initialize_agrees_on_a_version_the_client_knows() {
        let r = call("initialize", json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        // And offers its own when the client asks for something else.
        let r = call("initialize", json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        assert!(r["result"]["instructions"].as_str().unwrap().contains("frame"));
    }

    /// A notification carries no id and must go unanswered. Replying to one is
    /// what makes a client decide the server is broken and hang up.
    #[test]
    fn a_notification_is_not_answered() {
        let line = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle(&line.to_string()).is_none());
    }

    #[test]
    fn every_command_is_offered_as_a_tool() {
        let r = call("tools/list", Value::Null);
        let listed = r["result"]["tools"].as_array().unwrap();
        assert_eq!(listed.len(), spec::COMMANDS.len());
        for cmd in spec::COMMANDS {
            let tool = listed
                .iter()
                .find(|t| t["name"] == cmd.name)
                .unwrap_or_else(|| panic!("{} is not offered", cmd.name));
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["annotations"]["readOnlyHint"], !cmd.writes);
            for p in cmd.params {
                let schema = &tool["inputSchema"]["properties"][p.name];
                assert!(!schema.is_null(), "{}.{} is missing", cmd.name, p.name);
                assert_eq!(schema["description"], p.help);
            }
            let required = tool["inputSchema"]["required"].as_array().unwrap();
            let want = cmd.params.iter().filter(|p| p.required).count();
            assert_eq!(required.len(), want, "{} lists the wrong required set", cmd.name);
        }
    }

    #[test]
    fn a_frame_number_is_declared_as_an_integer_not_a_string() {
        let r = call("tools/list", Value::Null);
        let listed = r["result"]["tools"].as_array().unwrap();
        let split = listed.iter().find(|t| t["name"] == "split").unwrap();
        assert_eq!(split["inputSchema"]["properties"]["at"]["type"], "integer");
        let import = listed.iter().find(|t| t["name"] == "import").unwrap();
        assert_eq!(import["inputSchema"]["properties"]["path"]["type"], "array");
    }

    /// A command that fails has to come back as a readable result, not a
    /// JSON-RPC error — an error is a transport failure and never reaches the
    /// model that has to correct it.
    #[test]
    fn a_failed_call_is_a_result_the_model_can_read() {
        let r = call("tools/call", json!({ "name": "split", "arguments": { "at": "soon" } }));
        assert!(r["error"].is_null(), "{r}");
        assert_eq!(r["result"]["isError"], true);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("frames"), "{text}");
    }

    #[test]
    fn an_unknown_tool_says_so_without_dropping_the_connection() {
        let r = call("tools/call", json!({ "name": "sharpen", "arguments": {} }));
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("sharpen"));
    }

    #[test]
    fn an_unknown_method_is_a_protocol_error() {
        let r = call("tools/eat", Value::Null);
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn unparseable_input_does_not_panic() {
        let r = handle("{ not json").unwrap();
        assert_eq!(r["error"]["code"], -32700);
    }
}
