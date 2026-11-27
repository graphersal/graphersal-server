//! A TINY synthetic Model2Vec embedding model for the tests (a word-level tokenizer of four
//! tokens, an f32 matrix of 4 x 3); no network.
//!
//! Copied from the graphersal repository's test support
//! `crates/graphersal/tests/all/model_fixture.rs` (`VOCAB`, `DIMENSION`, `tokenizer_json`,
//! `safetensors`, `CONFIG`, `tiny_model`), which is test-only there and cannot be depended on.

use std::io::Cursor;

use graphersal::persist::{ModelRole, NewModel};

/// The vocabulary of the tiny model.
const VOCAB: [&str; 4] = ["[UNK]", "graph", "vector", "model"];
/// The tiny model's dimension.
const DIMENSION: usize = 3;

/// The tokenizer of the tiny model.
fn tokenizer_json() -> String {
    let entries: Vec<String> = VOCAB
        .iter()
        .enumerate()
        .map(|(id, token)| format!("{token:?}: {id}"))
        .collect();
    format!(
        r#"{{"version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
            "normalizer": {{"type": "Lowercase"}}, "pre_tokenizer": {{"type": "Whitespace"}},
            "post_processor": null, "decoder": null,
            "model": {{"type": "WordLevel", "vocab": {{{}}}, "unk_token": "[UNK]"}}}}"#,
        entries.join(", ")
    )
}

/// The weights of the tiny model; `seed` makes another model (other values).
fn safetensors(seed: u8) -> Vec<u8> {
    let data: Vec<u8> = (0..VOCAB.len() * DIMENSION)
        .flat_map(|i| (1.0f32 + i as f32 + f32::from(seed) * 0.5).to_le_bytes())
        .collect();
    let mut header = format!(
        r#"{{"embeddings": {{"dtype": "F32", "shape": [{}, {DIMENSION}], "data_offsets": [0, {}]}}}}"#,
        VOCAB.len(),
        data.len()
    );
    while header.len() % 8 != 0 {
        header.push(' ');
    }
    let mut out = (header.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&data);
    out
}

/// The config of the tiny model.
const CONFIG: &str = r#"{"normalize": true}"#;

/// The tiny model `seed` of the repository `repo` as a [`NewModel`] (licence MIT, a card).
pub(crate) fn tiny_model(seed: u8, repo: &str) -> NewModel {
    NewModel::new()
        .with_file(
            "config.json",
            ModelRole::Config,
            Cursor::new(CONFIG.as_bytes().to_vec()),
        )
        .with_file(
            "tokenizer.json",
            ModelRole::Tokenizer,
            Cursor::new(tokenizer_json().into_bytes()),
        )
        .with_file(
            "model.safetensors",
            ModelRole::Weights,
            Cursor::new(safetensors(seed)),
        )
        .with_file(
            "README.md",
            ModelRole::Card,
            Cursor::new(b"---\nlicense: mit\n---\n# A tiny model\n".to_vec()),
        )
        .with_repo(repo)
        .with_license("mit")
}

/// The tiny model `seed` as the files of a Model2Vec directory in `dir` (created): `config.json`,
/// `tokenizer.json`, `model.safetensors` and a card naming the licence MIT.
pub(crate) fn tiny_model_dir(seed: u8, dir: &std::path::Path) {
    std::fs::create_dir_all(dir).expect("the model directory");
    std::fs::write(dir.join("config.json"), CONFIG).expect("config.json");
    std::fs::write(dir.join("tokenizer.json"), tokenizer_json()).expect("tokenizer.json");
    std::fs::write(dir.join("model.safetensors"), safetensors(seed)).expect("weights");
    std::fs::write(
        dir.join("README.md"),
        "---\nlicense: mit\n---\n# A tiny model\n",
    )
    .expect("README.md");
}

/// A fake Hugging Face endpoint on 127.0.0.1 serving the tiny model as the repository
/// `acme/tiny` at `main` and at its commit [`FakeHub::COMMIT`] (no network): the three model
/// files, a card and the repository's metadata with the licence `license`, each file answered
/// with `X-Repo-Commit`. `slow`: the weights are an endless trickle (a download that only a
/// cancel ends). Every other path is `404`. Each connection has a thread of its
/// own; the listener lives as long as the process (tests only).
pub(crate) struct FakeHub {
    /// `http://127.0.0.1:<port>`.
    pub(crate) url: String,
}

impl FakeHub {
    /// The commit `main` resolves to (`X-Repo-Commit`); the files are served under it too.
    pub(crate) const COMMIT: &'static str = "0123456789abcdef0123456789abcdef01234567";

    pub(crate) fn start(license: &str, slow: bool) -> FakeHub {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        let mut files: std::collections::HashMap<String, Vec<u8>> =
            std::collections::HashMap::new();
        // The same files at the branch `main` and at its commit `FakeHub::COMMIT`.
        for revision in ["main", Self::COMMIT] {
            let base = format!("/acme/tiny/resolve/{revision}");
            files.insert(format!("{base}/config.json"), CONFIG.as_bytes().to_vec());
            files.insert(
                format!("{base}/tokenizer.json"),
                tokenizer_json().into_bytes(),
            );
            files.insert(format!("{base}/model.safetensors"), safetensors(1));
            files.insert(
                format!("{base}/README.md"),
                format!("---\nlicense: {license}\n---\n# tiny\n").into_bytes(),
            );
            files.insert(
                format!("/api/models/acme/tiny/revision/{revision}"),
                format!(
                    r#"{{"cardData": {{"license": "{license}"}}, "gated": false,
                        "siblings": [{{"rfilename": "config.json"}}]}}"#
                )
                .into_bytes(),
            );
        }
        let files = std::sync::Arc::new(files);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let files = files.clone();
                std::thread::spawn(move || {
                    let Ok(clone) = stream.try_clone() else {
                        return;
                    };
                    let mut reader = BufReader::new(clone);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        return;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("");
                    let path = path.split('?').next().unwrap_or("").to_string();
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                            break;
                        }
                    }
                    if slow && path.ends_with("/model.safetensors") {
                        let total = 64 * 1024 * 1024;
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
                        );
                        if stream.write_all(head.as_bytes()).is_err() {
                            return;
                        }
                        let chunk = vec![0u8; 64 * 1024];
                        for _ in 0..(total / chunk.len()) {
                            if stream.write_all(&chunk).is_err() {
                                return;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                        return;
                    }
                    let (status, body) = match files.get(&path) {
                        Some(body) => ("200 OK", body.clone()),
                        None => ("404 Not Found", b"not found".to_vec()),
                    };
                    // The hub names the commit a revision resolves to on every file.
                    let commit = if path.contains("/resolve/") {
                        format!("X-Repo-Commit: {}\r\n", FakeHub::COMMIT)
                    } else {
                        String::new()
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{commit}Connection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                    let _ = stream.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                });
            }
        });
        FakeHub { url }
    }
}
