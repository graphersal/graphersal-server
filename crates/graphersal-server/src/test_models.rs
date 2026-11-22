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
