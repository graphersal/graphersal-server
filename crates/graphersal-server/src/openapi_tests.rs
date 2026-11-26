//! The API descriptions themselves: structure, the engine patch, the per-database patch and the
//! offline base URL. The served answers, the route coverage and the console's export are
//! tested over a router in `router_access_tests.rs`.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::*;

/// The HTTP methods an OpenAPI path item may hold.
const OPERATION_METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Every `(method, path, operation)` of `spec`.
pub(crate) fn operations(spec: &Value) -> Vec<(String, String, Value)> {
    let mut out = Vec::new();
    for (path, item) in spec["paths"].as_object().into_iter().flatten() {
        for (method, operation) in item.as_object().into_iter().flatten() {
            if OPERATION_METHODS.contains(&method.as_str()) {
                out.push((method.clone(), path.clone(), operation.clone()));
            }
        }
    }
    out
}

/// Every `$ref` string below `value`.
fn refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                if key == "$ref" {
                    if let Some(target) = item.as_str() {
                        out.push(target.to_string());
                    }
                } else {
                    refs(item, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| refs(item, out)),
        _ => {}
    }
}

/// The structural checks of an OpenAPI 3.1 document every description here must pass (no
/// validator crate: the rules clients and generators trip over): the required top-level
/// fields, a `servers` URL, the two security schemes, unique operation ids, responses on every
/// operation, path parameters declared exactly as the template names them, and every `$ref`
/// resolving inside the document.
pub(crate) fn check_structure(spec: &Value, what: &str) {
    assert_eq!(spec["openapi"], "3.1.0", "{what}");
    assert!(spec["info"]["title"].is_string(), "{what}: info.title");
    assert!(spec["info"]["version"].is_string(), "{what}: info.version");
    assert!(spec["paths"].is_object(), "{what}: paths");
    let servers = spec["servers"].as_array().expect("servers");
    assert!(!servers.is_empty(), "{what}: servers");
    for server in servers {
        let url = server["url"].as_str().expect("servers[].url");
        assert!(
            url.starts_with("http://") || url.starts_with("https://"),
            "{what}: {url}"
        );
        assert!(!url.ends_with('/'), "{what}: {url}");
    }
    let schemes = &spec["components"]["securitySchemes"];
    assert_eq!(schemes["bearerAuth"]["type"], "http", "{what}");
    assert_eq!(schemes["bearerAuth"]["scheme"], "bearer", "{what}");
    assert_eq!(schemes["sessionCookie"]["type"], "apiKey", "{what}");
    assert_eq!(schemes["sessionCookie"]["in"], "cookie", "{what}");
    assert_eq!(schemes["sessionCookie"]["name"], "gs_session", "{what}");
    let security = spec["security"].as_array().expect("security");
    assert!(security.contains(&json!({"bearerAuth": []})), "{what}");
    assert!(security.contains(&json!({"sessionCookie": []})), "{what}");
    let mut ids = BTreeSet::new();
    for (method, path, operation) in operations(spec) {
        let at = format!("{what}: {method} {path}");
        let id = operation["operationId"]
            .as_str()
            .unwrap_or_else(|| panic!("{at}: operationId"));
        assert!(
            ids.insert(id.to_string()),
            "{at}: duplicate operationId {id}"
        );
        let responses = operation["responses"].as_object().expect("responses");
        assert!(!responses.is_empty(), "{at}: no responses");
        for (code, response) in responses {
            assert!(
                code.len() == 3 && code.chars().all(|c| c.is_ascii_digit()),
                "{at}: response code {code}"
            );
            assert!(
                response.get("$ref").is_some() || response["description"].is_string(),
                "{at} {code}: a response needs a description"
            );
        }
        let templated: BTreeSet<String> = path
            .split('/')
            .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
            .map(str::to_string)
            .collect();
        let mut declared = BTreeSet::new();
        let item_params = spec["paths"][&path]["parameters"].as_array().cloned();
        let op_params = operation["parameters"].as_array().cloned();
        for param in item_params.into_iter().chain(op_params).flatten() {
            let param = match param["$ref"].as_str() {
                Some(target) => resolve(spec, target).clone(),
                None => param,
            };
            if param["in"] == "path" {
                assert_eq!(param["required"], true, "{at}: path parameter not required");
                declared.insert(param["name"].as_str().unwrap_or_default().to_string());
            }
        }
        assert_eq!(templated, declared, "{at}: path parameters");
    }
    let mut targets = Vec::new();
    refs(spec, &mut targets);
    for target in targets {
        assert!(
            !resolve(spec, &target).is_null(),
            "{what}: unresolved $ref {target}"
        );
    }
}

/// The value a local `$ref` (`#/a/b`) names, `Null` when none.
fn resolve<'a>(spec: &'a Value, target: &str) -> &'a Value {
    let Some(pointer) = target.strip_prefix('#') else {
        return &Value::Null;
    };
    spec.pointer(pointer).unwrap_or(&Value::Null)
}

#[test]
fn the_server_description_is_a_valid_openapi_document() {
    let spec = server_spec("http://127.0.0.1:8484/").unwrap();
    check_structure(&spec, "server");
    assert_eq!(spec["info"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(spec["servers"][0]["url"], "http://127.0.0.1:8484");
    // MCP is named in the description, not described as REST.
    assert!(spec["paths"].get("/mcp").is_none());
    let text = spec["info"]["description"].as_str().unwrap();
    assert!(
        text.contains("/mcp") && text.contains("/db/<name>/mcp"),
        "{text}"
    );
    // Errors are {error, help}; a stale revision answers the current one.
    assert!(spec["components"]["schemas"]["Error"]["properties"]["help"].is_object());
    assert!(spec["components"]["schemas"]["Conflict"]["properties"]["revision"].is_object());
    // Every change that takes a revision says so: If-Match and a 409.
    for (method, path, operation) in operations(&spec) {
        let body_has_revision =
            operation["requestBody"]["content"]["application/json"]["schema"]["properties"]
                .get("revision")
                .is_some();
        if body_has_revision {
            let params = operation["parameters"].to_string();
            assert!(params.contains("IfMatch"), "{method} {path}: If-Match");
            assert!(
                operation["responses"]["409"].is_object(),
                "{method} {path}: 409"
            );
        }
        let access = operation["x-graphersal-access"]
            .as_str()
            .unwrap_or_default();
        assert!(
            ["admin", "any", "public", "probe"].contains(&access),
            "{method} {path}: x-graphersal-access {access:?}"
        );
        if path.starts_with("/api/admin/") {
            assert_eq!(access, "admin", "{method} {path}");
            assert!(
                operation.get("security").is_none(),
                "{method} {path}: global security"
            );
        }
    }
}

#[test]
fn the_engine_patch_is_still_needed_and_applies() {
    let raw = served_engine_spec().unwrap();
    let patch: Value = serde_json::from_str(ENGINE_PATCH).unwrap();
    assert_eq!(
        patch["engineVersion"], raw["info"]["version"],
        "openapi/engine-patch.json was made for another graphersal-serve: check what the new \
         version's openapi.json carries and drop what it has"
    );
    for (name, _) in patch["responses"].as_object().unwrap() {
        assert!(
            raw["components"]["responses"].get(name).is_none(),
            "the engine has the response {name} now: drop it from engine-patch.json"
        );
    }
    for (key, operation) in patch["operations"].as_object().unwrap() {
        let (method, path) = key.split_once(' ').unwrap();
        let theirs = &raw["paths"][path][method.to_ascii_lowercase()];
        assert!(theirs.is_object(), "the engine has no {key}");
        assert_ne!(
            theirs, operation,
            "the engine has {key} as patched: drop it"
        );
    }
    let patched = engine_spec().unwrap();
    assert_eq!(
        patched["paths"]["/api/schema/set"]["post"]["responses"]["409"]["$ref"],
        "#/components/responses/E409ReadOnlyRequest"
    );
    assert!(patched["components"]["responses"]["E403FileRoot"].is_object());
    assert!(
        patched["paths"]["/api/store/backup"]["post"]["responses"]["403"]["$ref"]
            .as_str()
            .unwrap()
            .ends_with("E403FileRoot")
    );
}

#[test]
fn a_database_description_names_the_mount_security_and_the_access_answers() {
    let engine = engine_spec().unwrap();
    let spec = database_spec(&engine, "https://db.example.com/", "acme", false);
    check_structure(&spec, "database");
    assert_eq!(spec["servers"][0]["url"], "https://db.example.com/db/acme");
    assert_eq!(
        spec["security"],
        json!([{"bearerAuth": []}, {"sessionCookie": []}])
    );
    assert_eq!(spec["info"]["x-graphersal-database"], "acme");
    assert_eq!(spec["info"]["version"], env!("CARGO_PKG_VERSION"));
    for path in [
        "/healthz",
        "/readyz",
        "/api/admin/runs",
        "/api/admin/runs/cancel",
    ] {
        assert!(spec["paths"].get(path).is_none(), "{path} is not mounted");
    }
    assert!(
        !spec["paths"]
            .as_object()
            .unwrap()
            .keys()
            .any(|p| p.starts_with("/api/ai/"))
    );
    let mut writes = BTreeSet::new();
    for (method, path, operation) in operations(&spec) {
        let at = format!("{method} {path}");
        for code in ["401", "404"] {
            assert!(operation["responses"][code].is_object(), "{at}: {code}");
        }
        let spec_route = path.ends_with("openapi.json");
        assert_eq!(
            operation["responses"]["409"].is_object(),
            !spec_route,
            "{at}: 409 (the description is answered also when not loaded)"
        );
        let access = operation["x-graphersal-access"].as_str().unwrap();
        if access == "write" {
            writes.insert(at.clone());
            let text = operation["responses"]["409"]["description"]
                .as_str()
                .unwrap();
            assert!(text.contains("read grant"), "{at}: {text}");
            assert!(text.contains("from the database's engine"), "{at}: {text}");
        }
        assert!(
            ["read", "query", "write"].contains(&access),
            "{at}: {access}"
        );
    }
    // The engine's writing routes (graphersal-serve's route_access `Write`, without the run
    // administration the mounts keep off).
    assert_eq!(writes.len(), 35, "{writes:#?}");
    for at in [
        "post /api/schema/set",
        "post /api/element/create",
        "post /api/save",
        "post /api/store/backup",
        "post /api/catalog/load",
        "post /api/jobs/define",
    ] {
        assert!(writes.contains(at), "{at}");
    }
    assert_eq!(
        spec["paths"]["/api/execute"]["post"]["x-graphersal-access"],
        "query"
    );
    assert_eq!(
        spec["paths"]["/api/graph"]["get"]["x-graphersal-access"],
        "read"
    );
    // A 404 the engine describes too is kept beside the access layer's.
    let layout = spec["paths"]["/api/layout"]["post"]["responses"]["404"]["description"]
        .as_str()
        .unwrap();
    assert!(layout.starts_with("graphersal-server:"), "{layout}");
    assert!(layout.contains("from the database's engine"), "{layout}");
    // The MCP endpoint of the mount says what the switches answer.
    let mcp = &spec["paths"]["/mcp"]["post"];
    assert!(mcp["description"].as_str().unwrap().contains("mcp.toml"));
    assert!(mcp["responses"]["503"].is_object());

    let open = database_spec(&engine, "http://127.0.0.1:8484", "cur", true);
    assert_eq!(
        open["security"],
        json!([{"bearerAuth": []}, {"sessionCookie": []}, {}])
    );
    assert_eq!(open["servers"][0]["url"], "http://127.0.0.1:8484/db/cur");
}

#[test]
fn the_offline_base_url_follows_server_toml() {
    let parse =
        |text: &str| ServerConfig::parse(text, std::path::Path::new("server.toml")).unwrap();
    let config = parse("");
    assert_eq!(offline_base_url(&config, None), "http://127.0.0.1:8484");
    assert_eq!(
        offline_base_url(&config, Some("https://db.example.com/")),
        "https://db.example.com"
    );
    assert_eq!(
        offline_base_url(&config, Some("10.0.0.5:9000")),
        "http://10.0.0.5:9000"
    );
    let config = parse("listen = \"0.0.0.0:7000\"\n");
    assert_eq!(offline_base_url(&config, None), "http://127.0.0.1:7000");
    let config = parse("[http]\npublic_scheme = \"https\"\nallowed_hosts = [\"db.example.com\"]\n");
    assert_eq!(offline_base_url(&config, None), "https://db.example.com");
    assert_eq!(
        offline_base_url(&config, Some("proxy:8443")),
        "https://proxy:8443"
    );
    assert_eq!(
        request_base_url(true, Some("db.example.com")).as_deref(),
        Some("https://db.example.com")
    );
    assert_eq!(request_base_url(false, None), None);
}
