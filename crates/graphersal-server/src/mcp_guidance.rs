//! Per-database guidance on the multi-database endpoint `/mcp` (MCPDB, 0.3.1): MCP hands out
//! instructions only at `initialize` (before a database is chosen) and one session-wide
//! `tools/list`, so `/mcp` serves the server default profile there
//! ([`crate::mcp_profiles::rewrite_directory_answer`]). What a database's own profile says
//! reaches the agent as tool-result text instead: the block **"Instructions for this
//! database"** ([`block`]), generated per caller:
//!
//! - the instructions of the database's own profile (its own, or an alias's database's): the
//!   notes of `append` mode, the whole text of `replace` mode; none without a profile of its
//!   own (the server default's instructions came with `initialize`);
//! - the tools really available on that database for this caller: the profile's selection
//!   (own, database's, server default, built-in: [`crate::mcp_control::McpControl::profile_for`])
//!   intersected with the caller's effective level (grant, base, MCP read-only:
//!   [`ReadOnlyWhy`]), and the tools of the session's list that are not available there, each
//!   with the reason (a database whose read-only flag is on keeps its tools, as on
//!   `/db/<name>/mcp`, and the block says that every write is refused);
//! - the saved queries the profile offers as tools (`saved_query_tools`), with their
//!   descriptions and parameters, to run with `run_saved_query` (never tools of their own on
//!   `/mcp`).
//!
//! **When** ([`annotate`]): appended to every successful `use_graph` answer, and to the first
//! tool call of an MCP session on a database (named by `graph`, or the session's selection);
//! [`Shown`] remembers per session (in memory) which databases were shown with which
//! [`Fingerprint`] (the real database, the profile, the read-only reason and flag), so the block comes
//! again when an alias was re-pointed, the profile changed or the caller's grant changed.
//! A request without a session gets it with every call.
//!
//! **Also**: `list_graphs` marks each database or alias that has a profile of its own with
//! `"hasInstructions": true`; a call of a built-in tool the database does not offer to the
//! caller is refused (graphersal-mcp's `unknown tool` error) with the tools that are
//! available there and a pointer to the block.
//!
//! **Size**: the block is at most the profile's answer limit (`answer_limit_chars`); a long
//! saved-query list is cut first, with a note naming `list_saved_queries`, then the
//! instructions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use graphersal_mcp::{GraphDirectory, Host, McpConfig, McpRequest, Profile};
use serde_json::{Value, json};

use crate::mcp::Directory;
use crate::mcp_control::McpControl;
use crate::mcp_profiles::ProfileSource;

/// The heading of the block.
pub(crate) const HEADING: &str = "Instructions for this database";

/// The arguments a saved-query tool takes for paging (not parameters of the query).
const PAGING_ARGS: [&str; 3] = ["offset", "max_rows", "max_items"];

/// The most MCP sessions [`Shown`] remembers (the oldest is forgotten first: its next call
/// shows the block again, which is harmless).
const MAX_SESSIONS: usize = 4096;

/// Why the calls on a database run read-only, the first that applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadOnlyWhy {
    /// The caller's grant is `read`.
    Grant,
    /// The database is a frozen base.
    Base,
    /// MCP serves the name or its database read-only (`mcp.toml`).
    Mcp,
}

impl ReadOnlyWhy {
    /// The first reason that applies, `None`: the caller may write.
    pub(crate) fn first(grant: bool, base: bool, mcp: bool) -> Option<Self> {
        if grant {
            Some(Self::Grant)
        } else if base {
            Some(Self::Base)
        } else if mcp {
            Some(Self::Mcp)
        } else {
            None
        }
    }

    /// The reason as the block says it.
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::Grant => "your access to this database is read-only",
            Self::Base => "the database is a frozen base (read-only)",
            Self::Mcp => "MCP serves this database read-only",
        }
    }
}

/// What one block is made of.
pub(crate) struct Facts<'a> {
    /// The name the agent used (a database or an alias).
    pub(crate) requested: &'a str,
    /// The real database.
    pub(crate) real: &'a str,
    /// The database's profile of its own (the name's, or an alias's database's), `None`: none.
    pub(crate) own_profile: Option<&'a Value>,
    /// The configuration the calls run under (`None`: the built-in one).
    pub(crate) config: Option<&'a McpConfig>,
    /// Why the calls run read-only, if they do.
    pub(crate) read_only: Option<ReadOnlyWhy>,
    /// Whether the database's read-only flag is on (its tools stay offered, every write is
    /// refused).
    pub(crate) flag: bool,
    /// The configuration of the session's tool list (`/mcp`: the server default; `None`:
    /// the built-in one).
    pub(crate) session_config: Option<&'a McpConfig>,
    /// The database's host (its saved queries).
    pub(crate) host: &'a dyn Host,
}

/// The instructions text of a profile (JSON): the notes of `append` mode, the whole text of
/// `replace` mode; `None` for the built-in instructions or an empty text.
pub(crate) fn instructions_of(profile: &Value) -> Option<String> {
    let instructions = profile.get("instructions")?;
    let mode = instructions
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("builtin");
    if !matches!(mode, "append" | "replace") {
        return None;
    }
    instructions
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// The names of the built-in data tools `profile` offers (in `tools/list` order).
fn offered(profile: Profile) -> Vec<String> {
    crate::mcp_profiles::throwaway(profile)
        .directory_tool_listings()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .filter(|name| !matches!(*name, "list_graphs" | "use_graph"))
        .map(str::to_string)
        .collect()
}

/// The profile of a call under `config` (`None`: built-in), read-only or not.
fn profile_of(config: Option<&McpConfig>, read_only: bool) -> Profile {
    Profile::new(read_only).with_config(config.cloned().unwrap_or_default())
}

/// The built-in tools available on the database for the caller.
pub(crate) fn available_tools(config: Option<&McpConfig>, read_only: bool) -> Vec<String> {
    offered(profile_of(config, read_only))
}

/// The tools of the session's list (`session_config`) that are not available on the database,
/// each with the reason.
fn unavailable(facts: &Facts<'_>, available: &[String]) -> Vec<(String, &'static str)> {
    let session = offered(profile_of(facts.session_config, false));
    let writer = offered(profile_of(facts.config, false));
    session
        .into_iter()
        .filter(|name| !available.contains(name))
        .map(|name| {
            let why = match facts.read_only {
                Some(why) if writer.contains(&name) => why.text(),
                _ => "not offered by the database's MCP profile",
            };
            (name, why)
        })
        .collect()
}

/// One line per saved query the profile offers as a tool on the database (`Err`: why they
/// cannot be listed).
fn saved_query_lines(facts: &Facts<'_>) -> Result<Vec<String>, String> {
    let Some(config) = facts.config.filter(|c| c.has_saved_query_tools()) else {
        return Ok(Vec::new());
    };
    let profile = profile_of(Some(config), facts.read_only.is_some());
    let mcp = crate::mcp_profiles::throwaway(profile.clone());
    let serve = |request: McpRequest<'_>| mcp.handle_with_profile(facts.host, &profile, request);
    let params = crate::mcp_profiles::initialize_params();
    let (session, _) = crate::mcp_profiles::rpc(&serve, 1, "initialize", params, None)?;
    let (_, tools) =
        crate::mcp_profiles::rpc(&serve, 2, "tools/list", json!({}), session.as_deref())?;
    let builtin = crate::mcp_profiles::builtin_tool_names();
    Ok(tools["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            let name = tool["name"].as_str()?;
            (!builtin.iter().any(|b| b == name)).then(|| saved_query_line(name, tool))
        })
        .collect())
}

/// `- name: description Parameters: a (string, required), b (integer, default 5).`
fn saved_query_line(name: &str, tool: &Value) -> String {
    let description = tool["description"]
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let schema = &tool["inputSchema"];
    let required: Vec<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let params: Vec<String> = schema["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| !PAGING_ARGS.contains(&key.as_str()))
        .map(|(key, property)| {
            let mut notes = Vec::new();
            match &property["type"] {
                Value::String(kind) => notes.push(kind.clone()),
                Value::Array(kinds) => notes.push(
                    kinds
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("|"),
                ),
                _ => {}
            }
            if required.contains(&key.as_str()) {
                notes.push("required".to_string());
            }
            if let Some(default) = property.get("default") {
                notes.push(format!("default {default}"));
            }
            if notes.is_empty() {
                key.clone()
            } else {
                format!("{key} ({})", notes.join(", "))
            }
        })
        .collect();
    let mut line = format!("- {name}");
    if !description.is_empty() {
        line.push_str(": ");
        line.push_str(&description);
    }
    if !params.is_empty() {
        line.push_str(if description.ends_with('.') || description.is_empty() {
            " Parameters: "
        } else {
            ". Parameters: "
        });
        line.push_str(&params.join(", "));
        line.push('.');
    }
    line
}

/// The first `max` characters of `text`.
fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The block "Instructions for this database" for `facts` (module docs), at most the profile's
/// answer limit in characters.
pub(crate) fn block(facts: &Facts<'_>) -> String {
    let max = profile_of(facts.config, false).max_chars();
    let name = json!(facts.requested);
    let mut head = if crate::names::fold(facts.requested) == facts.real {
        format!("--- {HEADING} {name} ---")
    } else {
        format!("--- {HEADING} {name} (alias of {}) ---", json!(facts.real))
    };
    head.push('\n');
    let available = available_tools(facts.config, facts.read_only.is_some());
    let mut tools = format!(
        "Tools available on {name} for you: {}.",
        if available.is_empty() {
            "none".to_string()
        } else {
            available.join(", ")
        }
    );
    let missing = unavailable(facts, &available);
    if !missing.is_empty() {
        // Grouped by reason, in the order of the session's list.
        let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
        for (tool, why) in &missing {
            match groups.iter_mut().find(|(w, _)| w == why) {
                Some((_, names)) => names.push(tool),
                None => groups.push((why, vec![tool])),
            }
        }
        let parts: Vec<String> = groups
            .iter()
            .map(|(why, names)| format!("{} ({why})", names.join(", ")))
            .collect();
        tools.push_str(&format!(
            "\nNot available on {name}, though the session's tool list has them: {}.",
            parts.join("; ")
        ));
    }
    if facts.flag && facts.read_only.is_none() {
        tools.push_str(&format!(
            "\n{name} is read-only now (its read-only flag): every write is refused until an \
             operator turns the flag off."
        ));
    }
    tools.push('\n');
    let mut queries_head = String::new();
    let lines = match saved_query_lines(facts) {
        Ok(lines) => lines,
        Err(err) => {
            queries_head =
                format!("The saved queries this database offers cannot be listed now: {err}\n");
            Vec::new()
        }
    };
    if !lines.is_empty() {
        queries_head = format!(
            "Saved queries this database offers (run one with run_saved_query {{\"name\": \"...\", \
             \"params\": {{...}}, \"graph\": {name}}}):\n"
        );
    }
    let instructions = facts
        .own_profile
        .and_then(instructions_of)
        .map(|text| format!("{text}\n\n"))
        .unwrap_or_default();
    let fixed = head.chars().count() + tools.chars().count() + queries_head.chars().count();
    let mut out = head;
    let room = max.saturating_sub(fixed);
    if instructions.chars().count() <= room {
        out.push_str(&instructions);
    } else {
        let note =
            format!("\n[the instructions are cut here: the answer limit is {max} characters]\n\n");
        out.push_str(&cut(
            &instructions,
            room.saturating_sub(note.chars().count()),
        ));
        out.push_str(&note);
    }
    out.push_str(&tools);
    if lines.is_empty() {
        out.push_str(&queries_head);
        return cut(out.trim_end(), max);
    }
    out.push_str(&queries_head);
    let note_of = |left: usize| {
        format!(
            "({left} more saved queries are not shown here: list_saved_queries {{\"graph\": \
             {name}}} lists them all)\n"
        )
    };
    let mut shown = 0;
    for (at, line) in lines.iter().enumerate() {
        let left_after = lines.len() - at - 1;
        let reserve = if left_after > 0 {
            note_of(left_after).chars().count()
        } else {
            0
        };
        if out.chars().count() + line.chars().count() + 1 + reserve > max {
            break;
        }
        out.push_str(line);
        out.push('\n');
        shown += 1;
    }
    if shown < lines.len() {
        out.push_str(&note_of(lines.len() - shown));
    }
    cut(out.trim_end(), max)
}

/// What a block was made from: a session is shown it again when this changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    real: String,
    config: Option<Arc<McpConfig>>,
    read_only: Option<ReadOnlyWhy>,
    flag: bool,
}

/// Which databases' blocks each `/mcp` session was shown, in memory (module docs): at most
/// [`MAX_SESSIONS`] sessions, a session forgotten when it ends (`DELETE`).
#[derive(Debug, Default)]
pub(crate) struct Shown {
    table: Mutex<ShownTable>,
}

#[derive(Debug, Default)]
struct ShownTable {
    next: u64,
    /// Per SHA-256 of the session id: its age and, per folded name, the fingerprint shown.
    sessions: HashMap<String, (u64, HashMap<String, Fingerprint>)>,
}

impl Shown {
    fn table(&self) -> std::sync::MutexGuard<'_, ShownTable> {
        self.table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether `session` was shown the block of `name` made from `fingerprint` (and records it
    /// as shown when it was not): `true` = show it now.
    pub(crate) fn show(&self, session: &str, name: &str, fingerprint: &Fingerprint) -> bool {
        let id = crate::mcp_control::sha256_hex(session);
        let name = crate::names::fold(name);
        let mut table = self.table();
        if let Some((_, names)) = table.sessions.get(&id)
            && names.get(&name) == Some(fingerprint)
        {
            return false;
        }
        if !table.sessions.contains_key(&id)
            && table.sessions.len() >= MAX_SESSIONS
            && let Some(oldest) = table
                .sessions
                .iter()
                .min_by_key(|(_, (age, _))| *age)
                .map(|(id, _)| id.clone())
        {
            table.sessions.remove(&oldest);
        }
        table.next += 1;
        let age = table.next;
        table
            .sessions
            .entry(id)
            .or_insert_with(|| (age, HashMap::new()))
            .1
            .insert(name, fingerprint.clone());
        true
    }

    /// Forgets `session` (it ended).
    pub(crate) fn forget(&self, session: &str) {
        let id = crate::mcp_control::sha256_hex(session);
        self.table().sessions.remove(&id);
    }

    /// How many sessions are remembered (tests).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.table().sessions.len()
    }
}

/// A call's database as the guidance sees it.
struct Target {
    requested: String,
    route: String,
    serving: crate::mcp::Serving,
}

/// The database a call names (`requested`) for the caller of `directory`, when it serves.
fn target(directory: &Directory<'_>, requested: &str) -> Option<Target> {
    let route = directory.resolve(requested).ok()?;
    let serving = directory.serving(&route).ok()?;
    Some(Target {
        requested: requested.to_string(),
        route,
        serving,
    })
}

/// The block of `target` for a session whose list is the server default's.
fn block_of(target: &Target, control: &McpControl) -> String {
    let (profile, source) = control.effective_profile(&target.route, &target.serving.entry.name);
    let own_profile = match source {
        ProfileSource::Own | ProfileSource::Database => profile,
        ProfileSource::Default | ProfileSource::Builtin => None,
    };
    let session_config = control.default_profile();
    block(&Facts {
        requested: &target.requested,
        real: &target.serving.entry.name,
        own_profile: own_profile.as_ref(),
        config: target.serving.config.as_deref(),
        read_only: target.serving.read_only,
        flag: target.serving.flag,
        session_config: session_config.as_deref(),
        host: target.serving.context.as_ref(),
    })
}

/// The fingerprint of `target`'s block.
fn fingerprint_of(target: &Target) -> Fingerprint {
    Fingerprint {
        real: target.serving.entry.name.clone(),
        config: target.serving.config.clone(),
        read_only: target.serving.read_only,
        flag: target.serving.flag,
    }
}

/// Adds the guidance to the answer `body` of a `/mcp` request (its JSON-RPC request `sent`,
/// in the MCP session `session`) for the caller of `directory` (module docs): the block after
/// `use_graph` and the first call on a database, `hasInstructions` in `list_graphs`, the
/// available tools in the refusal of a tool the database does not offer. Every other answer
/// (a batch, another method) is given back unchanged.
pub(crate) fn annotate(
    directory: &Directory<'_>,
    control: &McpControl,
    session: Option<&str>,
    sent: &[u8],
    body: Vec<u8>,
) -> Vec<u8> {
    let Ok(request) = serde_json::from_slice::<Value>(sent) else {
        return body;
    };
    if request["method"] != "tools/call" {
        return body;
    }
    let Ok(mut answer) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let name = request["params"]["name"].as_str().unwrap_or_default();
    let args = &request["params"]["arguments"];
    let changed = match name {
        "list_graphs" => mark_instructions(directory, control, &mut answer),
        "use_graph" => match args["name"].as_str().map(str::trim) {
            Some(requested) if !requested.is_empty() && answer["result"]["isError"] == false => {
                match target(directory, requested) {
                    Some(target) => {
                        if let Some(session) = session {
                            control
                                .shown()
                                .show(session, requested, &fingerprint_of(&target));
                        }
                        append(&mut answer, &block_of(&target, control))
                    }
                    None => false,
                }
            }
            _ => false,
        },
        _ if crate::mcp_profiles::builtin_tool_names()
            .iter()
            .any(|tool| tool == name) =>
        {
            let requested = match args["graph"].as_str().map(str::trim) {
                Some(graph) if !graph.is_empty() => Some(graph.to_string()),
                _ => session.and_then(|id| control.usage().selected(id)),
            };
            match requested.and_then(|r| target(directory, &r)) {
                None => false,
                Some(target) if answer.get("error").is_some() => refused(&mut answer, &target),
                Some(target) => {
                    let show = session.is_none_or(|session| {
                        control
                            .shown()
                            .show(session, &target.requested, &fingerprint_of(&target))
                    });
                    show && append(&mut answer, &block_of(&target, control))
                }
            }
        }
        _ => false,
    };
    if !changed {
        return body;
    }
    serde_json::to_vec(&answer).unwrap_or(body)
}

/// Appends `block` to the text of a tool result: whether there was one.
fn append(answer: &mut Value, block: &str) -> bool {
    let Some(text) = answer
        .pointer_mut("/result/content/0/text")
        .and_then(|text| text.as_str().map(str::to_string))
    else {
        return false;
    };
    answer["result"]["content"][0]["text"] = Value::String(format!("{text}\n\n{block}"));
    true
}

/// Adds the available tools to the refusal of a built-in tool the database does not offer
/// (graphersal-mcp's `unknown tool` error): whether it was one.
fn refused(answer: &mut Value, target: &Target) -> bool {
    let Some(message) = answer["error"]["message"].as_str() else {
        return false;
    };
    if !message.starts_with("unknown tool") {
        return false;
    }
    let available = available_tools(
        target.serving.config.as_deref(),
        target.serving.read_only.is_some(),
    );
    let message = format!(
        "{message}. On {} you can use: {}. The block \"{HEADING}\" (use_graph shows it, and the \
         first call on a database) lists them and says why the others are not available.",
        json!(target.requested),
        if available.is_empty() {
            "no tool".to_string()
        } else {
            available.join(", ")
        }
    );
    answer["error"]["message"] = Value::String(message);
    true
}

/// Marks each graph of a `list_graphs` answer that has a profile of its own with
/// `"hasInstructions": true` (the entries keep graphersal-mcp's key order): whether one was.
fn mark_instructions(directory: &Directory<'_>, control: &McpControl, answer: &mut Value) -> bool {
    let Some(text) = answer
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
    else {
        return false;
    };
    if answer["result"]["isError"] == true {
        return false;
    }
    // A rename note may come first.
    let (note, json_text) = match text.find('{') {
        Some(at) => text.split_at(at),
        None => return false,
    };
    let Ok(listing) = serde_json::from_str::<Value>(json_text) else {
        return false;
    };
    let Some(graphs) = listing["graphs"].as_array() else {
        return false;
    };
    let mut marked = false;
    let entries: Vec<String> = graphs
        .iter()
        .map(|graph| {
            let name = graph["name"].as_str().unwrap_or_default();
            let real = graph["aliasOf"].as_str().unwrap_or(name);
            let own = directory.resolve(name).is_ok()
                && matches!(
                    control.effective_profile(name, real).1,
                    ProfileSource::Own | ProfileSource::Database
                );
            marked |= own;
            ordered(graph, own)
        })
        .collect();
    if !marked {
        return false;
    }
    let text = format!(
        "{note}{{\"graphs\":[{}],\"selected\":{},\"selectedGraph\":{}}}",
        entries.join(","),
        listing["selected"],
        listing["selectedGraph"]
    );
    answer["result"]["content"][0]["text"] = Value::String(text);
    true
}

/// A `list_graphs` entry as text in graphersal-mcp's key order, `hasInstructions` last when
/// `own`; keys it does not know follow in their own order.
fn ordered(graph: &Value, own: bool) -> String {
    const ORDER: [&str; 7] = [
        "name",
        "aliasOf",
        "kind",
        "state",
        "reason",
        "readOnly",
        "description",
    ];
    let Some(fields) = graph.as_object() else {
        return graph.to_string();
    };
    let mut pairs: Vec<String> = ORDER
        .iter()
        .filter_map(|key| {
            fields
                .get(*key)
                .map(|value| format!("{}:{value}", json!(key)))
        })
        .collect();
    pairs.extend(
        fields
            .iter()
            .filter(|(key, _)| !ORDER.contains(&key.as_str()) && key.as_str() != "hasInstructions")
            .map(|(key, value)| format!("{}:{value}", json!(key))),
    );
    if own {
        pairs.push("\"hasInstructions\":true".to_string());
    }
    format!("{{{}}}", pairs.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(profile: Value) -> McpConfig {
        crate::mcp_profiles::config_of(&profile).unwrap()
    }

    #[test]
    fn the_instructions_of_a_profile_are_its_notes_or_its_whole_text() {
        let append = json!({"instructions": {"mode": "append", "text": " Notes. "}});
        assert_eq!(instructions_of(&append).as_deref(), Some("Notes."));
        let replace = json!({"instructions": {"mode": "replace", "text": "All."}});
        assert_eq!(instructions_of(&replace).as_deref(), Some("All."));
        assert_eq!(
            instructions_of(&json!({"instructions": {"mode": "builtin"}})),
            None
        );
        assert_eq!(instructions_of(&json!({})), None);
    }

    #[test]
    fn the_block_names_the_tools_and_why_the_others_are_missing() {
        let host = crate::mcp_profiles::empty_host().unwrap();
        let profile = json!({
            "instructions": {"mode": "append", "text": "Use the kb folder."},
            "tools": {"include": ["gql", "query", "set_schema"]},
        });
        let db = config(profile.clone());
        let session = config(json!({"tools": {"include": ["gql", "statistics", "set_schema"]}}));
        let facts = |read_only| Facts {
            requested: "kb",
            real: "kb",
            own_profile: Some(&profile),
            config: Some(&db),
            read_only,
            flag: false,
            session_config: Some(&session),
            host: host.as_ref(),
        };
        let writer = block(&facts(None));
        assert!(
            writer
                .starts_with("--- Instructions for this database \"kb\" ---\nUse the kb folder.\n"),
            "{writer}"
        );
        assert!(
            writer.contains("Tools available on \"kb\" for you: query, gql, set_schema."),
            "{writer}"
        );
        assert!(
            writer.contains("statistics (not offered by the database's MCP profile)"),
            "{writer}"
        );
        let reader = block(&facts(Some(ReadOnlyWhy::Grant)));
        assert!(reader.contains("for you: query, gql."), "{reader}");
        assert!(
            reader.contains("set_schema (your access to this database is read-only)"),
            "{reader}"
        );
        // A read-only flag keeps the tools and says that writes are refused.
        let flagged = block(&Facts {
            flag: true,
            ..facts(None)
        });
        assert!(
            flagged.contains("for you: query, gql, set_schema."),
            "{flagged}"
        );
        assert!(
            flagged.contains("\"kb\" is read-only now (its read-only flag)"),
            "{flagged}"
        );
        // The answer limit: the instructions are cut.
        let long = json!({
            "instructions": {"mode": "replace", "text": "x".repeat(5000)},
            "answer_limit_chars": 1000,
        });
        let small = config(long.clone());
        let cut_block = block(&Facts {
            own_profile: Some(&long),
            config: Some(&small),
            ..facts(None)
        });
        assert!(cut_block.chars().count() <= 1000, "{}", cut_block.len());
        assert!(
            cut_block.contains("the instructions are cut here"),
            "{cut_block}"
        );
        assert!(cut_block.contains("Tools available on"), "{cut_block}");
    }

    #[test]
    fn a_session_is_shown_a_block_once_per_fingerprint() {
        let shown = Shown::default();
        let one = Fingerprint {
            real: "a".into(),
            config: None,
            read_only: None,
            flag: false,
        };
        assert!(shown.show("s1", "cur", &one));
        assert!(!shown.show("s1", "CUR", &one));
        assert!(shown.show("s2", "cur", &one));
        let moved = Fingerprint {
            real: "b".into(),
            ..one.clone()
        };
        assert!(shown.show("s1", "cur", &moved));
        let reader = Fingerprint {
            read_only: Some(ReadOnlyWhy::Grant),
            ..moved.clone()
        };
        assert!(shown.show("s1", "cur", &reader));
        assert!(!shown.show("s1", "cur", &reader));
        shown.forget("s1");
        assert!(shown.show("s1", "cur", &reader));
        for n in 0..MAX_SESSIONS + 10 {
            shown.show(&format!("x{n}"), "a", &one);
        }
        assert_eq!(shown.len(), MAX_SESSIONS);
    }

    #[test]
    fn a_listing_entry_keeps_the_key_order() {
        let entry = json!({"name": "a", "kind": "plain", "state": "loaded", "readOnly": false});
        assert_eq!(
            ordered(&entry, true),
            r#"{"name":"a","kind":"plain","state":"loaded","readOnly":false,"hasInstructions":true}"#
        );
        assert_eq!(
            ordered(&entry, false),
            r#"{"name":"a","kind":"plain","state":"loaded","readOnly":false}"#
        );
    }
}
