//! Stdio MCP adapter. Talks to the live sit mailbox. Does not spawn a window.

use crate::capabilities::MCP_TOOL_NAMES;
use crate::document::ApplyRequest;
use crate::mailbox::{HistoryAction, MailboxClient, MailboxRequest, ProjectAction};
use crate::query::QuerySpec;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Write};

const PROTOCOL: &str = "2024-11-05";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(default)]
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

pub fn serve_stdio(mailbox: &str) -> io::Result<()> {
    let client = MailboxClient::connect_sit(mailbox).map_err(|e| {
        io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("mcp: sit mailbox {mailbox} not up ({e}); start /thinner-floor first"),
        )
    })?;
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout();
    loop {
        let Some(raw) = read_message(&mut reader)? else {
            break;
        };
        if raw.trim().is_empty() {
            continue;
        }
        let req: RpcRequest = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                write_rpc(
                    &mut stdout,
                    None,
                    None,
                    Some(RpcError {
                        code: -32700,
                        message: format!("parse error: {e}"),
                    }),
                )?;
                continue;
            }
        };
        if req.method.starts_with("notifications/") || req.id.is_none() {
            if req.method == "notifications/initialized" {
                continue;
            }
            continue;
        }
        let (result, err) = dispatch(&client, &req.method, req.params);
        write_rpc(&mut stdout, req.id, result, err)?;
    }
    Ok(())
}

fn dispatch(client: &MailboxClient, method: &str, params: Value) -> (Option<Value>, Option<RpcError>) {
    match method {
        "initialize" => (
            Some(json!({
                "protocolVersion": PROTOCOL,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "thinner-floor", "version": "0.1.0" },
                "instructions": "First tool call must be three_studio_status. Live capabilities win over docs. Start the wgpu sit before this adapter."
            })),
            None,
        ),
        "ping" => (Some(json!({})), None),
        "tools/list" => (Some(json!({ "tools": tool_defs() })), None),
        "tools/call" => match call_tool(client, &params) {
            Ok(v) => (Some(v), None),
            Err(e) => (
                Some(json!({
                    "content": [{ "type": "text", "text": e }],
                    "isError": true
                })),
                None,
            ),
        },
        other => (
            None,
            Some(RpcError {
                code: -32601,
                message: format!("method not found: {other}"),
            }),
        ),
    }
}

fn tool_defs() -> Vec<Value> {
    MCP_TOOL_NAMES
        .iter()
        .map(|name| {
            json!({
                "name": name,
                "description": tool_description(name),
                "inputSchema": { "type": "object", "additionalProperties": true }
            })
        })
        .collect()
}

fn tool_description(name: &str) -> &'static str {
    match name {
        "three_studio_status" => "Session authority: revision, paths, capabilities. Call first.",
        "three_studio_project" => "list / save / open / create. create needs path; writes bootstrap JSON; refuses if file exists.",
        "three_studio_inspect" => "slice=summary|full. Optional query: left_of, color_of, on_screen, assembly_of.",
        "three_studio_apply" => "One labelled apply: baseRevision, idempotencyKey, label, changes[], optional dryRun. $id aliases.",
        "three_studio_validate" => "No mutation. Duplicate ids, parent/graph refs, recipes, parent cycles.",
        "three_studio_play" => "Playhead clock only (enter/stop/pause/seek/step). Not gameplay.",
        "three_studio_render" => "Offscreen authored-camera beauty PNG. No document mutation. width/height cap 1920x1080.",
        "three_studio_history" => "action=list|undo|redo. Undo/redo create NEW revisions.",
        "three_studio_job" => "Reserved. capabilities.jobs = false.",
        _ => "",
    }
}

fn call_tool(client: &MailboxClient, params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("tools/call needs name")?;
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let payload = match name {
        "three_studio_status" => mailbox_json(client, &MailboxRequest::Status)?,
        "three_studio_inspect" => inspect_tool(client, &args)?,
        "three_studio_apply" => apply_tool(client, &args)?,
        "three_studio_project" => project_tool(client, &args)?,
        "three_studio_validate" => validate_tool(client)?,
        "three_studio_history" => history_tool(client, &args)?,
        "three_studio_render" => render_tool(client, &args)?,
        "three_studio_play" => play_tool(client, &args)?,
        "three_studio_job" => {
            let cap = name.trim_start_matches("three_studio_");
            json!({
                "ok": false,
                "error": "not_supported",
                "code": "not_supported",
                "capability": cap
            })
        }
        other => {
            return Err(format!(
                "tool_contract_mismatch: unknown tool {other}; live tools are {:?}",
                MCP_TOOL_NAMES
            ))
        }
    };
    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into()) }],
        "isError": payload.get("ok").and_then(|v| v.as_bool()) == Some(false)
    }))
}

fn mailbox_json(client: &MailboxClient, req: &MailboxRequest) -> Result<Value, String> {
    let resp = client.request(req).map_err(|e| format!("mailbox: {e}"))?;
    serde_json::to_value(&resp).map_err(|e| e.to_string())
}

fn inspect_tool(client: &MailboxClient, args: &Value) -> Result<Value, String> {
    if let Some(q) = args.get("query") {
        let spec: QuerySpec = serde_json::from_value(q.clone()).map_err(|e| format!("bad query: {e}"))?;
        return mailbox_json(client, &MailboxRequest::Query { query: spec });
    }
    let slice = args
        .get("slice")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    mailbox_json(client, &MailboxRequest::Inspect { slice })
}

fn apply_tool(client: &MailboxClient, args: &Value) -> Result<Value, String> {
    let raw = serde_json::to_vec(args).map_err(|e| e.to_string())?;
    if raw.len() > MAX_REQUEST_BYTES {
        return Err(format!("apply over 1 MiB ({})", raw.len()));
    }
    let request: ApplyRequest =
        serde_json::from_value(args.clone()).map_err(|e| format!("bad apply: {e}"))?;
    mailbox_json(client, &MailboxRequest::Apply { request })
}

fn project_tool(client: &MailboxClient, args: &Value) -> Result<Value, String> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("list");
    match action {
        "list" => mailbox_json(
            client,
            &MailboxRequest::Project {
                action: ProjectAction::List,
                path: None,
            },
        ),
        "save" => mailbox_json(
            client,
            &MailboxRequest::Project {
                action: ProjectAction::Save,
                path: None,
            },
        ),
        "open" | "create" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let pa = if action == "create" {
                ProjectAction::Create
            } else {
                ProjectAction::Open
            };
            mailbox_json(
                client,
                &MailboxRequest::Project {
                    action: pa,
                    path,
                },
            )
        }
        other => Err(format!("unknown project action: {other}")),
    }
}

fn play_tool(client: &MailboxClient, args: &Value) -> Result<Value, String> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("stop")
        .to_string();
    let time = args.get("time").and_then(|v| v.as_f64()).map(|t| t as f32);
    mailbox_json(client, &MailboxRequest::Play { action, time })
}

fn render_tool(client: &MailboxClient, args: &Value) -> Result<Value, String> {
    let width = args.get("width").and_then(|v| v.as_u64()).map(|n| n as u32);
    let height = args.get("height").and_then(|v| v.as_u64()).map(|n| n as u32);
    mailbox_json(client, &MailboxRequest::Render { width, height })
}

fn history_tool(client: &MailboxClient, args: &Value) -> Result<Value, String> {
    let action = match args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("list")
    {
        "list" => HistoryAction::List,
        "undo" => HistoryAction::Undo,
        "redo" => HistoryAction::Redo,
        other => return Err(format!("history action list|undo|redo, got {other}")),
    };
    mailbox_json(client, &MailboxRequest::History { action })
}

fn validate_tool(client: &MailboxClient) -> Result<Value, String> {
    mailbox_json(client, &MailboxRequest::Validate)
}

fn write_rpc<W: Write>(
    out: &mut W,
    id: Option<Value>,
    result: Option<Value>,
    error: Option<RpcError>,
) -> io::Result<()> {
    let mut body = serde_json::Map::new();
    body.insert("jsonrpc".into(), json!("2.0"));
    if let Some(id) = id {
        body.insert("id".into(), id);
    }
    if let Some(err) = error {
        body.insert("error".into(), serde_json::to_value(err).unwrap());
    } else if let Some(result) = result {
        body.insert("result".into(), result);
    }
    let payload = Value::Object(body).to_string();
    write!(out, "Content-Length: {}\r\n\r\n{}", payload.len(), payload)?;
    out.flush()
}

fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut first = String::new();
    let n = reader.read_line(&mut first)?;
    if n == 0 {
        return Ok(None);
    }
    let trimmed = first.trim_end();
    if trimmed.starts_with('{') {
        return Ok(Some(trimmed.to_string()));
    }
    let mut content_length: Option<usize> = None;
    let mut line = first;
    loop {
        let t = line.trim_end().trim_start_matches('\u{feff}');
        if t.is_empty() {
            break;
        }
        if let Some(rest) = t.strip_prefix("Content-Length:") {
            content_length = Some(
                rest.trim()
                    .parse()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            );
        }
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
    }
    let len = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "mcp: missing Content-Length")
    })?;
    if len > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mcp: request over 1 MiB",
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    String::from_utf8(buf).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn initialize_lists_protocol() {
        let (result, err) = (
            Some(json!({
                "protocolVersion": PROTOCOL,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "thinner-floor", "version": "0.1.0" }
            })),
            None::<RpcError>,
        );
        assert!(err.is_none());
        let v = result.unwrap();
        assert_eq!(v["protocolVersion"], PROTOCOL);
        assert_eq!(v["serverInfo"]["name"], "thinner-floor");
    }

    #[test]
    fn tools_list_has_nine() {
        assert_eq!(tool_defs().len(), 9);
        let defs = tool_defs();
        let names: Vec<_> = defs
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"three_studio_status"));
        assert!(names.contains(&"three_studio_job"));
    }

    #[test]
    fn reads_json_line_and_content_length() {
        let line = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n";
        let mut c = Cursor::new(line.as_bytes());
        let got = read_message(&mut c).unwrap().unwrap();
        assert!(got.contains("initialize"));

        let payload = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}";
        let framed = format!("Content-Length: {}\r\n\r\n{}", payload.len(), payload);
        let mut c2 = Cursor::new(framed.as_bytes());
        let got2 = read_message(&mut c2).unwrap().unwrap();
        assert!(got2.contains("ping"));
    }
}
