//! Concurrent admin changes over a real listener: the revisions of the shared objects
//! (`crate::revision`: a stale change is `409`, nothing changed) and a stress test of many
//! clients changing users, grants, aliases, flags and databases at once (every acknowledged
//! change is on disk and in memory, the files parse, no temporary file is left, the memory
//! equals a fresh load).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::access::AccessFile;
use crate::database::{DATABASE_FILE, DatabaseConfig};
use crate::layout::{ServerDir, is_temp_name};
use crate::registry::Registry;
use crate::registry::tests::server;
use crate::revision::CONFLICT_HELP;
use crate::serve::{Running, ServeOptions};
use crate::test_support::TempDir;

/// One answer: status, headers, JSON body (`Null` when empty).
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Value,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A client of the admin API as the admin (the run's operator token).
#[derive(Clone)]
struct Client {
    address: SocketAddr,
    token: String,
}

impl Client {
    fn of(running: &Running) -> Self {
        let address = running.address();
        let token = crate::test_support::operator_token(address).expect("the operator token");
        Self { address, token }
    }

    /// `method path` with `body` as JSON and `If-Match: "<if_match>"` when given.
    fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        if_match: Option<&str>,
    ) -> Answer {
        let mut stream = TcpStream::connect(self.address).expect("connects");
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .expect("timeout");
        let payload = body.map(Value::to_string).unwrap_or_default();
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: \
             application/json\r\nAuthorization: Bearer {}\r\n",
            self.address, self.token
        );
        if body.is_some() {
            head.push_str("Content-Type: application/json\r\n");
        }
        if let Some(revision) = if_match {
            head.push_str(&format!("If-Match: \"{revision}\"\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n\r\n", payload.len()));
        stream.write_all(head.as_bytes()).expect("sends");
        stream.write_all(payload.as_bytes()).expect("sends");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("reads");
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a header end");
        let headers = String::from_utf8_lossy(&raw[..split])
            .lines()
            .skip(1)
            .filter_map(|line| {
                let (field, value) = line.split_once(':')?;
                Some((field.trim().to_string(), value.trim().to_string()))
            })
            .collect();
        let (status, body) = crate::client::parse_answer(&raw).expect("an answer");
        let body = if body.iter().all(u8::is_ascii_whitespace) {
            Value::Null
        } else {
            serde_json::from_slice(&body).expect("JSON")
        };
        Answer {
            status,
            headers,
            body,
        }
    }

    fn get(&self, path: &str) -> Answer {
        self.send("GET", path, None, None)
    }
}

fn start(dir: &ServerDir) -> Running {
    crate::serve::start(
        dir.root(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .expect("the server starts")
}

fn create(client: &Client, name: &str) {
    let answer = client.send(
        "POST",
        "/api/admin/databases",
        Some(&json!({"name": name})),
        None,
    );
    assert_eq!(answer.status, 201, "{}", answer.body);
}

fn revision_of(answer: &Answer) -> String {
    answer.body["revision"]
        .as_str()
        .unwrap_or_else(|| panic!("no revision in {}", answer.body))
        .to_string()
}

/// A refused stale change: `409`, the help, the current revision.
fn assert_conflict(answer: &Answer, current: &str) {
    assert_eq!(answer.status, 409, "{}", answer.body);
    assert_eq!(answer.body["help"], CONFLICT_HELP, "{}", answer.body);
    assert_eq!(answer.body["revision"], current, "{}", answer.body);
    let error = answer.body["error"].as_str().unwrap_or_default();
    assert!(error.contains("changed by someone else"), "{error}");
}

#[test]
fn stale_revisions_are_refused_and_change_nothing() {
    let temp = TempDir::new("concurrency-revisions");
    let dir = server(&temp);
    let running = start(&dir);
    let client = Client::of(&running);
    create(&client, "acme");
    create(&client, "beta");

    // Users and grants: one revision of access.toml.
    let users = client.get("/api/admin/users");
    let r1 = revision_of(&users);
    assert_eq!(users.header("ETag"), Some(format!("\"{r1}\"").as_str()));
    let added = client.send(
        "POST",
        "/api/admin/users",
        Some(&json!({"name": "bob"})),
        None,
    );
    assert_eq!(added.status, 201, "{}", added.body);
    let r2 = revision_of(&client.get("/api/admin/users"));
    assert_ne!(r1, r2);
    assert_eq!(revision_of(&client.get("/api/admin/grants")), r2);
    let grant = json!({"subject": "bob", "database": "acme", "level": "read"});
    let mut stale = grant.clone();
    stale["revision"] = json!(r1);
    assert_conflict(
        &client.send("POST", "/api/admin/grants", Some(&stale), None),
        &r2,
    );
    assert_conflict(
        &client.send("POST", "/api/admin/grants", Some(&grant), Some(&r1)),
        &r2,
    );
    assert!(running.access().unwrap().file().grants().is_empty());
    let set = client.send("POST", "/api/admin/grants", Some(&grant), Some(&r2));
    assert_eq!(set.status, 200, "{}", set.body);
    let r3 = revision_of(&client.get("/api/admin/users"));
    for (method, path) in [
        ("DELETE", "/api/admin/grants/user:bob/acme"),
        ("POST", "/api/admin/users/bob/token"),
        ("DELETE", "/api/admin/users/bob/token"),
        ("DELETE", "/api/admin/users/bob"),
    ] {
        assert_conflict(&client.send(method, path, None, Some(&r2)), &r3);
    }
    let roles = client.send(
        "PUT",
        "/api/admin/users/bob/admin",
        Some(&json!({"admin": true, "revision": r2})),
        None,
    );
    assert_conflict(&roles, &r3);
    assert_eq!(
        revision_of(&client.get("/api/admin/users")),
        r3,
        "nothing changed"
    );
    // `*` and no revision at all: no check (the console, scripts).
    let any = client.send("DELETE", "/api/admin/grants/user:bob/acme", None, Some("*"));
    assert_eq!(any.status, 200, "{}", any.body);
    assert_eq!(any.body["revoked"], true);
    let free = client.send("POST", "/api/admin/grants", Some(&grant), None);
    assert_eq!(free.status, 200, "{}", free.body);

    // Aliases: one revision of aliases.toml.
    let a1 = revision_of(&client.get("/api/admin/aliases"));
    let set = client.send(
        "PUT",
        "/api/admin/aliases/live",
        Some(&json!({"target": "acme", "revision": a1})),
        None,
    );
    assert_eq!(set.status, 200, "{}", set.body);
    let aliases = client.get("/api/admin/aliases");
    let a2 = revision_of(&aliases);
    assert_eq!(aliases.header("ETag"), Some(format!("\"{a2}\"").as_str()));
    assert_eq!(
        client.get("/api/admin/databases").body["aliasesRevision"],
        a2
    );
    let flip = client.send(
        "PUT",
        "/api/admin/aliases/live",
        Some(&json!({"target": "beta", "revision": a1})),
        None,
    );
    assert_conflict(&flip, &a2);
    let rename = client.send(
        "POST",
        "/api/admin/aliases/live/rename",
        Some(&json!({"name": "next"})),
        Some(&a1),
    );
    assert_conflict(&rename, &a2);
    assert_conflict(
        &client.send("DELETE", "/api/admin/aliases/live", None, Some(&a1)),
        &a2,
    );
    assert_eq!(
        running.registry().aliases()["live"],
        "acme",
        "nothing changed"
    );
    let rename = client.send(
        "POST",
        "/api/admin/aliases/live/rename",
        Some(&json!({"name": "next"})),
        Some(&a2),
    );
    assert_eq!(rename.status, 200, "{}", rename.body);

    // A database's settings: one revision of its database.toml.
    let entry = client.get("/api/admin/databases/acme");
    let d1 = revision_of(&entry);
    assert_eq!(entry.header("ETag"), Some(format!("\"{d1}\"").as_str()));
    let listed = client.get("/api/admin/databases").body;
    let row = listed["databases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "acme")
        .unwrap()
        .clone();
    assert_eq!(row["revision"], d1);
    let on = client.send(
        "PUT",
        "/api/admin/databases/acme/read-only",
        Some(&json!({"readOnly": true, "revision": d1})),
        None,
    );
    assert_eq!(on.status, 200, "{}", on.body);
    let d2 = revision_of(&on);
    assert_ne!(d1, d2, "the answer carries the new revision");
    let auto = client.send(
        "PUT",
        "/api/admin/databases/acme/auto-load",
        Some(&json!({"autoLoad": true})),
        Some(&d1),
    );
    assert_conflict(&auto, &d2);
    let off = client.send(
        "PUT",
        "/api/admin/databases/acme/read-only",
        Some(&json!({"readOnly": false, "revision": d1})),
        None,
    );
    assert_conflict(&off, &d2);
    let on_disk = DatabaseConfig::read(&dir.databases().join("acme")).unwrap();
    assert!(
        on_disk.read_only && !on_disk.auto_load,
        "nothing changed on disk"
    );
    // Another database's revision is its own: beta is unaffected by acme's changes.
    let beta = revision_of(&client.get("/api/admin/databases/beta"));
    let auto = client.send(
        "PUT",
        "/api/admin/databases/beta/auto-load",
        Some(&json!({"autoLoad": true, "revision": beta})),
        None,
    );
    assert_eq!(auto.status, 200, "{}", auto.body);
    running.stop().unwrap();
}

/// A tiny deterministic random sequence (xorshift), one per client.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// What one client changed and the server acknowledged (each client changes only its own
/// users, aliases and databases, so the models add up to the expected state).
#[derive(Default)]
struct Model {
    /// User -> its token's last four characters (`None`: no token).
    users: BTreeMap<String, Option<String>>,
    /// (subject, database) -> level.
    grants: BTreeMap<(String, String), String>,
    aliases: BTreeMap<String, String>,
    created: BTreeSet<String>,
    read_only: bool,
    auto_load: bool,
    /// The `autoLoad` values of the shared database this client set (acknowledged).
    shared: Vec<bool>,
    /// The changes sent with a revision that met a `409` (retried without one).
    conflicts: usize,
}

const CLIENTS: usize = 8;
const ROUNDS: usize = 50;

/// The databases every client may point aliases at (never deleted).
fn fixed(i: usize) -> String {
    format!("p{i}")
}

/// One client's run: `ROUNDS` mixed admin changes; some carry the revision just read (a
/// `409` is retried without one).
fn client_run(client: &Client, me: usize) -> Model {
    let mut model = Model::default();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ ((me as u64 + 1) * 0x1000_0001));
    let mut next = 0usize;
    let mut fresh = |prefix: &str| {
        next += 1;
        format!("{prefix}{me}-{next}")
    };
    let own_db = fixed(me);
    let conflicts = std::cell::Cell::new(0usize);
    for _ in 0..ROUNDS {
        // The revision of the object the change touches, read just before (or none).
        let checked = rng.below(3) == 0;
        let pick = |rng: &mut Rng, items: Vec<String>| -> Option<String> {
            (!items.is_empty()).then(|| items[rng.below(items.len())].clone())
        };
        let users: Vec<String> = model.users.keys().cloned().collect();
        let aliases: Vec<String> = model.aliases.keys().cloned().collect();
        let created: Vec<String> = model.created.iter().cloned().collect();
        let action = rng.below(14);
        // Sends with the revision of `revision_path` when `checked`; a `409` is retried
        // without (the object changed meanwhile: another client's change).
        let send = |method: &str, path: &str, body: Option<Value>, revision_path: &str| {
            if checked {
                let revision = client.get(revision_path).body["revision"]
                    .as_str()
                    .map(str::to_string);
                let answer = client.send(method, path, body.as_ref(), revision.as_deref());
                if answer.status != 409 {
                    return answer;
                }
                assert_eq!(answer.body["help"], CONFLICT_HELP, "{}", answer.body);
                conflicts.set(conflicts.get() + 1);
            }
            client.send(method, path, body.as_ref(), None)
        };
        match (action, pick(&mut rng, users), pick(&mut rng, aliases)) {
            (1, Some(user), _) => {
                let answer = send(
                    "DELETE",
                    &format!("/api/admin/users/{user}"),
                    None,
                    "/api/admin/users",
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                model.users.remove(&user);
                let subject = format!("user:{user}");
                model.grants.retain(|(s, _), _| *s != subject);
            }
            (2, Some(user), _) => {
                let answer = send(
                    "POST",
                    &format!("/api/admin/users/{user}/token"),
                    None,
                    "/api/admin/users",
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                let last4 = answer.body["tokenLast4"].as_str().map(str::to_string);
                assert!(last4.is_some(), "{}", answer.body);
                model.users.insert(user, last4);
            }
            (3, Some(user), _) => {
                let answer = send(
                    "DELETE",
                    &format!("/api/admin/users/{user}/token"),
                    None,
                    "/api/admin/users",
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                model.users.insert(user, None);
            }
            (4 | 5, Some(user), alias) => {
                // On the own database, an own alias or an own created database.
                let mut targets = vec![own_db.clone()];
                targets.extend(alias);
                targets.extend(created.iter().cloned());
                let database = targets[rng.below(targets.len())].clone();
                let level = if rng.below(2) == 0 { "read" } else { "write" };
                let body = json!({"subject": format!("user:{user}"), "database": database, "level": level});
                let answer = send("POST", "/api/admin/grants", Some(body), "/api/admin/grants");
                assert_eq!(answer.status, 200, "{}", answer.body);
                model
                    .grants
                    .insert((format!("user:{user}"), database), level.to_string());
            }
            (6, _, _) if !model.grants.is_empty() => {
                let keys: Vec<(String, String)> = model.grants.keys().cloned().collect();
                let (subject, database) = keys[rng.below(keys.len())].clone();
                let answer = send(
                    "DELETE",
                    &format!("/api/admin/grants/{subject}/{database}"),
                    None,
                    "/api/admin/grants",
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                assert_eq!(answer.body["revoked"], true, "{}", answer.body);
                model.grants.remove(&(subject, database));
            }
            (7, _, Some(alias)) => {
                // A flip to another fixed database.
                let target = fixed(rng.below(CLIENTS));
                let body = json!({"target": target});
                let answer = send(
                    "PUT",
                    &format!("/api/admin/aliases/{alias}"),
                    Some(body),
                    "/api/admin/aliases",
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                model.aliases.insert(alias, target);
            }
            (8, _, Some(alias)) => {
                let new = fresh("b");
                let body = json!({"name": new});
                let answer = send(
                    "POST",
                    &format!("/api/admin/aliases/{alias}/rename"),
                    Some(body),
                    "/api/admin/aliases",
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                let target = model.aliases.remove(&alias).unwrap();
                model.aliases.insert(new.clone(), target);
                let moved: Vec<((String, String), String)> = model
                    .grants
                    .iter()
                    .filter(|((_, d), _)| *d == alias)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                for ((subject, _), level) in moved {
                    model.grants.remove(&(subject.clone(), alias.clone()));
                    model.grants.insert((subject, new.clone()), level);
                }
            }
            (9, _, Some(alias)) => {
                let answer = send(
                    "DELETE",
                    &format!("/api/admin/aliases/{alias}"),
                    None,
                    "/api/admin/aliases",
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                model.aliases.remove(&alias);
                model.grants.retain(|(_, d), _| *d != alias);
            }
            (10, _, _) => {
                let value = !model.read_only;
                let answer = send(
                    "PUT",
                    &format!("/api/admin/databases/{own_db}/read-only"),
                    Some(json!({"readOnly": value})),
                    &format!("/api/admin/databases/{own_db}"),
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                model.read_only = value;
            }
            (11, _, _) => {
                let value = !model.auto_load;
                let answer = send(
                    "PUT",
                    &format!("/api/admin/databases/{own_db}/auto-load"),
                    Some(json!({"autoLoad": value})),
                    &format!("/api/admin/databases/{own_db}"),
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                model.auto_load = value;
            }
            (12, _, _) => {
                // The shared database: every client flips it (contended database.toml).
                let value = rng.below(2) == 0;
                let answer = send(
                    "PUT",
                    "/api/admin/databases/shared/auto-load",
                    Some(json!({"autoLoad": value})),
                    "/api/admin/databases/shared",
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                model.shared.push(value);
            }
            (13, _, _) if !created.is_empty() && rng.below(2) == 0 => {
                let name = created[rng.below(created.len())].clone();
                let answer = client.send(
                    "DELETE",
                    &format!("/api/admin/databases/{name}"),
                    None,
                    None,
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                model.created.remove(&name);
                model.grants.retain(|(_, d), _| *d != name);
            }
            (13, _, _) => {
                let name = fresh("c");
                create(client, &name);
                model.created.insert(name);
            }
            (_, _, _) if rng.below(2) == 0 || model.users.is_empty() => {
                let user = fresh("u");
                let answer = send(
                    "POST",
                    "/api/admin/users",
                    Some(json!({"name": user})),
                    "/api/admin/users",
                );
                assert_eq!(answer.status, 201, "{}", answer.body);
                let last4 = answer.body["tokenLast4"].as_str().map(str::to_string);
                model.users.insert(user, last4);
            }
            _ => {
                let alias = fresh("a");
                let target = fixed(rng.below(CLIENTS));
                let answer = send(
                    "PUT",
                    &format!("/api/admin/aliases/{alias}"),
                    Some(json!({"target": target})),
                    "/api/admin/aliases",
                );
                assert_eq!(answer.status, 200, "{}", answer.body);
                model.aliases.insert(alias, target);
            }
        }
    }
    model.conflicts = conflicts.get();
    model
}

/// Every file below `dir` (not into Stores, which the engine owns) whose name is a temporary
/// name of this server.
fn temp_files(dir: &Path, out: &mut Vec<String>) {
    for item in std::fs::read_dir(dir).unwrap().flatten() {
        let path = item.path();
        let name = item.file_name().to_string_lossy().into_owned();
        if item.file_type().unwrap().is_dir() {
            if name != crate::database::STORE_DIR {
                temp_files(&path, out);
            }
        } else if is_temp_name(&name) {
            out.push(path.display().to_string());
        }
    }
}

#[test]
fn many_clients_changing_everything_at_once_lose_nothing() {
    let temp = TempDir::new("concurrency-stress");
    let dir = server(&temp);
    let running = start(&dir);
    let client = Client::of(&running);
    for i in 0..CLIENTS {
        create(&client, &fixed(i));
    }
    create(&client, "shared");

    let stop = std::sync::atomic::AtomicBool::new(false);
    let models: Vec<Model> = std::thread::scope(|scope| {
        // A reader beside the writers: every listing answers whole, all the time.
        let reader = scope.spawn(|| {
            let mut reads = 0usize;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                for path in [
                    "/api/admin/databases",
                    "/api/admin/users",
                    "/api/admin/aliases",
                ] {
                    let answer = client.get(path);
                    assert_eq!(answer.status, 200, "{path}: {}", answer.body);
                    assert!(answer.body["revision"].is_string() || path == "/api/admin/databases");
                    reads += 1;
                }
            }
            reads
        });
        let writers: Vec<_> = (0..CLIENTS)
            .map(|me| {
                let client = client.clone();
                scope.spawn(move || client_run(&client, me))
            })
            .collect();
        let models = writers.into_iter().map(|w| w.join().unwrap()).collect();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(reader.join().unwrap() > 0);
        models
    });

    // Every acknowledged change, over the API.
    let mut users: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut grants: BTreeSet<(String, String, String)> = BTreeSet::new();
    let mut aliases: BTreeMap<String, String> = BTreeMap::new();
    let mut databases: BTreeSet<String> = (0..CLIENTS).map(fixed).collect();
    databases.insert("shared".to_string());
    for model in &models {
        users.extend(model.users.clone());
        grants.extend(
            model
                .grants
                .iter()
                .map(|((s, d), l)| (s.clone(), d.clone(), l.clone())),
        );
        aliases.extend(model.aliases.clone());
        databases.extend(model.created.iter().cloned());
    }
    let conflicts: usize = models.iter().map(|m| m.conflicts).sum();
    eprintln!(
        "stress: {} users, {} grants, {} aliases, {} databases, {} stale revisions retried",
        users.len(),
        grants.len(),
        aliases.len(),
        databases.len(),
        conflicts
    );
    assert!(!users.is_empty() && !grants.is_empty() && !aliases.is_empty());
    assert!(databases.len() > CLIENTS + 1, "databases were created");
    let listed = client.get("/api/admin/users").body;
    let mut seen_users: BTreeMap<String, Option<String>> = BTreeMap::new();
    for user in listed["users"].as_array().unwrap() {
        let name = user["name"].as_str().unwrap().to_string();
        if name == crate::access::FIRST_ADMIN {
            continue;
        }
        let last4 = user["hasToken"]
            .as_bool()
            .unwrap()
            .then(|| user["tokenLast4"].as_str().unwrap().to_string());
        seen_users.insert(name, last4);
    }
    assert_eq!(seen_users, users, "users and their tokens");
    let seen_grants: BTreeSet<(String, String, String)> = listed["grants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["subject"].as_str().unwrap().to_string(),
                g["database"].as_str().unwrap().to_string(),
                g["level"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(seen_grants, grants, "grants");
    let seen_aliases: BTreeMap<String, String> =
        serde_json::from_value(client.get("/api/admin/aliases").body["aliases"].clone()).unwrap();
    assert_eq!(seen_aliases, aliases, "aliases");
    let listing = client.get("/api/admin/databases").body;
    let rows: Vec<&Value> = listing["databases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["aliasOf"].is_null())
        .collect();
    let seen: BTreeSet<String> = rows
        .iter()
        .map(|row| row["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(seen, databases, "databases");
    for (me, model) in models.iter().enumerate() {
        let row = rows.iter().find(|row| row["name"] == fixed(me)).unwrap();
        assert_eq!(row["readOnly"], model.read_only, "{row}");
        assert_eq!(row["autoLoad"], model.auto_load, "{row}");
    }
    let shared = rows.iter().find(|row| row["name"] == "shared").unwrap();
    let acknowledged: Vec<bool> = models.iter().flat_map(|m| m.shared.clone()).collect();
    if acknowledged.is_empty() {
        assert_eq!(shared["autoLoad"], false);
    } else {
        assert!(acknowledged.contains(&shared["autoLoad"].as_bool().unwrap()));
    }

    // The memory now, then a fresh load of the files after the stop.
    let memory_entries: BTreeMap<String, Option<DatabaseConfig>> = running
        .registry()
        .list()
        .into_iter()
        .map(|entry| (entry.name, entry.config))
        .collect();
    let memory_aliases = running.registry().aliases();
    let memory_access = running.access().unwrap().file().to_text();
    running.stop().unwrap();
    let mut temps = Vec::new();
    temp_files(dir.root(), &mut temps);
    assert!(temps.is_empty(), "temporary files left: {temps:?}");
    assert_eq!(
        std::fs::read_dir(dir.tmp()).unwrap().count(),
        0,
        "tmp/ is empty"
    );
    // Every file parses.
    dir.load_config().expect("server.toml parses");
    let access = AccessFile::read(&dir).expect("access.toml parses").unwrap();
    let aliases_text = std::fs::read_to_string(dir.aliases_path()).unwrap();
    let _: toml::Value = toml::from_str(&aliases_text).expect("aliases.toml parses");
    for item in std::fs::read_dir(dir.databases()).unwrap().flatten() {
        assert!(item.path().join(DATABASE_FILE).is_file());
        DatabaseConfig::read(&item.path()).expect("database.toml parses");
    }
    for item in std::fs::read_dir(dir.sessions()).unwrap().flatten() {
        let text = std::fs::read_to_string(item.path()).unwrap();
        let _: Value = serde_json::from_str(&text).expect("a session file parses");
    }
    let fresh = Registry::scan(&dir).unwrap();
    assert!(fresh.problems().is_empty(), "{:?}", fresh.problems());
    let fresh_entries: BTreeMap<String, Option<DatabaseConfig>> = fresh
        .list()
        .into_iter()
        .map(|entry| (entry.name, entry.config))
        .collect();
    assert_eq!(fresh_entries, memory_entries, "databases: memory = files");
    assert_eq!(fresh.aliases(), memory_aliases, "aliases: memory = files");
    assert_eq!(access.to_text(), memory_access, "access: memory = files");
}
