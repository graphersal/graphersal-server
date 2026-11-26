//! The ZIP container of a server backup ([`crate::server_backup`]): stored entries (the
//! configuration is small and the embedded database backups are ZIPs of compressed chunks
//! already), zip64 records always, so any size works; written in one pass, read in one pass.
//! The same subset of the ZIP format (APPNOTE 6.3.x) as graphersal's own backup archives, no
//! dependency: CRC-32 for the container, SHA-256 (`ring`) of every entry for the manifest.
//!
//! Reading checks each entry's CRC-32 over all of its bytes and answers its SHA-256; the
//! caller compares that with the manifest. Compressed, encrypted or streamed entries, a
//! repeated name and a structure that ends early are refused (`Err` with the reason): this
//! reader only reads archives [`ZipWriter`] wrote.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const ZIP64_END: u32 = 0x0606_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const END_OF_CENTRAL: u32 = 0x0605_4b50;
/// Version 4.5: zip64.
const VERSION: u16 = 45;
/// General purpose flag bit 11: the name is UTF-8.
const FLAG_UTF8: u16 = 0x0800;
/// 1980-01-01 00:00 in DOS date/time (the manifest carries the time).
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = (1 << 5) | 1;
/// The longest entry name accepted.
const MAX_NAME: usize = 1024;

/// CRC-32 (IEEE 802.3, the ZIP checksum).
fn crc32_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        table
    })
}

#[derive(Clone, Copy)]
struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Crc32(0xFFFF_FFFF)
    }

    fn update(&mut self, bytes: &[u8]) {
        let table = crc32_table();
        let mut c = self.0;
        for &b in bytes {
            c = table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }

    fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

/// The SHA-256 of `bytes` as 64 lower-case hex digits.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One entry written: its name, length and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Written {
    pub(crate) name: String,
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
}

struct CentralEntry {
    name: String,
    crc: u32,
    size: u64,
    offset: u64,
}

/// Writes a stored, zip64 archive in one pass.
pub(crate) struct ZipWriter<W: Write> {
    out: W,
    offset: u64,
    entries: Vec<CentralEntry>,
}

impl<W: Write> ZipWriter<W> {
    pub(crate) fn new(out: W) -> Self {
        Self {
            out,
            offset: 0,
            entries: Vec::new(),
        }
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    fn local_header(&mut self, name: &str, crc: u32, size: u64) -> io::Result<()> {
        let mut h = Vec::with_capacity(30 + name.len() + 20);
        h.extend_from_slice(&LOCAL_HEADER.to_le_bytes());
        h.extend_from_slice(&VERSION.to_le_bytes());
        h.extend_from_slice(&FLAG_UTF8.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // stored
        h.extend_from_slice(&DOS_TIME.to_le_bytes());
        h.extend_from_slice(&DOS_DATE.to_le_bytes());
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&u32::MAX.to_le_bytes()); // sizes in the zip64 extra field
        h.extend_from_slice(&u32::MAX.to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(name.as_bytes());
        h.extend_from_slice(&1u16.to_le_bytes()); // zip64 extended information
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        self.put(&h)
    }

    fn check_name(name: &str) -> io::Result<()> {
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("archive entry name {name:?} is empty or too long"),
            ));
        }
        Ok(())
    }

    /// Adds an entry from memory.
    pub(crate) fn add_bytes(&mut self, name: &str, bytes: &[u8]) -> io::Result<Written> {
        Self::check_name(name)?;
        let mut crc = Crc32::new();
        crc.update(bytes);
        let crc = crc.finish();
        let offset = self.offset;
        self.local_header(name, crc, bytes.len() as u64)?;
        self.put(bytes)?;
        self.entries.push(CentralEntry {
            name: name.to_string(),
            crc,
            size: bytes.len() as u64,
            offset,
        });
        Ok(Written {
            name: name.to_string(),
            bytes: bytes.len() as u64,
            sha256: sha256_hex(bytes),
        })
    }

    /// Adds the file `path` (read twice: the CRC goes into the local header, so a one-pass
    /// reader can check every entry; the second read must see the same bytes, else the file
    /// changed while it was copied and the write fails).
    pub(crate) fn add_file(&mut self, name: &str, path: &Path) -> io::Result<Written> {
        Self::check_name(name)?;
        let mut buf = vec![0u8; 1 << 16];
        let mut crc = Crc32::new();
        let mut sha = ring::digest::Context::new(&ring::digest::SHA256);
        let mut len = 0u64;
        let mut file = File::open(path)?;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            crc.update(&buf[..n]);
            sha.update(&buf[..n]);
            len += n as u64;
        }
        let crc = crc.finish();
        let offset = self.offset;
        self.local_header(name, crc, len)?;
        let mut file = File::open(path)?.take(len);
        let mut check = Crc32::new();
        let mut copied = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            check.update(&buf[..n]);
            self.out.write_all(&buf[..n])?;
            self.offset += n as u64;
            copied += n as u64;
        }
        if copied != len || check.finish() != crc {
            return Err(io::Error::other(format!(
                "{} changed while it was copied",
                path.display()
            )));
        }
        self.entries.push(CentralEntry {
            name: name.to_string(),
            crc,
            size: len,
            offset,
        });
        Ok(Written {
            name: name.to_string(),
            bytes: len,
            sha256: hex(sha.finish().as_ref()),
        })
    }

    /// Writes the central directory and the zip64 end records; answers the writer.
    pub(crate) fn finish(mut self) -> io::Result<W> {
        let central_start = self.offset;
        let entries = std::mem::take(&mut self.entries);
        for entry in &entries {
            let mut h = Vec::with_capacity(46 + entry.name.len() + 28);
            h.extend_from_slice(&CENTRAL_HEADER.to_le_bytes());
            h.extend_from_slice(&(VERSION | (3 << 8)).to_le_bytes()); // made by: UNIX
            h.extend_from_slice(&VERSION.to_le_bytes());
            h.extend_from_slice(&FLAG_UTF8.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&DOS_TIME.to_le_bytes());
            h.extend_from_slice(&DOS_DATE.to_le_bytes());
            h.extend_from_slice(&entry.crc.to_le_bytes());
            h.extend_from_slice(&u32::MAX.to_le_bytes());
            h.extend_from_slice(&u32::MAX.to_le_bytes());
            h.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
            h.extend_from_slice(&28u16.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes()); // comment
            h.extend_from_slice(&0u16.to_le_bytes()); // disk
            h.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            h.extend_from_slice(&(0o100600u32 << 16).to_le_bytes()); // external: rw-------
            h.extend_from_slice(&u32::MAX.to_le_bytes()); // offset in the zip64 field
            h.extend_from_slice(entry.name.as_bytes());
            h.extend_from_slice(&1u16.to_le_bytes());
            h.extend_from_slice(&24u16.to_le_bytes());
            h.extend_from_slice(&entry.size.to_le_bytes());
            h.extend_from_slice(&entry.size.to_le_bytes());
            h.extend_from_slice(&entry.offset.to_le_bytes());
            self.put(&h)?;
        }
        let central_size = self.offset - central_start;
        let zip64_end = self.offset;
        let count = entries.len() as u64;
        let mut t = Vec::with_capacity(56 + 20 + 22);
        t.extend_from_slice(&ZIP64_END.to_le_bytes());
        t.extend_from_slice(&44u64.to_le_bytes());
        t.extend_from_slice(&(VERSION | (3 << 8)).to_le_bytes());
        t.extend_from_slice(&VERSION.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&count.to_le_bytes());
        t.extend_from_slice(&count.to_le_bytes());
        t.extend_from_slice(&central_size.to_le_bytes());
        t.extend_from_slice(&central_start.to_le_bytes());
        t.extend_from_slice(&ZIP64_LOCATOR.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&zip64_end.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&END_OF_CENTRAL.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        t.extend_from_slice(&u16::MAX.to_le_bytes());
        t.extend_from_slice(&u16::MAX.to_le_bytes());
        t.extend_from_slice(&u32::MAX.to_le_bytes());
        t.extend_from_slice(&u32::MAX.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        self.put(&t)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

fn bad(offset: u64, reason: impl Into<String>) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("at byte {offset}: {}", reason.into()),
    )
}

fn read_array<const N: usize>(source: &mut impl Read, at: u64) -> io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    source
        .read_exact(&mut buf)
        .map_err(|_| bad(at, "the archive ends inside a header"))?;
    Ok(buf)
}

/// Reads an archive [`ZipWriter`] wrote, entry by entry, in one pass.
pub(crate) struct ZipReader<R: Read> {
    inner: R,
    offset: u64,
    seen: HashSet<String>,
}

/// One entry of a [`ZipReader`]: read its bytes (at most its size), then [`Entry::finish`].
pub(crate) struct Entry<'a, R: Read> {
    reader: &'a mut ZipReader<R>,
    /// The entry's name.
    pub(crate) name: String,
    /// Its size in bytes.
    pub(crate) size: u64,
    crc: u32,
    left: u64,
    check: Crc32,
    sha: ring::digest::Context,
}

impl<R: Read> ZipReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            offset: 0,
            seen: HashSet::new(),
        }
    }

    /// The next entry; `None` at the central directory (the end of the entries).
    pub(crate) fn next_entry(&mut self) -> io::Result<Option<Entry<'_, R>>> {
        let offset = self.offset;
        let signature = u32::from_le_bytes(read_array::<4>(&mut self.inner, offset)?);
        if signature == CENTRAL_HEADER {
            return Ok(None);
        }
        if signature != LOCAL_HEADER {
            return Err(bad(
                offset,
                "not a local file header (not a server backup?)",
            ));
        }
        let h = read_array::<26>(&mut self.inner, offset + 4)?;
        let u16_at = |i: usize| u16::from_le_bytes([h[i], h[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
        let flags = u16_at(2);
        let method = u16_at(4);
        let crc = u32_at(10);
        let (mut stored, mut size) = (u64::from(u32_at(14)), u64::from(u32_at(18)));
        let name_len = u16_at(22) as usize;
        let extra_len = u16_at(24) as usize;
        if method != 0 || flags & 0x0008 != 0 || flags & 0x0001 != 0 {
            return Err(bad(
                offset,
                "a compressed, encrypted or streamed entry (not a server backup)",
            ));
        }
        if name_len == 0 || name_len > MAX_NAME {
            return Err(bad(offset, "an empty or too long entry name"));
        }
        let mut name = vec![0u8; name_len];
        self.inner
            .read_exact(&mut name)
            .map_err(|_| bad(offset, "the archive ends inside a name"))?;
        let mut extra = vec![0u8; extra_len];
        self.inner
            .read_exact(&mut extra)
            .map_err(|_| bad(offset, "the archive ends inside an extra field"))?;
        let name = String::from_utf8(name).map_err(|_| bad(offset, "a name that is not UTF-8"))?;
        if !self.seen.insert(name.clone()) {
            return Err(bad(offset, format!("repeated entry {name:?}")));
        }
        let mut at = 0;
        while at + 4 <= extra.len() {
            let id = u16::from_le_bytes([extra[at], extra[at + 1]]);
            let len = u16::from_le_bytes([extra[at + 2], extra[at + 3]]) as usize;
            let body = extra
                .get(at + 4..at + 4 + len)
                .ok_or_else(|| bad(offset, "an extra field runs past its block"))?;
            if id == 1 {
                let mut words = body
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|c| u64::from_le_bytes(*c));
                if size == u64::from(u32::MAX) {
                    size = words
                        .next()
                        .ok_or_else(|| bad(offset, "zip64 size missing"))?;
                }
                if stored == u64::from(u32::MAX) {
                    stored = words
                        .next()
                        .ok_or_else(|| bad(offset, "zip64 size missing"))?;
                }
            }
            at += 4 + len;
        }
        if stored != size {
            return Err(bad(offset, "stored and uncompressed sizes differ"));
        }
        self.offset += 30 + name_len as u64 + extra_len as u64;
        Ok(Some(Entry {
            reader: self,
            name,
            size,
            crc,
            left: size,
            check: Crc32::new(),
            sha: ring::digest::Context::new(&ring::digest::SHA256),
        }))
    }
}

impl<R: Read> Read for Entry<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        if want == 0 {
            return Ok(0);
        }
        let n = self.reader.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("the archive ends inside {:?}", self.name),
            ));
        }
        self.check.update(&buf[..n]);
        self.sha.update(&buf[..n]);
        self.left -= n as u64;
        self.reader.offset += n as u64;
        Ok(n)
    }
}

impl<R: Read> Entry<'_, R> {
    /// Reads what is left of the entry, checks its CRC-32 and answers its SHA-256 (64 hex
    /// digits). `Err` (`InvalidData`) on a CRC mismatch.
    pub(crate) fn finish(mut self) -> io::Result<String> {
        io::copy(&mut self, &mut io::sink())?;
        if self.check.finish() != self.crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{:?}: CRC-32 mismatch (the archive is damaged)", self.name),
            ));
        }
        Ok(hex(self.sha.finish().as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_reference_value() {
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.finish(), 0xCBF4_3926);
    }

    #[test]
    fn an_archive_reads_back_with_its_hashes_and_a_damaged_byte_is_found() {
        let dir = crate::test_support::TempDir::new("archive");
        let file = dir.path().join("f.bin");
        std::fs::write(&file, vec![7u8; 200_000]).unwrap();
        let mut zip = ZipWriter::new(Vec::new());
        let a = zip.add_bytes("a.toml", b"x = 1\n").unwrap();
        let b = zip.add_file("dir/f.bin", &file).unwrap();
        let bytes = zip.finish().unwrap();
        assert_eq!(a.sha256, sha256_hex(b"x = 1\n"));
        assert_eq!(b.bytes, 200_000);
        let mut reader = ZipReader::new(bytes.as_slice());
        let mut seen = Vec::new();
        while let Some(entry) = reader.next_entry().unwrap() {
            let name = entry.name.clone();
            seen.push((name, entry.finish().unwrap()));
        }
        assert_eq!(
            seen,
            vec![
                ("a.toml".to_string(), a.sha256.clone()),
                ("dir/f.bin".to_string(), b.sha256.clone())
            ]
        );
        // One byte of the file's data flipped: the CRC finds it.
        let mut damaged = bytes.clone();
        let at = damaged.len() / 2;
        damaged[at] ^= 0xFF;
        let mut reader = ZipReader::new(damaged.as_slice());
        let first = reader.next_entry().unwrap().unwrap();
        first.finish().unwrap();
        let second = reader.next_entry().unwrap().unwrap();
        let err = second.finish().unwrap_err();
        assert!(err.to_string().contains("CRC-32"), "{err}");
    }
}
