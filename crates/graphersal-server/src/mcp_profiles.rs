//! MCP profiles (MCPPROF): what an agent sees on an MCP endpoint (the tools offered, their
//! descriptions, the instructions, saved queries as tools of their own, the resources, the
//! server name, the answer limit), per database or alias name and as one server-wide default.
//!
//! **The format** is the graphersal CLI's `--mcp-config FILE` (graphersal-mcp's
//! [`McpConfig`]): `tools.include` / `tools.exclude` / `tools.overrides`, `instructions` (`mode`
//! `builtin` | `append` | `replace` with its `text`), `saved_query_tools`, `resources`,
//! `server_name`, `server_title`, `answer_limit_chars`, `descriptions` (only `"short"`), keys
//! starting with `_` as comments. The server keeps the instructions INSIDE the profile: an
//! instructions `file` is refused when a profile is saved ([`check`]); the console's `mcp
//! profile set FILE` and the web UI's import put the file's text into `instructions.text`
//! ([`inline_instructions_file`]).
//!
//! **Where they live**: `mcp.toml` ([`crate::mcp_control`]), a name's profile under
//! `[databases.<name>.profile]`, the server default under `[default_profile]`; so a renamed
//! alias carries its profile, a deleted database or removed alias loses it, and a server
//! backup carries them all.
//!
//! **Which profile applies** ([`crate::mcp_control::McpControl::profile_for`]), whole profiles,
//! never merged key by key: on `/db/<name>/mcp` the name's own profile, else (an alias) its
//! database's, else the server default, else the built-in one (no profile: every tool of the
//! mode with its built-in description, the built-in instructions). On `/mcp` the server default
//! (else the built-in one) for everything: its instructions, `tools/list` and `resources/list`
//! ([`rewrite_directory_answer`]) and the tools a call may use; saved queries are never tools of
//! their own on `/mcp` (graphersal-mcp's directory mode), whatever the profile says.
//!
//! **When a change applies**: at the next request of every endpoint (every request is served
//! under the profile of that moment), also inside an open MCP session; a client that keeps the
//! tool list of its `initialize` sees the new list after it reconnects (graphersal-mcp pushes
//! `notifications/tools/list_changed` only over stream connections, which the server does not
//! serve).
//!
//! **Access**: the profile says what is offered, the caller's effective level what may run. A
//! caller that may only read (a `read` grant, a read-only database or base, MCP read-only) gets
//! only the reading tools a profile names: writing tools are simply not offered (never an
//! error at request time); [`warnings`] tells the admin when saving.
//!
//! **The built-in texts** ([`builtin`]) and the **preview** ([`preview_database`],
//! [`preview_directory`]) come from graphersal-mcp itself: a throwaway endpoint with the
//! profile answers `initialize`, `tools/list` and `resources/list` exactly as the real one
//! would, over the database's own context when it is loaded (its saved queries), else over an
//! empty graph.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use graphersal_mcp::{
    GraphDirectory, GraphEntry, Host, MAX_INSTRUCTIONS_CHARS, MIN_ANSWER_CHARS, Mcp, McpConfig,
    McpError, McpRequest, McpResponse, Profile,
};
use serde_json::{Value, json};

/// The largest profile in bytes as JSON text (instructions and descriptions are at most
/// [`MAX_INSTRUCTIONS_CHARS`] each; this bounds `mcp.toml`).
pub(crate) const MAX_PROFILE_BYTES: usize = 1024 * 1024;

/// The help of a refused profile.
pub(crate) const PROFILE_HELP: &str = "a profile is the JSON of graphersal's --mcp-config file \
     (tools.include, tools.overrides, instructions {mode, text}, saved_query_tools, resources, \
     server_name, server_title, answer_limit_chars); GET /api/admin/mcp/builtin lists the tools \
     and resources; the book's MCP page, \"Profiles\", has the details";

/// The protocol revision the previews initialize with.
const PREVIEW_PROTOCOL: &str = "2025-06-18";

/// The `read_file` callback of [`McpConfig::parse`]: the server keeps no instructions file.
fn no_files(path: &str) -> Result<String, String> {
    Err(format!(
        "the server keeps the instructions inside the profile, not in a file ({path:?}): write \
         them as \"instructions\": {{\"mode\": ..., \"text\": \"...\"}} (the console's `mcp \
         profile set FILE` and the web UI's import put the file's text there)"
    ))
}

/// `value` without JSON nulls (TOML has none; graphersal-mcp takes none either, a comment key
/// aside).
pub(crate) fn without_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, without_nulls(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .filter(|v| !v.is_null())
                .map(without_nulls)
                .collect(),
        ),
        other => other,
    }
}

/// The configuration of the profile `profile` (JSON, the `--mcp-config` format, instructions
/// inline): `Err` with graphersal-mcp's message (the key named, a did-you-mean).
pub(crate) fn config_of(profile: &Value) -> Result<McpConfig, String> {
    if !profile.is_object() {
        return Err(
            "a profile is a JSON object (the format of graphersal's --mcp-config file)".to_string(),
        );
    }
    McpConfig::parse(&profile.to_string(), &mut no_files).map_err(|err| err.message)
}

/// Checks a profile before it is saved: the stored form (nulls dropped) and its configuration.
/// `Err`: what is wrong (a structural mistake with graphersal-mcp's message, an instructions
/// `file`, a profile over [`MAX_PROFILE_BYTES`]).
pub(crate) fn check(profile: Value) -> Result<(Value, McpConfig), String> {
    let profile = without_nulls(profile);
    let bytes = profile.to_string().len();
    if bytes > MAX_PROFILE_BYTES {
        return Err(format!(
            "the profile has {bytes} bytes as JSON: at most {MAX_PROFILE_BYTES}"
        ));
    }
    let config = config_of(&profile)?;
    Ok((profile, config))
}

/// Puts the text of an instructions `file` the profile names into `instructions.text`, reading
/// it through `read` (the console resolves it against the configuration file's directory):
/// whether there was one.
///
/// `Err`: the file cannot be read (the message names it), or `instructions` names both a `file`
/// and a `text`.
pub(crate) fn inline_instructions_file(
    profile: &mut Value,
    read: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<bool, String> {
    let Some(instructions) = profile
        .get_mut("instructions")
        .and_then(Value::as_object_mut)
    else {
        return Ok(false);
    };
    let Some(file) = instructions.get("file").and_then(Value::as_str) else {
        return Ok(false);
    };
    if instructions.contains_key("text") {
        return Err("\"instructions\" takes \"text\" or \"file\", not both".to_string());
    }
    let file = file.to_string();
    let text = read(&file).map_err(|err| format!("\"instructions.file\" {file}: {err}"))?;
    instructions.remove("file");
    instructions.insert("text".to_string(), Value::String(text));
    Ok(true)
}

/// [`inline_instructions_file`] for a configuration file read from `config_path`: a relative
/// instructions `file` is resolved against the configuration file's directory (as the
/// graphersal CLI does).
pub(crate) fn inline_from_disk(profile: &mut Value, config_path: &Path) -> Result<bool, String> {
    let base = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    inline_instructions_file(profile, &mut |file| {
        let path = Path::new(file);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        };
        std::fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))
    })
}

/// The tools a profile names in `tools.include` that only a writer gets: the reasons a reader
/// sees fewer tools than the profile names. `read_tools`: the names offered to a reader
/// ([`builtin`]'s `readOnly` listing).
fn writing_tools_named(profile: &Value, read_tools: &[String]) -> Vec<String> {
    profile
        .pointer("/tools/include")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|name| !read_tools.iter().any(|t| t == name))
        .map(str::to_string)
        .collect()
}

/// What an admin should know about a valid profile before relying on it: writing tools that
/// callers with read access never get (always so on a read-only database: `read_only`), saved
/// queries as tools on the server-wide endpoint (`server_wide`, where they are never tools).
pub(crate) fn warnings(profile: &Value, read_only: bool, server_wide: bool) -> Vec<String> {
    let mut out = Vec::new();
    let read_tools = reading_tool_names();
    let writing = writing_tools_named(profile, &read_tools);
    if !writing.is_empty() {
        let names = writing.join(", ");
        out.push(if read_only {
            format!(
                "the database is read-only for MCP, so its agents never get the writing tools \
                 the profile names: {names}"
            )
        } else {
            format!(
                "agents that may only read (a read grant, MCP read-only) do not get the writing \
                 tools the profile names: {names}"
            )
        });
    }
    if server_wide && profile.get("saved_query_tools").is_some() {
        out.push(
            "saved_query_tools has no effect on /mcp (saved queries are never tools of their \
             own there: agents run them with run_saved_query); it applies on /db/<name>/mcp to \
             the names without a profile of their own"
                .to_string(),
        );
    }
    out
}

/// A directory without graphs: the previews and the built-in texts of `/mcp` need none.
struct NoGraphs;

impl GraphDirectory for NoGraphs {
    fn list(&self) -> Result<Vec<GraphEntry>, McpError> {
        Ok(Vec::new())
    }

    fn resolve(&self, name: &str) -> Result<String, McpError> {
        Err(McpError::Failed(format!("no database or alias '{name}'")))
    }

    fn host(&self, route: &str) -> Result<(Arc<dyn Host>, Profile), McpError> {
        Err(McpError::Failed(format!("no database or alias '{route}'")))
    }
}

/// One JSON-RPC request through `serve` (an endpoint's `handle*`), in the MCP session
/// `session`: the new session id (an `initialize`) and the `result`. `Err`: the endpoint's
/// error text.
fn rpc(
    serve: &dyn Fn(McpRequest<'_>) -> McpResponse,
    id: u64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Result<(Option<String>, Value), String> {
    let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    let mut request = McpRequest::new("POST", body.as_bytes())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(session) = session {
        request = request
            .header("Mcp-Session-Id", session)
            .header("MCP-Protocol-Version", PREVIEW_PROTOCOL);
    }
    let answer = serve(request);
    let session = answer.header("Mcp-Session-Id").map(str::to_string);
    let value: Value = serde_json::from_slice(answer.body())
        .map_err(|err| format!("{method}: the endpoint's answer is not JSON: {err}"))?;
    if let Some(error) = value.get("error") {
        return Err(format!(
            "{method}: {}",
            error["message"].as_str().unwrap_or("failed")
        ));
    }
    Ok((session, value["result"].clone()))
}

/// The `initialize` params of a preview.
fn initialize_params() -> Value {
    json!({
        "protocolVersion": PREVIEW_PROTOCOL,
        "capabilities": {},
        "clientInfo": {"name": "graphersal-server preview", "version": env!("CARGO_PKG_VERSION")},
    })
}

/// A throwaway endpoint serving `profile` by default (no sessions kept beyond it).
fn throwaway(profile: Profile) -> Mcp {
    Mcp::new(false, None, false).with_profile(profile)
}

/// What an agent sees on a `/db/<name>/mcp` endpoint served by `host` under `config` (`None`:
/// the built-in profile), `read_only` or not: `{instructions, serverInfo, tools, resources}`,
/// answered by graphersal-mcp itself.
///
/// `Err`: the endpoint's error (e.g. the host is busy).
pub(crate) fn preview_database(
    config: Option<McpConfig>,
    host: &dyn Host,
    read_only: bool,
) -> Result<Value, String> {
    let profile = Profile::new(read_only).with_config(config.unwrap_or_default());
    let mcp = throwaway(profile.clone());
    let serve = |request: McpRequest<'_>| mcp.handle_with_profile(host, &profile, request);
    let (session, init) = rpc(&serve, 1, "initialize", initialize_params(), None)?;
    let session = session.as_deref();
    let (_, tools) = rpc(&serve, 2, "tools/list", json!({}), session)?;
    let (_, resources) = rpc(&serve, 3, "resources/list", json!({}), session)?;
    Ok(json!({
        "instructions": init["instructions"],
        "serverInfo": init["serverInfo"],
        "tools": tools["tools"],
        "resources": resources["resources"],
    }))
}

/// What an agent sees on `/mcp` under `config` (`None`: the built-in profile): `{instructions,
/// serverInfo, tools, resources}` of graphersal-mcp's directory mode (the same list for every
/// caller: writing tools on a database the caller may only read are refused when called).
pub(crate) fn preview_directory(config: Option<McpConfig>) -> Result<Value, String> {
    let mcp = throwaway(Profile::new(false).with_config(config.unwrap_or_default()));
    let serve = |request: McpRequest<'_>| mcp.handle_directory(&NoGraphs, request);
    let (session, init) = rpc(&serve, 1, "initialize", initialize_params(), None)?;
    let session = session.as_deref();
    let (_, tools) = rpc(&serve, 2, "tools/list", json!({}), session)?;
    let (_, resources) = rpc(&serve, 3, "resources/list", json!({}), session)?;
    Ok(json!({
        "instructions": init["instructions"],
        "serverInfo": init["serverInfo"],
        "tools": tools["tools"],
        "resources": resources["resources"],
    }))
}

/// A context over an empty graph, the host of the previews of a database that is not loaded
/// (built once; `Err` when it cannot be launched).
pub(crate) fn empty_host() -> Result<Arc<dyn Host>, String> {
    static EMPTY: OnceLock<Result<Arc<graphersal_serve::Context>, String>> = OnceLock::new();
    let context = EMPTY.get_or_init(|| {
        let (launched, ()) = graphersal_serve::launch(
            graphersal_serve::ServedSource::Sample("empty".to_string()),
            graphersal_session::SessionPresets::default(),
            |_| Ok::<(), String>(()),
            None,
        )
        .map_err(|err| format!("the preview's empty graph cannot be served: {err}"))?;
        let check = graphersal_serve::HostCheck::new(
            graphersal_serve::Exposure::Loopback,
            0,
            "http://127.0.0.1:0/",
        );
        Ok(launched.context_builder(check).build())
    });
    match context {
        Ok(context) => {
            let host: Arc<dyn Host> = context.clone();
            Ok(host)
        }
        Err(message) => Err(message.clone()),
    }
}

/// The saved queries of a loaded database's `context` as `[{name, folder, description,
/// writes}]` (the profile editor's picker); `Err`: the session's error.
pub(crate) fn saved_queries(context: &graphersal_serve::Context) -> Result<Vec<Value>, String> {
    let list = context
        .call_fallible(|session| session.list_queries(None))
        .map_err(|err| err.to_string())?;
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let name = entry["name"].as_str()?;
            let folder = entry["folder"].as_str().filter(|f| !f.is_empty());
            Some(json!({
                "name": name,
                "folder": folder,
                "description": entry["description"].as_str().filter(|d| !d.is_empty()),
                "writes": entry["writes"].as_bool().unwrap_or(false),
            }))
        })
        .collect())
}

/// The names of the built-in tools offered to a reader.
fn reading_tool_names() -> Vec<String> {
    builtin_tools()
        .iter()
        .filter(|tool| tool.read_description.is_some())
        .map(|tool| tool.name.clone())
        .collect()
}

/// One built-in tool as the editor shows it.
#[derive(Clone, Debug)]
struct BuiltinTool {
    name: String,
    title: String,
    description: String,
    /// The description a reader gets (`None`: not offered to readers).
    read_description: Option<String>,
    /// Whether the tool may change data (not marked read-only).
    writes: bool,
    /// Whether it is offered without `tools.include` naming it.
    default: bool,
}

/// The built-in tools (computed once from graphersal-mcp's own listings).
fn builtin_tools() -> &'static [BuiltinTool] {
    static TOOLS: OnceLock<Vec<BuiltinTool>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        let listing = |config: McpConfig, read_only: bool| {
            throwaway(Profile::new(read_only).with_config(config)).directory_tool_listings()
        };
        // The directory's own two tools are not built-in data tools.
        let own =
            |tool: &&Value| !matches!(tool["name"].as_str(), Some("list_graphs" | "use_graph"));
        let defaults: Vec<Value> = listing(McpConfig::default(), false);
        let mut names: Vec<String> = defaults
            .iter()
            .filter(own)
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        // The opt-in tools (offered only when `tools.include` names them): graphersal-mcp
        // 0.9 has one; asked by name, so a version without it simply has none.
        for opt_in in OPT_IN_TOOLS {
            let mut all = names.clone();
            all.push(opt_in.to_string());
            if config_of(&json!({"tools": {"include": all}})).is_ok() {
                names.push(opt_in.to_string());
            }
        }
        let all = config_of(&json!({"tools": {"include": names}})).unwrap_or_default();
        let writer = listing(all.clone(), false);
        let reader = listing(all, true);
        writer
            .iter()
            .filter(own)
            .filter_map(|tool| {
                let name = tool["name"].as_str()?.to_string();
                let read_description = reader
                    .iter()
                    .find(|t| t["name"] == tool["name"])
                    .and_then(|t| t["description"].as_str())
                    .map(str::to_string);
                Some(BuiltinTool {
                    default: defaults.iter().any(|t| t["name"] == tool["name"]),
                    title: tool["title"].as_str().unwrap_or_default().to_string(),
                    description: tool["description"].as_str().unwrap_or_default().to_string(),
                    writes: tool["annotations"]["readOnlyHint"] != json!(true),
                    read_description,
                    name,
                })
            })
            .collect()
    })
}

/// The built-in tools offered only when `tools.include` names them (asked of graphersal-mcp
/// one by one: a name it does not know is left out).
const OPT_IN_TOOLS: [&str; 1] = ["usage_statistics"];

/// The built-in texts and limits a profile starts from (`GET /api/admin/mcp/builtin`):
/// `instructions` (graphersal-mcp's built-in text), `directoryInstructions` (what `/mcp` puts
/// before it), `tools` (`[{name, title, description, readDescription (null: not offered to a
/// reader), writes, default}]`), `resources` (`[{name, title, description, uri}]`),
/// `serverName`, `serverTitle`, `answerLimitChars` (the default), `minAnswerChars`,
/// `maxInstructionsChars`, `version` (graphersal-mcp's).
pub(crate) fn builtin() -> Value {
    static BUILTIN: OnceLock<Value> = OnceLock::new();
    BUILTIN
        .get_or_init(|| {
            let directory = preview_directory(None).unwrap_or_else(|_| json!({}));
            let base = graphersal_mcp::instructions();
            let directory_text = directory["instructions"].as_str().unwrap_or_default();
            let prefix = directory_text
                .strip_suffix(base)
                .map(|p| p.trim_end().to_string())
                .unwrap_or_default();
            let tools: Vec<Value> = builtin_tools()
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "title": t.title,
                        "description": t.description,
                        "readDescription": t.read_description,
                        "writes": t.writes,
                        "default": t.default,
                    })
                })
                .collect();
            let resources: Vec<Value> = directory["resources"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    json!({
                        "name": r["name"],
                        "title": r["title"],
                        "description": r["description"],
                        "uri": r["uri"],
                    })
                })
                .collect();
            json!({
                "instructions": base,
                "directoryInstructions": prefix,
                "tools": tools,
                "resources": resources,
                "serverName": directory["serverInfo"]["name"],
                "serverTitle": directory["serverInfo"]["title"],
                "version": directory["serverInfo"]["version"],
                "answerLimitChars": Profile::new(false).max_chars(),
                "minAnswerChars": MIN_ANSWER_CHARS,
                "maxInstructionsChars": MAX_INSTRUCTIONS_CHARS,
            })
        })
        .clone()
}

/// Rewrites the answer `body` of a `/mcp` request (its JSON-RPC request `sent`) for the server
/// default profile `config`: the `result` of `initialize`, `tools/list` and `resources/list` is
/// replaced with the one a throwaway endpoint serving `config` gives for the same request (the
/// real endpoint was built with the built-in profile and keeps its sessions); every other
/// answer, an error and a batch are left as they are. `None`: nothing to change.
pub(crate) fn rewrite_directory_answer(
    config: &McpConfig,
    sent: &[u8],
    body: &[u8],
) -> Option<Vec<u8>> {
    let request: Value = serde_json::from_slice(sent).ok()?;
    let method = request["method"].as_str()?;
    if !matches!(method, "initialize" | "tools/list" | "resources/list") {
        return None;
    }
    let mut answer: Value = serde_json::from_slice(body).ok()?;
    answer.get("result")?;
    let mcp = throwaway(Profile::new(false).with_config(config.clone()));
    let serve = |request: McpRequest<'_>| mcp.handle_directory(&NoGraphs, request);
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "initialize" => rpc(&serve, 1, method, params, None).ok()?.1,
        _ => {
            let (session, _) = rpc(&serve, 1, "initialize", initialize_params(), None).ok()?;
            rpc(&serve, 2, method, params, session.as_deref()).ok()?.1
        }
    };
    answer["result"] = result;
    serde_json::to_vec(&answer).ok()
}

/// The JSON of a profile's source for the admin API: `own`, `database` (an alias without its
/// own profile, served its database's), `default` (the server default) or `builtin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileSource {
    /// The name's own profile.
    Own,
    /// The profile of the alias's database.
    Database,
    /// The server-wide default profile.
    Default,
    /// No profile: graphersal-mcp's built-in one.
    Builtin,
}

impl ProfileSource {
    /// The word of the admin API and the console.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Database => "database",
            Self::Default => "default",
            Self::Builtin => "builtin",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_is_checked_with_graphersal_mcps_messages() {
        let err = check(json!({"tools": {"inclde": ["gql"]}})).unwrap_err();
        assert!(err.contains("tools.inclde"), "{err}");
        assert!(err.contains("did you mean \"tools.include\""), "{err}");
        let err = check(json!({"tools": {"include": ["gqll"]}})).unwrap_err();
        assert!(err.contains("did you mean \"gql\""), "{err}");
        let err = check(json!({"instructions": {"mode": "replace", "file": "x.md"}})).unwrap_err();
        assert!(err.contains("inside the profile"), "{err}");
        assert!(check(json!([1])).is_err());
        let (stored, config) = check(json!({
            "_comment": null,
            "server_name": "kb",
            "instructions": {"mode": "append", "text": "Notes."},
        }))
        .unwrap();
        assert_eq!(
            stored,
            json!({"server_name": "kb", "instructions": {"mode": "append", "text": "Notes."}})
        );
        assert!(!config.has_saved_query_tools());
        let big = "x".repeat(MAX_PROFILE_BYTES);
        assert!(check(json!({"_c": big})).unwrap_err().contains("at most"));
    }

    #[test]
    fn an_instructions_file_is_inlined_relative_to_the_configuration() {
        let temp = crate::test_support::TempDir::new("mcp-profile-inline");
        std::fs::write(temp.path().join("notes.md"), "Hello agents.\n").unwrap();
        let config = temp.path().join("mcp-config.json");
        let mut profile = json!({"instructions": {"mode": "replace", "file": "notes.md"}});
        assert!(inline_from_disk(&mut profile, &config).unwrap());
        assert_eq!(profile["instructions"]["text"], "Hello agents.\n");
        assert!(profile["instructions"].get("file").is_none());
        assert!(!inline_from_disk(&mut profile, &config).unwrap());
        let mut missing = json!({"instructions": {"mode": "replace", "file": "gone.md"}});
        let err = inline_from_disk(&mut missing, &config).unwrap_err();
        assert!(err.contains("gone.md"), "{err}");
    }

    #[test]
    fn the_builtin_texts_come_from_graphersal_mcp() {
        let builtin = builtin();
        assert_eq!(builtin["instructions"], graphersal_mcp::instructions());
        let tools = builtin["tools"].as_array().unwrap();
        let find = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap().clone();
        assert_eq!(find("gql")["default"], true);
        assert_eq!(find("usage_statistics")["default"], false);
        assert_eq!(find("set_schema")["writes"], true);
        assert!(find("set_schema")["readDescription"].is_null());
        assert!(find("gql")["readDescription"].is_string());
        assert_ne!(
            find("query")["description"],
            find("query")["readDescription"]
        );
        assert!(tools.iter().all(|t| t["name"] != "list_graphs"));
        assert_eq!(builtin["resources"].as_array().unwrap().len(), 7);
        assert!(
            builtin["directoryInstructions"]
                .as_str()
                .unwrap()
                .contains("use_graph")
        );
    }

    #[test]
    fn the_previews_answer_what_the_endpoint_answers() {
        let config = config_of(&json!({
            "server_name": "kb",
            "instructions": {"mode": "replace", "text": "Only this."},
            "tools": {"include": ["gql", "set_schema"], "overrides": {"gql": {"description": "Ask in GQL."}}},
        }))
        .unwrap();
        let host = empty_host().unwrap();
        let writer = preview_database(Some(config.clone()), host.as_ref(), false).unwrap();
        assert_eq!(writer["instructions"], "Only this.");
        assert_eq!(writer["serverInfo"]["name"], "kb");
        let names: Vec<&str> = writer["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, ["gql", "set_schema"]);
        assert_eq!(writer["tools"][0]["description"], "Ask in GQL.");
        let reader = preview_database(Some(config.clone()), host.as_ref(), true).unwrap();
        let names: Vec<&str> = reader["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, ["gql"], "a reader is not offered the writing tool");
        let directory = preview_directory(Some(config)).unwrap();
        let text = directory["instructions"].as_str().unwrap();
        assert!(
            text.contains("use_graph") && text.ends_with("Only this."),
            "{text}"
        );
        let names: Vec<&str> = directory["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, ["list_graphs", "use_graph", "gql", "set_schema"]);
        let warnings = warnings(
            &json!({"tools": {"include": ["gql", "set_schema"]}}),
            true,
            false,
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("set_schema"), "{warnings:?}");
    }

    #[test]
    fn the_directory_answer_is_rewritten_for_the_default_profile() {
        let config =
            config_of(&json!({"server_name": "acme", "tools": {"include": ["gql"]}})).unwrap();
        let real = Mcp::new(false, None, false);
        let sent = json!({"jsonrpc": "2.0", "id": 7, "method": "initialize", "params": initialize_params()}).to_string();
        let serve = |request: McpRequest<'_>| real.handle_directory(&NoGraphs, request);
        let post = |body: &str| {
            serve(
                McpRequest::new("POST", body.as_bytes()).header("Content-Type", "application/json"),
            )
        };
        let answer = post(&sent);
        let rewritten = rewrite_directory_answer(&config, sent.as_bytes(), answer.body()).unwrap();
        let value: Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(value["id"], 7);
        assert_eq!(value["result"]["serverInfo"]["name"], "acme");
        let list = json!({"jsonrpc": "2.0", "id": 8, "method": "tools/list"}).to_string();
        let answer = post(&list);
        let rewritten = rewrite_directory_answer(&config, list.as_bytes(), answer.body()).unwrap();
        let value: Value = serde_json::from_slice(&rewritten).unwrap();
        let names: Vec<&str> = value["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, ["list_graphs", "use_graph", "gql"]);
        let call =
            json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {}}).to_string();
        assert!(rewrite_directory_answer(&config, call.as_bytes(), b"{}").is_none());
    }
}
