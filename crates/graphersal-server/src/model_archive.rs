//! A Model2Vec model directory as a tar archive (`models add model.tar`, the upload form of
//! `POST /api/admin/models`): its detection and a SAFE unpack into a new directory in `tmp/`.
//!
//! The format is POSIX ustar (`tar -cf model.tar -C potion-base-8M .`, or `tar -cf model.tar
//! potion-base-8M`; GNU tar and bsdtar write it, Windows 10+ ships bsdtar), uncompressed: the
//! weights (f32 safetensors) barely compress, and the server reads ustar already (a model blob
//! is one), so no dependency is needed. What the unpack takes:
//! - regular files and directories, at relative paths of plain names (a leading `./` and empty
//!   components are dropped); an absolute path, a `..` component, a backslash or a `:` in a
//!   name refuses the WHOLE archive, as do symbolic links, hard links, devices, FIFOs, GNU long
//!   names and base-256 sizes; pax headers (`x`, `g`) are skipped (their `path` is NOT used, so
//!   a name is always the ustar one), and so are macOS resource forks (`._*`);
//! - at most [`MAX_ENTRIES`] entries, [`MAX_DEPTH`] directory levels; a file written twice
//!   refuses the archive; every entry must lie inside the file and pass its header checksum.
//!   The bytes written are bounded by the archive's own length (the upload limit).
//!
//! The model directory is the archive's root, or its only top-level directory.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The most entries an archive may hold.
pub const MAX_ENTRIES: usize = 1000;
/// The deepest directory nesting of an entry.
pub const MAX_DEPTH: usize = 16;
/// The ustar block size.
const BLOCK: u64 = 512;

/// Whether the file at `path` starts with a ustar header (magic and checksum).
pub(crate) fn is_tar(path: &Path) -> bool {
    let mut header = [0u8; BLOCK as usize];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok()
        && header_ok(&header)
}

/// Unpacks the ustar archive `archive` into the new directory `dir` (module docs) and answers
/// the model directory: `dir`, or the archive's only top-level directory. `Err`: why the
/// archive is refused, in words (whatever was written stays in `dir` for the caller to
/// remove).
pub(crate) fn unpack(archive: &Path, dir: &Path) -> Result<PathBuf, String> {
    let mut file = File::open(archive).map_err(|err| format!("cannot read it: {err}"))?;
    let total = file
        .metadata()
        .map_err(|err| format!("cannot read it: {err}"))?
        .len();
    fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let mut header = [0u8; BLOCK as usize];
    let mut at = 0u64;
    let mut entries = 0usize;
    let mut files = 0usize;
    loop {
        if at + BLOCK > total {
            // No end-of-archive blocks: accepted when every entry was whole.
            break;
        }
        file.seek(SeekFrom::Start(at))
            .and_then(|_| file.read_exact(&mut header))
            .map_err(|err| format!("cannot read it: {err}"))?;
        if header.iter().all(|&b| b == 0) {
            break;
        }
        if !header_ok(&header) {
            return Err(format!("not a tar archive (a broken header at byte {at})"));
        }
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(format!("more than {MAX_ENTRIES} entries"));
        }
        let name = entry_name(&header);
        let size = octal(&header[124..136])
            .ok_or_else(|| format!("the entry {name:?} has a size this server does not read"))?;
        let offset = at + BLOCK;
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= total)
            .ok_or_else(|| format!("the entry {name:?} runs past the end of the archive"))?;
        match header[156] {
            b'0' | 0 | b'7' => {
                if let Some(parts) = relative_parts(&name)? {
                    if parts.last().is_some_and(|p| p.starts_with("._")) {
                        tracing::debug!(
                            target: "graphersal_server",
                            "model archive: skipped the resource fork {name:?}"
                        );
                    } else {
                        write_file(&mut file, offset, size, dir, &parts, &name)?;
                        files += 1;
                    }
                } else {
                    return Err(format!("the file entry {name:?} has no name"));
                }
            }
            b'5' => {
                if let Some(parts) = relative_parts(&name)? {
                    let path = join(dir, &parts);
                    fs::create_dir_all(&path)
                        .map_err(|err| format!("cannot create {}: {err}", path.display()))?;
                }
            }
            // pax headers: their records are skipped (a name is always the ustar one).
            b'x' | b'g' => {}
            b'1' => return Err(format!("the entry {name:?} is a hard link (refused)")),
            b'2' => return Err(format!("the entry {name:?} is a symbolic link (refused)")),
            b'L' | b'K' => {
                return Err(format!(
                    "the entry {name:?} uses a GNU long name (refused; write the archive in \
                     the ustar or pax format)"
                ));
            }
            other => {
                return Err(format!(
                    "the entry {name:?} is of type {:?}: only files and directories are taken",
                    other as char
                ));
            }
        }
        at = end
            .checked_add(size.div_ceil(BLOCK) * BLOCK - size)
            .ok_or_else(|| "a broken entry size".to_string())?;
    }
    if files == 0 {
        return Err("it holds no file".to_string());
    }
    model_root(dir)
}

/// The model directory in `dir`: its only top-level directory when it holds nothing else.
fn model_root(dir: &Path) -> Result<PathBuf, String> {
    let listing: Vec<fs::DirEntry> = fs::read_dir(dir)
        .map_err(|err| format!("cannot read {}: {err}", dir.display()))?
        .flatten()
        .collect();
    match listing.as_slice() {
        [only] if only.file_type().is_ok_and(|t| t.is_dir()) => Ok(only.path()),
        _ => Ok(dir.to_path_buf()),
    }
}

/// Writes the entry's `size` bytes at `offset` of `archive` to the NEW file `parts` below `dir`.
fn write_file(
    archive: &mut File,
    offset: u64,
    size: u64,
    dir: &Path,
    parts: &[String],
    name: &str,
) -> Result<(), String> {
    let path = join(dir, parts);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|err| match err.kind() {
            io::ErrorKind::AlreadyExists => format!("the file {name:?} is in it twice"),
            _ => format!("cannot write {}: {err}", path.display()),
        })?;
    archive
        .seek(SeekFrom::Start(offset))
        .map_err(|err| format!("cannot read it: {err}"))?;
    let copied = io::copy(&mut archive.take(size), &mut out)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    if copied != size {
        return Err(format!("the file {name:?} is truncated"));
    }
    Ok(())
}

/// `dir` joined with `parts`, one component at a time.
fn join(dir: &Path, parts: &[String]) -> PathBuf {
    let mut path = dir.to_path_buf();
    for part in parts {
        path.push(part);
    }
    path
}

/// The plain name components of an entry name (`./` and empty components dropped); `Ok(None)`
/// for the archive root itself; `Err` for a name that could leave the directory.
fn relative_parts(name: &str) -> Result<Option<Vec<String>>, String> {
    if name.starts_with('/') {
        return Err(format!("the entry {name:?} has an absolute path (refused)"));
    }
    if name.contains('\\') || name.contains(':') {
        return Err(format!(
            "the entry {name:?} has a backslash or a colon in its name (refused)"
        ));
    }
    let mut parts = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(format!(
                    "the entry {name:?} leaves the archive's root (refused)"
                ));
            }
            part if part.len() > 255 || part.chars().any(char::is_control) => {
                return Err(format!("the entry {name:?} has an unusable name (refused)"));
            }
            part => parts.push(part.to_string()),
        }
    }
    if parts.len() > MAX_DEPTH {
        return Err(format!(
            "the entry {name:?} is nested deeper than {MAX_DEPTH} directories"
        ));
    }
    Ok((!parts.is_empty()).then_some(parts))
}

/// The entry's name: the ustar prefix (bytes 345..500) and name (0..100).
fn entry_name(header: &[u8; BLOCK as usize]) -> String {
    let field = |bytes: &[u8]| {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    };
    let name = field(&header[..100]);
    let prefix = field(&header[345..500]);
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

/// Whether `header` has the ustar magic and a matching checksum.
fn header_ok(header: &[u8; BLOCK as usize]) -> bool {
    if &header[257..262] != b"ustar" {
        return false;
    }
    let Some(stored) = octal(&header[148..156]) else {
        return false;
    };
    let sum: u64 = header
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(b)
            }
        })
        .sum();
    sum == stored
}

/// An octal header field (NUL- or space-terminated, leading spaces allowed); `None` for
/// anything else (a GNU base-256 number included).
fn octal(field: &[u8]) -> Option<u64> {
    let text: String = field
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect();
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// One ustar entry: `name`, type flag, data.
    pub(crate) fn entry(name: &str, kind: u8, data: &[u8]) -> Vec<u8> {
        let mut header = [0u8; BLOCK as usize];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(b"0000644");
        header[108..115].copy_from_slice(b"0000000");
        header[116..123].copy_from_slice(b"0000000");
        header[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        header[136..147].copy_from_slice(b"00000000000");
        header[156] = kind;
        if kind == b'2' || kind == b'1' {
            header[157..163].copy_from_slice(b"target");
        }
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        let mut out = header.to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(BLOCK as usize) * BLOCK as usize, 0);
        out
    }

    /// A whole archive of `entries` (two zero blocks at the end).
    pub(crate) fn archive(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut out: Vec<u8> = entries.concat();
        out.extend_from_slice(&[0u8; 2 * BLOCK as usize]);
        out
    }

    /// The tiny model directory `seed` as a tar archive below `top/` (or at the root).
    pub(crate) fn model_tar(seed: u8, top: Option<&str>) -> Vec<u8> {
        let temp = TempDir::new("model-tar");
        crate::test_models::tiny_model_dir(seed, temp.path());
        let mut entries = Vec::new();
        if let Some(top) = top {
            entries.push(entry(&format!("{top}/"), b'5', b""));
        }
        let mut names: Vec<String> = fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            let data = fs::read(temp.path().join(&name)).unwrap();
            let path = match top {
                Some(top) => format!("{top}/{name}"),
                None => format!("./{name}"),
            };
            entries.push(entry(&path, b'0', &data));
        }
        archive(&entries)
    }

    fn unpack_bytes(bytes: &[u8]) -> (TempDir, Result<PathBuf, String>) {
        let temp = TempDir::new("model-archive");
        let file = temp.path().join("a.tar");
        fs::write(&file, bytes).unwrap();
        let out = temp.path().join("out");
        let result = unpack(&file, &out);
        (temp, result)
    }

    #[test]
    fn a_model_directory_unpacks_at_the_root_or_below_its_top_directory() {
        let (temp, root) = unpack_bytes(&model_tar(1, None));
        let root = root.expect("unpacked");
        assert_eq!(root, temp.path().join("out"));
        assert!(root.join("model.safetensors").is_file());
        assert!(is_tar(&temp.path().join("a.tar")));
        let (temp, root) = unpack_bytes(&model_tar(1, Some("potion")));
        assert_eq!(root.unwrap(), temp.path().join("out").join("potion"));
        // A resource fork and a pax header are skipped; a nested file is kept.
        let bytes = archive(&[
            entry("PaxHeader/x", b'x', b"20 path=../../evil\n"),
            entry("._config.json", b'0', b"fork"),
            entry("config.json", b'0', b"{}"),
            entry("sub/inner.txt", b'0', b"x"),
        ]);
        let (temp, root) = unpack_bytes(&bytes);
        let root = root.expect("unpacked");
        assert!(!root.join("._config.json").exists());
        assert!(root.join("sub").join("inner.txt").is_file());
        assert!(!temp.path().join("evil").exists());
    }

    #[test]
    fn malicious_entries_refuse_the_archive() {
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (archive(&[entry("/etc/passwd", b'0', b"x")]), "absolute"),
            (archive(&[entry("../evil", b'0', b"x")]), "leaves"),
            (archive(&[entry("a/../../evil", b'0', b"x")]), "leaves"),
            (archive(&[entry("a\\..\\evil", b'0', b"x")]), "backslash"),
            (archive(&[entry("C:evil", b'0', b"x")]), "colon"),
            (archive(&[entry("link", b'2', b"")]), "symbolic link"),
            (archive(&[entry("hard", b'1', b"")]), "hard link"),
            (archive(&[entry("dev", b'3', b"")]), "only files"),
            (archive(&[entry("fifo", b'6', b"")]), "only files"),
            (
                archive(&[entry("././@LongLink", b'L', b"x")]),
                "GNU long name",
            ),
            (
                archive(&[entry("a", b'0', b"1"), entry("a", b'0', b"2")]),
                "twice",
            ),
            (archive(&[entry("dir/", b'5', b"")]), "no file"),
            (
                archive(&[entry(&"d/".repeat(MAX_DEPTH + 1), b'5', b"")]),
                "deeper",
            ),
        ];
        for (bytes, expected) in cases {
            let (temp, result) = unpack_bytes(&bytes);
            let err = result.expect_err(expected);
            assert!(err.contains(expected), "{expected}: {err}");
            assert!(!temp.path().join("evil").exists());
            assert!(!temp.path().parent().unwrap().join("evil").exists());
        }
        // An entry past the end; a broken checksum; too many entries.
        let mut cut = entry("big", b'0', &[7u8; 2000]);
        cut.truncate(1024);
        let (_t, result) = unpack_bytes(&cut);
        assert!(result.unwrap_err().contains("past the end"));
        let mut broken = archive(&[entry("a", b'0', b"x")]);
        broken[0] = b'b';
        let (_t, result) = unpack_bytes(&broken);
        assert!(result.unwrap_err().contains("not a tar archive"));
        let many: Vec<Vec<u8>> = (0..=MAX_ENTRIES)
            .map(|i| entry(&format!("f{i}"), b'0', b""))
            .collect();
        let (_t, result) = unpack_bytes(&archive(&many));
        assert!(result.unwrap_err().contains("more than"));
        let temp = TempDir::new("model-archive-not");
        let other = temp.path().join("x.bin");
        fs::write(&other, [b'{'; 600]).unwrap();
        assert!(!is_tar(&other));
    }
}
