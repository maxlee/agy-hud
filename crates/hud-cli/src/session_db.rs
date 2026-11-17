use crate::paths::{atomic_write_bytes, read_bounded_string};
use hud_core::stdin::canonical_db_family_key;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const SQLITE_HEADER_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const SQLITE_HEADER_BYTES: usize = 100;
const WAL_HEADER_BYTES: usize = 32;
const WAL_FRAME_HEADER_BYTES: usize = 24;
const WAL_MAGIC_LE_CHECKSUM: u32 = 0x377f_0682;
const WAL_MAGIC_BE_CHECKSUM: u32 = 0x377f_0683;
const WAL_FORMAT_VERSION: u32 = 3_007_000;

const MIN_USABLE_PAGE_SIZE: usize = 480;
const MAX_OVERFLOW_HOPS: usize = 4096;
const MAX_BTREE_DEPTH: usize = 32;
const MAX_GEN_SCAN_ROWS: usize = 16;
const MAX_EXEC_SCAN_ROWS: usize = 4;
const MAX_PB_STR_BYTES: u64 = 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024;
const DB_CEILING_CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbCeilingOrigin {
    GenMetadata,
    ExecutorMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbCeilingObservation {
    pub ceiling_tokens: u64,
    pub origin: DbCeilingOrigin,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model_enum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subagent_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DbCeilingCacheRecord {
    version: u32,
    db_path: String,
    db_mtime_ns: u64,
    db_len: u64,
    wal_exists: bool,
    wal_mtime_ns: u64,
    wal_len: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    hint_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    observation: Option<DbCeilingObservation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStat {
    mtime_ns: u64,
    len: u64,
}

fn read_file_stat(path: &Path) -> Option<FileStat> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .unwrap_or(0);
    Some(FileStat {
        mtime_ns,
        len: meta.len(),
    })
}

fn wal_path_for(db_path: &Path) -> PathBuf {
    let mut os = db_path.as_os_str().to_os_string();
    os.push("-wal");
    PathBuf::from(os)
}

fn models_match(hint: &str, candidate: &str) -> bool {
    let ht = hint.trim();
    let ct = candidate.trim();
    if ht.is_empty() || ct.is_empty() {
        return false;
    }
    if ht.eq_ignore_ascii_case(ct) {
        return true;
    }
    match (canonical_db_family_key(ht), canonical_db_family_key(ct)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Reads the newest valid ceiling observation directly from a SQLite conversation DB (and its
/// `-wal` overlay if committed frames exist), strictly read-only without creating `-shm` or locks.
pub fn read_db_ceiling_from_path(
    db_path: &Path,
    active_model_hint: Option<&str>,
    is_shadow: fn(&str) -> bool,
) -> io::Result<Option<DbCeilingObservation>> {
    match read_db_ceiling_outcome(db_path, active_model_hint, is_shadow) {
        DbReadOutcome::Parsed(obs) => Ok(obs),
        DbReadOutcome::MissingOrInvalid => Ok(None),
    }
}

/// Cache-backed conversation DB ceiling reader for 300ms HUD ticks. Skips all SQLite/WAL parsing
/// when `(db_mtime_ns, db_len, wal_mtime_ns, wal_len, active_model_hint)` matches `<data_real_dir>/db-ceilings/*.json`.
pub fn read_db_ceiling_cached(
    db_path: &Path,
    cache_path_opt: Option<&Path>,
    active_model_hint: Option<&str>,
    is_shadow: fn(&str) -> bool,
) -> Option<DbCeilingObservation> {
    let db_stat = read_file_stat(db_path)?;
    let wal_stat = read_file_stat(&wal_path_for(db_path));
    let (wal_exists, wal_mtime_ns, wal_len) = match wal_stat {
        Some(st) => (true, st.mtime_ns, st.len),
        None => (false, 0, 0),
    };
    let hint_key = active_model_hint.and_then(canonical_db_family_key);
    let db_path_str = db_path.to_string_lossy();

    if let Some(cache_path) = cache_path_opt
        && let Some(raw) = read_bounded_string(cache_path, MAX_CACHE_BYTES)
        && let Ok(cached) = serde_json::from_str::<DbCeilingCacheRecord>(&raw)
        && cached.version == DB_CEILING_CACHE_VERSION
        && cached.db_path == db_path_str.as_ref()
        && cached.db_mtime_ns == db_stat.mtime_ns
        && cached.db_len == db_stat.len
        && cached.wal_exists == wal_exists
        && cached.wal_mtime_ns == wal_mtime_ns
        && cached.wal_len == wal_len
        && cached.hint_key == hint_key
    {
        return cached.observation;
    }

    match read_db_ceiling_outcome(db_path, active_model_hint, is_shadow) {
        DbReadOutcome::Parsed(obs) => {
            if let Some(cache_path) = cache_path_opt {
                let record = DbCeilingCacheRecord {
                    version: DB_CEILING_CACHE_VERSION,
                    db_path: db_path_str.into_owned(),
                    db_mtime_ns: db_stat.mtime_ns,
                    db_len: db_stat.len,
                    wal_exists,
                    wal_mtime_ns,
                    wal_len,
                    hint_key,
                    observation: obs.clone(),
                };
                if let Ok(bytes) = serde_json::to_vec(&record) {
                    let _ = atomic_write_bytes(cache_path, &bytes, 0o600);
                }
            }
            obs
        }
        DbReadOutcome::MissingOrInvalid => None,
    }
}

enum DbReadOutcome {
    Parsed(Option<DbCeilingObservation>),
    MissingOrInvalid,
}

fn read_db_ceiling_outcome(
    db_path: &Path,
    active_model_hint: Option<&str>,
    is_shadow: fn(&str) -> bool,
) -> DbReadOutcome {
    let Ok(mut source) = SqlitePageSource::open(db_path) else {
        return DbReadOutcome::MissingOrInvalid;
    };
    let Some(tables) = read_schema_rootpages(&mut source) else {
        return DbReadOutcome::MissingOrInvalid;
    };

    if let Some(gen_root) = tables.gen_metadata {
        match scan_gen_metadata(&mut source, gen_root, active_model_hint, is_shadow) {
            Ok(Some(obs)) => return DbReadOutcome::Parsed(Some(obs)),
            Ok(None) => {}
            Err(()) => return DbReadOutcome::MissingOrInvalid,
        }
    }

    if let Some(exec_root) = tables.executor_metadata {
        match scan_executor_metadata(&mut source, exec_root) {
            Ok(obs) => return DbReadOutcome::Parsed(obs),
            Err(()) => return DbReadOutcome::MissingOrInvalid,
        }
    }

    DbReadOutcome::Parsed(None)
}

struct SqlitePageSource {
    db_file: File,
    db_len: u64,
    wal_file: Option<File>,
    wal_len: u64,
    wal_map: HashMap<u32, u64>,
    page_size: usize,
    usable_size: usize,
}

impl SqlitePageSource {
    fn open(db_path: &Path) -> Result<Self, ()> {
        let meta = fs::metadata(db_path).map_err(|_| ())?;
        if !meta.is_file() || meta.len() < SQLITE_HEADER_BYTES as u64 {
            return Err(());
        }
        let db_len = meta.len();
        let mut db_file = File::open(db_path).map_err(|_| ())?;
        let mut hdr = [0u8; SQLITE_HEADER_BYTES];
        db_file.read_exact(&mut hdr).map_err(|_| ())?;

        if &hdr[0..16] != SQLITE_HEADER_MAGIC {
            return Err(());
        }
        let raw_ps = u16::from_be_bytes([hdr[16], hdr[17]]) as usize;
        let page_size = if raw_ps == 1 { 65_536 } else { raw_ps };
        if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
            return Err(());
        }
        let reserved = hdr[20] as usize;
        let usable_size = page_size.checked_sub(reserved).ok_or(())?;
        if usable_size < MIN_USABLE_PAGE_SIZE {
            return Err(());
        }

        let wal_path = wal_path_for(db_path);
        let (wal_file, wal_len, wal_map) = load_wal_overlay(&wal_path, page_size);

        Ok(Self {
            db_file,
            db_len,
            wal_file,
            wal_len,
            wal_map,
            page_size,
            usable_size,
        })
    }

    fn read_page(&mut self, pgno: u32, buf: &mut Vec<u8>) -> Result<(), ()> {
        if pgno == 0 {
            return Err(());
        }
        buf.resize(self.page_size, 0);
        if let Some(&wal_off) = self.wal_map.get(&pgno) {
            let wal = self.wal_file.as_mut().ok_or(())?;
            if wal_off.saturating_add(self.page_size as u64) > self.wal_len {
                return Err(());
            }
            wal.seek(SeekFrom::Start(wal_off)).map_err(|_| ())?;
            wal.read_exact(buf).map_err(|_| ())?;
            return Ok(());
        }
        let db_off = (pgno as u64 - 1)
            .checked_mul(self.page_size as u64)
            .ok_or(())?;
        if db_off.saturating_add(self.page_size as u64) > self.db_len {
            return Err(());
        }
        self.db_file.seek(SeekFrom::Start(db_off)).map_err(|_| ())?;
        self.db_file.read_exact(buf).map_err(|_| ())?;
        Ok(())
    }

    /// Reads only the 4-byte `next_ov_pgno` header of an overflow page when skipping large blobs.
    fn read_overflow_next_pgno(&mut self, pgno: u32, min_payload_bytes: u64) -> Result<u32, ()> {
        if pgno == 0 {
            return Err(());
        }
        let needed = 4u64.saturating_add(min_payload_bytes);
        let mut hdr4 = [0u8; 4];
        if let Some(&wal_off) = self.wal_map.get(&pgno) {
            let wal = self.wal_file.as_mut().ok_or(())?;
            if wal_off.saturating_add(needed) > self.wal_len {
                return Err(());
            }
            wal.seek(SeekFrom::Start(wal_off)).map_err(|_| ())?;
            wal.read_exact(&mut hdr4).map_err(|_| ())?;
            return Ok(u32::from_be_bytes(hdr4));
        }
        let db_off = (pgno as u64 - 1)
            .checked_mul(self.page_size as u64)
            .ok_or(())?;
        if db_off.saturating_add(needed) > self.db_len {
            return Err(());
        }
        self.db_file.seek(SeekFrom::Start(db_off)).map_err(|_| ())?;
        self.db_file.read_exact(&mut hdr4).map_err(|_| ())?;
        Ok(u32::from_be_bytes(hdr4))
    }

    /// Reads the 4-byte `next_ov_pgno` plus `take` payload bytes from an overflow page.
    fn read_overflow_chunk(
        &mut self,
        pgno: u32,
        take: usize,
        out_chunk: &mut Vec<u8>,
    ) -> Result<u32, ()> {
        if pgno == 0 || take > self.usable_size.saturating_sub(4) {
            return Err(());
        }
        let needed = 4u64.saturating_add(take as u64);
        let mut hdr4 = [0u8; 4];
        out_chunk.resize(take, 0);
        if let Some(&wal_off) = self.wal_map.get(&pgno) {
            let wal = self.wal_file.as_mut().ok_or(())?;
            if wal_off.saturating_add(needed) > self.wal_len {
                return Err(());
            }
            wal.seek(SeekFrom::Start(wal_off)).map_err(|_| ())?;
            wal.read_exact(&mut hdr4).map_err(|_| ())?;
            wal.read_exact(out_chunk).map_err(|_| ())?;
            return Ok(u32::from_be_bytes(hdr4));
        }
        let db_off = (pgno as u64 - 1)
            .checked_mul(self.page_size as u64)
            .ok_or(())?;
        if db_off.saturating_add(needed) > self.db_len {
            return Err(());
        }
        self.db_file.seek(SeekFrom::Start(db_off)).map_err(|_| ())?;
        self.db_file.read_exact(&mut hdr4).map_err(|_| ())?;
        self.db_file.read_exact(out_chunk).map_err(|_| ())?;
        Ok(u32::from_be_bytes(hdr4))
    }
}

fn wal_checksum(data: &[u8], mut s0: u32, mut s1: u32, big_endian_words: bool) -> (u32, u32) {
    let (chunks, _) = data.as_chunks::<8>();
    for chunk in chunks {
        let (x0, x1) = if big_endian_words {
            (
                u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
                u32::from_be_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]),
            )
        } else {
            (
                u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
                u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]),
            )
        };
        s0 = s0.wrapping_add(x0).wrapping_add(s1);
        s1 = s1.wrapping_add(x1).wrapping_add(s0);
    }
    (s0, s1)
}

fn load_wal_overlay(wal_path: &Path, page_size: usize) -> (Option<File>, u64, HashMap<u32, u64>) {
    let Ok(meta) = fs::metadata(wal_path) else {
        return (None, 0, HashMap::new());
    };
    let wal_len = meta.len();
    if !meta.is_file() || wal_len < WAL_HEADER_BYTES as u64 {
        return (None, 0, HashMap::new());
    }
    let Ok(mut wal_file) = File::open(wal_path) else {
        return (None, 0, HashMap::new());
    };

    let mut wh = [0u8; WAL_HEADER_BYTES];
    if wal_file.read_exact(&mut wh).is_err() {
        return (None, 0, HashMap::new());
    }
    let magic = u32::from_be_bytes([wh[0], wh[1], wh[2], wh[3]]);
    let big_endian_words = match magic {
        WAL_MAGIC_LE_CHECKSUM => false,
        WAL_MAGIC_BE_CHECKSUM => true,
        _ => return (None, 0, HashMap::new()),
    };
    let format = u32::from_be_bytes([wh[4], wh[5], wh[6], wh[7]]);
    let wal_ps = u32::from_be_bytes([wh[8], wh[9], wh[10], wh[11]]) as usize;
    if format != WAL_FORMAT_VERSION || wal_ps != page_size {
        return (None, 0, HashMap::new());
    }

    let hdr_c0 = u32::from_be_bytes([wh[24], wh[25], wh[26], wh[27]]);
    let hdr_c1 = u32::from_be_bytes([wh[28], wh[29], wh[30], wh[31]]);
    let (mut s0, mut s1) = wal_checksum(&wh[0..24], 0, 0, big_endian_words);
    if (s0, s1) != (hdr_c0, hdr_c1) {
        return (None, 0, HashMap::new());
    }

    let salt1 = &wh[16..20];
    let salt2 = &wh[20..24];
    let frame_size = WAL_FRAME_HEADER_BYTES as u64 + page_size as u64;

    let mut committed = HashMap::new();
    let mut pending: Vec<(u32, u64)> = Vec::new();
    let mut page_buf = vec![0u8; page_size];
    let mut fh = [0u8; WAL_FRAME_HEADER_BYTES];
    let mut offset = WAL_HEADER_BYTES as u64;

    while offset.saturating_add(frame_size) <= wal_len {
        if wal_file.read_exact(&mut fh).is_err() {
            break;
        }
        let pgno = u32::from_be_bytes([fh[0], fh[1], fh[2], fh[3]]);
        let db_size = u32::from_be_bytes([fh[4], fh[5], fh[6], fh[7]]);
        if pgno == 0 || &fh[8..12] != salt1 || &fh[12..16] != salt2 {
            break;
        }
        if wal_file.read_exact(&mut page_buf).is_err() {
            break;
        }
        (s0, s1) = wal_checksum(&fh[0..8], s0, s1, big_endian_words);
        (s0, s1) = wal_checksum(&page_buf, s0, s1, big_endian_words);
        let fc0 = u32::from_be_bytes([fh[16], fh[17], fh[18], fh[19]]);
        let fc1 = u32::from_be_bytes([fh[20], fh[21], fh[22], fh[23]]);
        if (s0, s1) != (fc0, fc1) {
            break;
        }

        pending.push((pgno, offset + WAL_FRAME_HEADER_BYTES as u64));
        if db_size > 0 {
            for (p, off) in pending.drain(..) {
                committed.insert(p, off);
            }
        }
        offset += frame_size;
    }

    if committed.is_empty() {
        (None, 0, HashMap::new())
    } else {
        (Some(wal_file), wal_len, committed)
    }
}

fn read_sqlite_varint(buf: &[u8], pos: usize) -> Option<(u64, usize)> {
    let mut val: u64 = 0;
    for i in 0..8 {
        let b = *buf.get(pos + i)?;
        val = (val << 7) | u64::from(b & 0x7f);
        if (b & 0x80) == 0 {
            return Some((val, pos + i + 1));
        }
    }
    let b = *buf.get(pos + 8)?;
    val = (val << 8) | u64::from(b);
    Some((val, pos + 9))
}

fn read_u16_be(buf: &[u8], off: usize) -> Option<u16> {
    let slice = buf.get(off..off + 2)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_u32_be(buf: &[u8], off: usize) -> Option<u32> {
    let slice = buf.get(off..off + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn compute_inline_len(payload_len: u64, usable_size: usize) -> usize {
    let u = usable_size as u64;
    let x = u - 35;
    if payload_len <= x {
        return payload_len as usize;
    }
    let m = ((u - 12) * 32 / 255) - 23;
    let k = m + ((payload_len - m) % (u - 4));
    let local = if k <= x { k } else { m };
    local as usize
}

struct PayloadStream<'a> {
    source: &'a mut SqlitePageSource,
    usable_size: usize,
    chunk_buf: Vec<u8>,
    chunk_pos: usize,
    chunk_end: usize,
    remaining_total: u64,
    next_ov_pgno: u32,
    hops: usize,
}

impl<'a> PayloadStream<'a> {
    fn from_cell(
        source: &'a mut SqlitePageSource,
        leaf_page: &[u8],
        payload_offset: usize,
        payload_len: u64,
    ) -> Result<Self, ()> {
        let usable_size = source.usable_size;
        let inline_len = compute_inline_len(payload_len, usable_size);
        let inline_end = payload_offset.checked_add(inline_len).ok_or(())?;
        if inline_end > usable_size || inline_end > leaf_page.len() {
            return Err(());
        }
        let next_ov_pgno = if payload_len > inline_len as u64 {
            let ov_end = inline_end.checked_add(4).ok_or(())?;
            if ov_end > usable_size || ov_end > leaf_page.len() {
                return Err(());
            }
            read_u32_be(leaf_page, inline_end).ok_or(())?
        } else {
            0
        };
        let chunk_buf = leaf_page[payload_offset..inline_end].to_vec();
        Ok(Self {
            source,
            usable_size,
            chunk_buf,
            chunk_pos: 0,
            chunk_end: inline_len,
            remaining_total: payload_len,
            next_ov_pgno,
            hops: 0,
        })
    }

    fn remaining(&self) -> u64 {
        self.remaining_total
    }

    fn advance_overflow_page(&mut self) -> Result<(), ()> {
        if self.remaining_total == 0 || self.next_ov_pgno == 0 || self.hops >= MAX_OVERFLOW_HOPS {
            return Err(());
        }
        self.hops += 1;
        let max_page_data = (self.usable_size.saturating_sub(4)) as u64;
        let take = self.remaining_total.min(max_page_data) as usize;
        if take == 0 {
            return Err(());
        }
        let next_pg =
            self.source
                .read_overflow_chunk(self.next_ov_pgno, take, &mut self.chunk_buf)?;
        self.next_ov_pgno = next_pg;
        self.chunk_pos = 0;
        self.chunk_end = take;
        Ok(())
    }

    fn read_byte(&mut self) -> Result<u8, ()> {
        if self.remaining_total == 0 {
            return Err(());
        }
        if self.chunk_pos >= self.chunk_end {
            self.advance_overflow_page()?;
        }
        let b = self.chunk_buf[self.chunk_pos];
        self.chunk_pos += 1;
        self.remaining_total -= 1;
        Ok(b)
    }

    fn read_exact_into(&mut self, mut n: usize, out: &mut Vec<u8>) -> Result<(), ()> {
        if (n as u64) > self.remaining_total {
            return Err(());
        }
        out.clear();
        out.reserve(n);
        while n > 0 {
            if self.chunk_pos >= self.chunk_end {
                self.advance_overflow_page()?;
            }
            let avail = self.chunk_end - self.chunk_pos;
            let take = avail.min(n);
            out.extend_from_slice(&self.chunk_buf[self.chunk_pos..self.chunk_pos + take]);
            self.chunk_pos += take;
            self.remaining_total -= take as u64;
            n -= take;
        }
        Ok(())
    }

    fn skip(&mut self, mut n: u64) -> Result<(), ()> {
        if n > self.remaining_total {
            return Err(());
        }
        if n == 0 {
            return Ok(());
        }
        let in_buf = ((self.chunk_end - self.chunk_pos) as u64).min(n);
        self.chunk_pos += in_buf as usize;
        self.remaining_total -= in_buf;
        n -= in_buf;

        let max_page_data = (self.usable_size.saturating_sub(4)) as u64;
        while n > 0 {
            if self.next_ov_pgno == 0 || self.hops >= MAX_OVERFLOW_HOPS {
                return Err(());
            }
            self.hops += 1;
            let page_cap = self.remaining_total.min(max_page_data);
            if page_cap == 0 {
                return Err(());
            }
            if n >= page_cap {
                let next_pg = self
                    .source
                    .read_overflow_next_pgno(self.next_ov_pgno, page_cap)?;
                self.next_ov_pgno = next_pg;
                self.remaining_total -= page_cap;
                n -= page_cap;
                self.chunk_pos = 0;
                self.chunk_end = 0;
            } else {
                let take = page_cap as usize;
                let next_pg = self.source.read_overflow_chunk(
                    self.next_ov_pgno,
                    take,
                    &mut self.chunk_buf,
                )?;
                self.next_ov_pgno = next_pg;
                self.chunk_pos = n as usize;
                self.chunk_end = take;
                self.remaining_total -= n;
                n = 0;
            }
        }
        Ok(())
    }

    fn read_sqlite_varint(&mut self) -> Result<(u64, u64), ()> {
        let mut val: u64 = 0;
        for i in 0..8 {
            let b = self.read_byte()?;
            val = (val << 7) | u64::from(b & 0x7f);
            if (b & 0x80) == 0 {
                return Ok((val, (i + 1) as u64));
            }
        }
        let b = self.read_byte()?;
        val = (val << 8) | u64::from(b);
        Ok((val, 9))
    }

    fn read_pb_varint(&mut self) -> Result<u64, ()> {
        let mut val: u64 = 0;
        let mut shift = 0u32;
        for _ in 0..10 {
            let b = self.read_byte()?;
            let low = u64::from(b & 0x7f);
            val |= low << shift;
            if (b & 0x80) == 0 {
                return Ok(val);
            }
            shift += 7;
        }
        Err(())
    }
}

fn serial_type_byte_len(st: u64) -> Option<u64> {
    match st {
        0 | 8 | 9 => Some(0),
        1 => Some(1),
        2 => Some(2),
        3 => Some(3),
        4 => Some(4),
        5 => Some(6),
        6 | 7 => Some(8),
        10 | 11 => None,
        n => Some((n - 12) / 2),
    }
}

fn read_record_serial_types(stream: &mut PayloadStream<'_>) -> Result<Vec<u64>, ()> {
    let (hdr_len, mut consumed) = stream.read_sqlite_varint()?;
    if hdr_len == 0 || hdr_len > stream.remaining().saturating_add(consumed) {
        return Err(());
    }
    let mut types = Vec::with_capacity(8);
    while consumed < hdr_len {
        let (st, used) = stream.read_sqlite_varint()?;
        consumed = consumed.saturating_add(used);
        if types.len() >= 64 {
            return Err(());
        }
        types.push(st);
    }
    if consumed != hdr_len {
        return Err(());
    }
    Ok(types)
}

fn read_sqlite_int(stream: &mut PayloadStream<'_>, st: u64, scratch: &mut Vec<u8>) -> Option<i64> {
    match st {
        8 => Some(0),
        9 => Some(1),
        1 => {
            stream.read_exact_into(1, scratch).ok()?;
            Some(scratch[0] as i8 as i64)
        }
        2 => {
            stream.read_exact_into(2, scratch).ok()?;
            Some(i16::from_be_bytes([scratch[0], scratch[1]]) as i64)
        }
        3 => {
            stream.read_exact_into(3, scratch).ok()?;
            let sign = if scratch[0] & 0x80 != 0 { 0xff } else { 0 };
            Some(i64::from_be_bytes([
                sign, sign, sign, sign, sign, scratch[0], scratch[1], scratch[2],
            ]))
        }
        4 => {
            stream.read_exact_into(4, scratch).ok()?;
            Some(i32::from_be_bytes([scratch[0], scratch[1], scratch[2], scratch[3]]) as i64)
        }
        5 => {
            stream.read_exact_into(6, scratch).ok()?;
            let sign = if scratch[0] & 0x80 != 0 { 0xff } else { 0 };
            Some(i64::from_be_bytes([
                sign, sign, scratch[0], scratch[1], scratch[2], scratch[3], scratch[4], scratch[5],
            ]))
        }
        6 => {
            stream.read_exact_into(8, scratch).ok()?;
            Some(i64::from_be_bytes([
                scratch[0], scratch[1], scratch[2], scratch[3], scratch[4], scratch[5], scratch[6],
                scratch[7],
            ]))
        }
        _ => None,
    }
}

fn read_sqlite_text(
    stream: &mut PayloadStream<'_>,
    st: u64,
    scratch: &mut Vec<u8>,
) -> Option<String> {
    if st < 13 || st.is_multiple_of(2) {
        return None;
    }
    let len = (st - 13) / 2;
    if len > MAX_PB_STR_BYTES {
        return None;
    }
    stream.read_exact_into(len as usize, scratch).ok()?;
    Some(String::from_utf8_lossy(scratch).into_owned())
}

struct TableRoots {
    gen_metadata: Option<u32>,
    executor_metadata: Option<u32>,
}

fn walk_table_btree_rtl<F>(
    source: &mut SqlitePageSource,
    pgno: u32,
    depth: usize,
    visited: &mut HashSet<u32>,
    on_cell: &mut F,
) -> Result<bool, ()>
where
    F: FnMut(&mut SqlitePageSource, u64, &[u8], usize) -> Result<bool, ()>,
{
    if depth >= MAX_BTREE_DEPTH || pgno == 0 || !visited.insert(pgno) {
        return Err(());
    }
    let mut page = Vec::new();
    source.read_page(pgno, &mut page)?;

    let hdr_off = if pgno == 1 { SQLITE_HEADER_BYTES } else { 0 };
    let usable = source.usable_size;
    if hdr_off + 8 > usable || usable > page.len() {
        return Err(());
    }
    let ptype = page[hdr_off];
    let num_cells = read_u16_be(&page, hdr_off + 3).ok_or(())? as usize;

    match ptype {
        0x05 => {
            if hdr_off + 12 > usable {
                return Err(());
            }
            let right_child = read_u32_be(&page, hdr_off + 8).ok_or(())?;
            let ptrs_start = hdr_off + 12;
            let ptrs_end = ptrs_start.checked_add(num_cells * 2).ok_or(())?;
            if ptrs_end > usable {
                return Err(());
            }
            if walk_table_btree_rtl(source, right_child, depth + 1, visited, on_cell)? {
                return Ok(true);
            }
            for i in (0..num_cells).rev() {
                let cell_off = read_u16_be(&page, ptrs_start + i * 2).ok_or(())? as usize;
                if cell_off + 4 > usable {
                    return Err(());
                }
                let left_child = read_u32_be(&page, cell_off).ok_or(())?;
                if walk_table_btree_rtl(source, left_child, depth + 1, visited, on_cell)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        0x0d => {
            let ptrs_start = hdr_off + 8;
            let ptrs_end = ptrs_start.checked_add(num_cells * 2).ok_or(())?;
            if ptrs_end > usable {
                return Err(());
            }
            for i in (0..num_cells).rev() {
                let cell_off = read_u16_be(&page, ptrs_start + i * 2).ok_or(())? as usize;
                if cell_off >= usable {
                    return Err(());
                }
                let (payload_len, pos) = read_sqlite_varint(&page, cell_off).ok_or(())?;
                let (_rowid, payload_off) = read_sqlite_varint(&page, pos).ok_or(())?;
                if payload_off > usable {
                    return Err(());
                }
                if on_cell(source, payload_len, &page, payload_off)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Err(()),
    }
}

fn read_schema_rootpages(source: &mut SqlitePageSource) -> Option<TableRoots> {
    let mut roots = TableRoots {
        gen_metadata: None,
        executor_metadata: None,
    };
    let mut visited = HashSet::new();
    let mut scratch = Vec::new();
    let res = walk_table_btree_rtl(source, 1, 0, &mut visited, &mut |src, plen, page, off| {
        let mut stream = PayloadStream::from_cell(src, page, off, plen)?;
        let Ok(stypes) = read_record_serial_types(&mut stream) else {
            return Ok(false);
        };
        if stypes.len() < 4 {
            return Ok(false);
        }
        let Some(obj_type) = read_sqlite_text(&mut stream, stypes[0], &mut scratch) else {
            return Ok(false);
        };
        if obj_type != "table" {
            return Ok(false);
        }
        let Some(tbl_name) = read_sqlite_text(&mut stream, stypes[1], &mut scratch) else {
            return Ok(false);
        };
        if tbl_name != "gen_metadata" && tbl_name != "executor_metadata" {
            return Ok(false);
        }
        let Some(skip_col2) = serial_type_byte_len(stypes[2]) else {
            return Ok(false);
        };
        if stream.skip(skip_col2).is_err() {
            return Ok(false);
        }
        if let Some(root_i64) = read_sqlite_int(&mut stream, stypes[3], &mut scratch)
            && let Ok(root_u32) = u32::try_from(root_i64)
            && root_u32 > 0
        {
            if tbl_name == "gen_metadata" {
                roots.gen_metadata = Some(root_u32);
            } else {
                roots.executor_metadata = Some(root_u32);
            }
        }
        Ok(roots.gen_metadata.is_some() && roots.executor_metadata.is_some())
    });
    res.ok().map(|_| roots)
}

fn skip_pb_field(stream: &mut PayloadStream<'_>, wt: u64, stop_at: u64) -> Result<(), ()> {
    match wt {
        0 => {
            stream.read_pb_varint()?;
            Ok(())
        }
        1 => {
            if stream.remaining().saturating_sub(stop_at) < 8 {
                return Err(());
            }
            stream.skip(8)
        }
        2 => {
            let flen = stream.read_pb_varint()?;
            if flen > stream.remaining().saturating_sub(stop_at) {
                return Err(());
            }
            stream.skip(flen)
        }
        5 => {
            if stream.remaining().saturating_sub(stop_at) < 4 {
                return Err(());
            }
            stream.skip(4)
        }
        _ => Err(()),
    }
}

fn read_pb_string(
    stream: &mut PayloadStream<'_>,
    len: u64,
    scratch: &mut Vec<u8>,
) -> Result<Option<String>, ()> {
    if len > MAX_PB_STR_BYTES {
        stream.skip(len)?;
        return Ok(None);
    }
    stream.read_exact_into(len as usize, scratch)?;
    let s = String::from_utf8_lossy(scratch);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

#[derive(Default)]
struct GenMetadataRow {
    ceiling_tokens: Option<u64>,
    model_id: Option<String>,
    display_name: Option<String>,
    model_enum: Option<String>,
    subagent_type: Option<String>,
}

fn parse_gen_f10(stream: &mut PayloadStream<'_>, len: u64) -> Result<Option<u64>, ()> {
    let stop_at = stream.remaining().checked_sub(len).ok_or(())?;
    let mut ceiling = None;
    while stream.remaining() > stop_at {
        let tag = stream.read_pb_varint()?;
        let fn_num = tag >> 3;
        let wt = tag & 7;
        if fn_num == 4 && wt == 0 {
            let val = stream.read_pb_varint()?;
            if val > 0 {
                ceiling = Some(val);
            }
        } else {
            skip_pb_field(stream, wt, stop_at)?;
        }
    }
    Ok(ceiling)
}

fn parse_gen_f9(stream: &mut PayloadStream<'_>, len: u64) -> Result<Option<u64>, ()> {
    let stop_at = stream.remaining().checked_sub(len).ok_or(())?;
    let mut ceiling = None;
    while stream.remaining() > stop_at {
        let tag = stream.read_pb_varint()?;
        let fn_num = tag >> 3;
        let wt = tag & 7;
        if fn_num == 10 && wt == 2 {
            let flen = stream.read_pb_varint()?;
            if flen > stream.remaining().saturating_sub(stop_at) {
                return Err(());
            }
            if let Some(c) = parse_gen_f10(stream, flen)? {
                ceiling = Some(c);
            }
        } else {
            skip_pb_field(stream, wt, stop_at)?;
        }
    }
    Ok(ceiling)
}

fn parse_gen_f20_entry(
    stream: &mut PayloadStream<'_>,
    len: u64,
    model_enum: &mut Option<String>,
    subagent_type: &mut Option<String>,
    scratch: &mut Vec<u8>,
) -> Result<(), ()> {
    let stop_at = stream.remaining().checked_sub(len).ok_or(())?;
    let mut key: Option<String> = None;
    let mut val: Option<String> = None;
    while stream.remaining() > stop_at {
        let tag = stream.read_pb_varint()?;
        let fn_num = tag >> 3;
        let wt = tag & 7;
        if (fn_num == 1 || fn_num == 2) && wt == 2 {
            let flen = stream.read_pb_varint()?;
            if flen > stream.remaining().saturating_sub(stop_at) {
                return Err(());
            }
            if fn_num == 1 {
                key = read_pb_string(stream, flen, scratch)?;
            } else if key
                .as_deref()
                .is_none_or(|k| k == "model_enum" || k == "subagent_type")
            {
                val = read_pb_string(stream, flen, scratch)?;
            } else {
                stream.skip(flen)?;
            }
        } else {
            skip_pb_field(stream, wt, stop_at)?;
        }
    }
    match key.as_deref() {
        Some("model_enum") => *model_enum = val,
        Some("subagent_type") => *subagent_type = val,
        _ => {}
    }
    Ok(())
}

fn parse_gen_chat_model(stream: &mut PayloadStream<'_>, len: u64) -> Result<GenMetadataRow, ()> {
    let stop_at = stream.remaining().checked_sub(len).ok_or(())?;
    let mut row = GenMetadataRow::default();
    let mut scratch = Vec::new();
    while stream.remaining() > stop_at {
        let tag = stream.read_pb_varint()?;
        let fn_num = tag >> 3;
        let wt = tag & 7;
        if wt == 2 {
            let flen = stream.read_pb_varint()?;
            if flen > stream.remaining().saturating_sub(stop_at) {
                return Err(());
            }
            match fn_num {
                9 => {
                    if let Some(c) = parse_gen_f9(stream, flen)? {
                        row.ceiling_tokens = Some(c);
                    }
                }
                19 => row.model_id = read_pb_string(stream, flen, &mut scratch)?,
                20 => parse_gen_f20_entry(
                    stream,
                    flen,
                    &mut row.model_enum,
                    &mut row.subagent_type,
                    &mut scratch,
                )?,
                21 => row.display_name = read_pb_string(stream, flen, &mut scratch)?,
                _ => stream.skip(flen)?,
            }
        } else {
            skip_pb_field(stream, wt, stop_at)?;
        }
    }
    Ok(row)
}

fn decode_gen_metadata_blob(
    stream: &mut PayloadStream<'_>,
    blob_len: u64,
) -> Result<Option<GenMetadataRow>, ()> {
    let stop_at = stream.remaining().checked_sub(blob_len).ok_or(())?;
    let mut found = None;
    while stream.remaining() > stop_at {
        let tag = stream.read_pb_varint()?;
        let fn_num = tag >> 3;
        let wt = tag & 7;
        if fn_num == 1 && wt == 2 {
            let flen = stream.read_pb_varint()?;
            if flen > stream.remaining().saturating_sub(stop_at) {
                return Err(());
            }
            found = Some(parse_gen_chat_model(stream, flen)?);
        } else {
            skip_pb_field(stream, wt, stop_at)?;
        }
    }
    Ok(found)
}

fn scan_gen_metadata(
    source: &mut SqlitePageSource,
    root_pgno: u32,
    active_model_hint: Option<&str>,
    is_shadow: fn(&str) -> bool,
) -> Result<Option<DbCeilingObservation>, ()> {
    let mut visited = HashSet::new();
    let mut rows_seen = 0usize;
    let mut primary_obs: Option<DbCeilingObservation> = None;
    let mut subagent_hint_obs: Option<DbCeilingObservation> = None;

    walk_table_btree_rtl(
        source,
        root_pgno,
        0,
        &mut visited,
        &mut |src, plen, page, off| {
            if rows_seen >= MAX_GEN_SCAN_ROWS {
                return Ok(true);
            }
            rows_seen += 1;

            let mut stream = PayloadStream::from_cell(src, page, off, plen)?;
            let Ok(stypes) = read_record_serial_types(&mut stream) else {
                return Ok(false);
            };
            if stypes.len() < 2 {
                return Ok(false);
            }
            let Some(col0_len) = serial_type_byte_len(stypes[0]) else {
                return Ok(false);
            };
            if stream.skip(col0_len).is_err() {
                return Ok(false);
            }
            let col1_st = stypes[1];
            if col1_st < 12 || col1_st % 2 != 0 {
                return Ok(false);
            }
            let blob_len = (col1_st - 12) / 2;
            if blob_len == 0 || blob_len > stream.remaining() {
                return Ok(false);
            }
            let Ok(Some(row)) = decode_gen_metadata_blob(&mut stream, blob_len) else {
                return Ok(false);
            };
            let Some(ceiling_tokens) = row.ceiling_tokens.filter(|&c| c > 0) else {
                return Ok(false);
            };

            let obs = DbCeilingObservation {
                ceiling_tokens,
                origin: DbCeilingOrigin::GenMetadata,
                model_id: row.model_id,
                display_name: row.display_name,
                model_enum: row.model_enum,
                subagent_type: row.subagent_type,
            };

            let subagent_is_shadow = obs.subagent_type.as_deref().is_some_and(is_shadow);
            if subagent_is_shadow {
                let hint_matches_sub = active_model_hint
                    .zip(obs.subagent_type.as_deref())
                    .is_some_and(|(h, s)| models_match(h, s));
                if !hint_matches_sub {
                    return Ok(false);
                }
            }

            let hint_matches_model = active_model_hint.is_some_and(|hint| {
                [
                    obs.model_id.as_deref(),
                    obs.display_name.as_deref(),
                    obs.model_enum.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|cand| models_match(hint, cand))
            });

            let model_is_shadow = [
                obs.model_id.as_deref(),
                obs.display_name.as_deref(),
                obs.model_enum.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(is_shadow);

            if model_is_shadow && !hint_matches_model {
                return Ok(false);
            }

            let has_subagent = obs
                .subagent_type
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty());

            if !has_subagent {
                primary_obs = Some(obs);
                return Ok(true);
            }

            if hint_matches_model && subagent_hint_obs.is_none() {
                subagent_hint_obs = Some(obs);
            }
            Ok(false)
        },
    )?;

    Ok(primary_obs.or(subagent_hint_obs))
}

fn parse_exec_f2(stream: &mut PayloadStream<'_>, len: u64) -> Result<Option<u64>, ()> {
    let stop_at = stream.remaining().checked_sub(len).ok_or(())?;
    let mut max_limit = None;
    while stream.remaining() > stop_at {
        let tag = stream.read_pb_varint()?;
        let fn_num = tag >> 3;
        let wt = tag & 7;
        if fn_num == 5 && wt == 0 {
            let val = stream.read_pb_varint()?;
            if val > 0 {
                max_limit = Some(val);
            }
        } else {
            skip_pb_field(stream, wt, stop_at)?;
        }
    }
    Ok(max_limit)
}

fn parse_exec_f10(stream: &mut PayloadStream<'_>, len: u64) -> Result<Option<u64>, ()> {
    let stop_at = stream.remaining().checked_sub(len).ok_or(())?;
    let mut max_limit = None;
    while stream.remaining() > stop_at {
        let tag = stream.read_pb_varint()?;
        let fn_num = tag >> 3;
        let wt = tag & 7;
        if fn_num == 2 && wt == 2 {
            let flen = stream.read_pb_varint()?;
            if flen > stream.remaining().saturating_sub(stop_at) {
                return Err(());
            }
            if let Some(val) = parse_exec_f2(stream, flen)? {
                max_limit = Some(val);
            }
        } else {
            skip_pb_field(stream, wt, stop_at)?;
        }
    }
    Ok(max_limit)
}

fn decode_executor_metadata_blob(
    stream: &mut PayloadStream<'_>,
    blob_len: u64,
) -> Result<Option<u64>, ()> {
    let stop_at = stream.remaining().checked_sub(blob_len).ok_or(())?;
    let mut max_limit = None;
    while stream.remaining() > stop_at {
        let tag = stream.read_pb_varint()?;
        let fn_num = tag >> 3;
        let wt = tag & 7;
        if fn_num == 10 && wt == 2 {
            let flen = stream.read_pb_varint()?;
            if flen > stream.remaining().saturating_sub(stop_at) {
                return Err(());
            }
            if let Some(val) = parse_exec_f10(stream, flen)? {
                max_limit = Some(val);
            }
        } else {
            skip_pb_field(stream, wt, stop_at)?;
        }
    }
    Ok(max_limit)
}

fn scan_executor_metadata(
    source: &mut SqlitePageSource,
    root_pgno: u32,
) -> Result<Option<DbCeilingObservation>, ()> {
    let mut visited = HashSet::new();
    let mut rows_seen = 0usize;
    let mut found = None;

    walk_table_btree_rtl(
        source,
        root_pgno,
        0,
        &mut visited,
        &mut |src, plen, page, off| {
            if rows_seen >= MAX_EXEC_SCAN_ROWS {
                return Ok(true);
            }
            rows_seen += 1;

            let mut stream = PayloadStream::from_cell(src, page, off, plen)?;
            let Ok(stypes) = read_record_serial_types(&mut stream) else {
                return Ok(false);
            };
            if stypes.len() < 2 {
                return Ok(false);
            }
            let Some(col0_len) = serial_type_byte_len(stypes[0]) else {
                return Ok(false);
            };
            if stream.skip(col0_len).is_err() {
                return Ok(false);
            }
            let col1_st = stypes[1];
            if col1_st < 12 || col1_st % 2 != 0 {
                return Ok(false);
            }
            let blob_len = (col1_st - 12) / 2;
            if blob_len == 0 || blob_len > stream.remaining() {
                return Ok(false);
            }
            if let Ok(Some(ceiling_tokens)) = decode_executor_metadata_blob(&mut stream, blob_len)
                && ceiling_tokens > 0
            {
                found = Some(DbCeilingObservation {
                    ceiling_tokens,
                    origin: DbCeilingOrigin::ExecutorMetadata,
                    model_id: None,
                    display_name: None,
                    model_enum: None,
                    subagent_type: None,
                });
                return Ok(true);
            }
            Ok(false)
        },
    )?;

    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Instant;

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new(label: &str) -> Self {
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!("agy-hud-sessdb-{label}-{pid}-{nanos}"));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn test_is_shadow(s: &str) -> bool {
        let lower = s.to_ascii_lowercase();
        if lower.contains("flash-lite")
            || lower.contains("flash_lite")
            || lower.contains("banana")
            || lower.contains("image")
        {
            return true;
        }
        let bytes = lower.as_bytes();
        let mut idx = 0;
        while let Some(pos) = lower[idx..].find("m50") {
            let start = idx + pos;
            let after = start + 3;
            let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
            let after_ok = after >= bytes.len() || !bytes[after].is_ascii_digit();
            if before_ok && after_ok {
                return true;
            }
            idx = start + 3;
        }
        false
    }

    fn push_pb_varint(out: &mut Vec<u8>, mut val: u64) {
        loop {
            let byte = (val & 0x7f) as u8;
            val >>= 7;
            if val == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
    }

    fn push_pb_bytes_field(out: &mut Vec<u8>, field_num: u64, data: &[u8]) {
        push_pb_varint(out, (field_num << 3) | 2);
        push_pb_varint(out, data.len() as u64);
        out.extend_from_slice(data);
    }

    fn push_pb_varint_field(out: &mut Vec<u8>, field_num: u64, val: u64) {
        push_pb_varint(out, field_num << 3);
        push_pb_varint(out, val);
    }

    fn build_gen_blob(
        ceiling: Option<u64>,
        model_id: Option<&str>,
        display_name: Option<&str>,
        model_enum: Option<&str>,
        subagent_type: Option<&str>,
        giant_prefix_bytes: usize,
    ) -> Vec<u8> {
        let mut chat = Vec::new();
        if giant_prefix_bytes > 0 {
            let filler = vec![b'A'; giant_prefix_bytes];
            push_pb_bytes_field(&mut chat, 2, &filler);
        }
        if let Some(c) = ceiling {
            let mut f10 = Vec::new();
            push_pb_varint_field(&mut f10, 4, c);
            let mut f9 = Vec::new();
            push_pb_bytes_field(&mut f9, 10, &f10);
            push_pb_bytes_field(&mut chat, 9, &f9);
        }
        if let Some(mid) = model_id {
            push_pb_bytes_field(&mut chat, 19, mid.as_bytes());
        }
        if let Some(me) = model_enum {
            let mut entry = Vec::new();
            push_pb_bytes_field(&mut entry, 1, b"model_enum");
            push_pb_bytes_field(&mut entry, 2, me.as_bytes());
            push_pb_bytes_field(&mut chat, 20, &entry);
        }
        if let Some(sub) = subagent_type {
            let mut entry = Vec::new();
            push_pb_bytes_field(&mut entry, 1, b"subagent_type");
            push_pb_bytes_field(&mut entry, 2, sub.as_bytes());
            push_pb_bytes_field(&mut chat, 20, &entry);
        }
        if let Some(disp) = display_name {
            push_pb_bytes_field(&mut chat, 21, disp.as_bytes());
        }
        let mut top = Vec::new();
        push_pb_bytes_field(&mut top, 1, &chat);
        top
    }

    fn build_exec_blob(max_token_limit: u64) -> Vec<u8> {
        let mut f2 = Vec::new();
        push_pb_varint_field(&mut f2, 5, max_token_limit);
        let mut f10 = Vec::new();
        push_pb_bytes_field(&mut f10, 2, &f2);
        let mut top = Vec::new();
        push_pb_bytes_field(&mut top, 10, &f10);
        top
    }

    fn push_sqlite_varint(out: &mut Vec<u8>, v: u64) {
        if v <= 0x7f {
            out.push(v as u8);
            return;
        }
        let mut buf = [0u8; 9];
        let mut len = 0usize;
        let mut x = v;
        while x > 0 {
            buf[8 - len] = (x & 0x7f) as u8;
            x >>= 7;
            len += 1;
        }
        for slot in &mut buf[(9 - len)..8] {
            *slot |= 0x80;
        }
        out.extend_from_slice(&buf[9 - len..9]);
    }

    enum SqlCol<'a> {
        Null,
        Int(i64),
        Text(&'a str),
        Blob(&'a [u8]),
    }

    fn build_sqlite_record(cols: &[SqlCol<'_>]) -> Vec<u8> {
        let mut types_bytes = Vec::new();
        let mut body = Vec::new();
        for c in cols {
            match c {
                SqlCol::Null => push_sqlite_varint(&mut types_bytes, 0),
                SqlCol::Int(v) => {
                    if *v == 0 {
                        push_sqlite_varint(&mut types_bytes, 8);
                    } else if *v == 1 {
                        push_sqlite_varint(&mut types_bytes, 9);
                    } else if let Ok(v8) = i8::try_from(*v) {
                        push_sqlite_varint(&mut types_bytes, 1);
                        body.push(v8 as u8);
                    } else if let Ok(v16) = i16::try_from(*v) {
                        push_sqlite_varint(&mut types_bytes, 2);
                        body.extend_from_slice(&v16.to_be_bytes());
                    } else {
                        push_sqlite_varint(&mut types_bytes, 4);
                        body.extend_from_slice(&(*v as i32).to_be_bytes());
                    }
                }
                SqlCol::Text(s) => {
                    let st = 13 + (s.len() as u64) * 2;
                    push_sqlite_varint(&mut types_bytes, st);
                    body.extend_from_slice(s.as_bytes());
                }
                SqlCol::Blob(b) => {
                    let st = 12 + (b.len() as u64) * 2;
                    push_sqlite_varint(&mut types_bytes, st);
                    body.extend_from_slice(b);
                }
            }
        }
        // Compute exact varint header size (handles 1- or 2-byte header lengths cleanly)
        let mut hdr_len = 1 + types_bytes.len();
        if hdr_len >= 128 {
            hdr_len += 1;
        }
        let mut rec = Vec::with_capacity(hdr_len + body.len());
        push_sqlite_varint(&mut rec, hdr_len as u64);
        rec.extend_from_slice(&types_bytes);
        rec.extend_from_slice(&body);
        rec
    }

    struct DbBuilder {
        page_size: usize,
        reserved: usize,
        pages: Vec<Vec<u8>>,
    }

    impl DbBuilder {
        fn new(page_size: usize, reserved: usize) -> Self {
            let mut p1 = vec![0u8; page_size];
            p1[0..16].copy_from_slice(SQLITE_HEADER_MAGIC);
            let ps_hdr: u16 = if page_size == 65_536 {
                1
            } else {
                page_size as u16
            };
            p1[16..18].copy_from_slice(&ps_hdr.to_be_bytes());
            p1[18] = 2;
            p1[19] = 2;
            p1[20] = reserved as u8;
            Self {
                page_size,
                reserved,
                pages: vec![p1],
            }
        }

        fn usable(&self) -> usize {
            self.page_size - self.reserved
        }

        fn alloc_page(&mut self) -> u32 {
            self.pages.push(vec![0u8; self.page_size]);
            self.pages.len() as u32
        }

        fn build_leaf_page(&mut self, pgno: u32, rows: &[(u64, Vec<u8>)]) {
            let usable = self.usable();
            let hdr_off = if pgno == 1 { SQLITE_HEADER_BYTES } else { 0 };
            let mut page = self.pages[pgno as usize - 1].clone();
            let mut content_end = usable;
            let mut cell_ptrs = Vec::with_capacity(rows.len());

            for (rowid, payload) in rows {
                let plen = payload.len() as u64;
                let inline = compute_inline_len(plen, usable);
                let mut next_ov_pg = 0u32;

                if plen > inline as u64 {
                    let ov_cap = usable - 4;
                    let mut rem = &payload[inline..];
                    let mut ov_pages = Vec::new();
                    while !rem.is_empty() {
                        let take = rem.len().min(ov_cap);
                        let ov_pg = self.alloc_page();
                        ov_pages.push((ov_pg, &rem[..take]));
                        rem = &rem[take..];
                    }
                    for idx in (0..ov_pages.len()).rev() {
                        let (ov_pg, slice) = ov_pages[idx];
                        let next = if idx + 1 < ov_pages.len() {
                            ov_pages[idx + 1].0
                        } else {
                            0
                        };
                        let buf = &mut self.pages[ov_pg as usize - 1];
                        buf[0..4].copy_from_slice(&next.to_be_bytes());
                        buf[4..4 + slice.len()].copy_from_slice(slice);
                    }
                    next_ov_pg = ov_pages[0].0;
                }

                let mut cell = Vec::new();
                push_sqlite_varint(&mut cell, plen);
                push_sqlite_varint(&mut cell, *rowid);
                cell.extend_from_slice(&payload[..inline]);
                if plen > inline as u64 {
                    cell.extend_from_slice(&next_ov_pg.to_be_bytes());
                }

                content_end -= cell.len();
                page[content_end..content_end + cell.len()].copy_from_slice(&cell);
                cell_ptrs.push(content_end as u16);
            }

            page[hdr_off] = 0x0d;
            page[hdr_off + 3..hdr_off + 5].copy_from_slice(&(rows.len() as u16).to_be_bytes());
            page[hdr_off + 5..hdr_off + 7].copy_from_slice(&(content_end as u16).to_be_bytes());
            for (i, ptr) in cell_ptrs.iter().enumerate() {
                let off = hdr_off + 8 + i * 2;
                page[off..off + 2].copy_from_slice(&ptr.to_be_bytes());
            }
            self.pages[pgno as usize - 1] = page;
        }

        fn build_interior_page(&mut self, pgno: u32, children: &[(u32, u64)], right_child: u32) {
            let usable = self.usable();
            let hdr_off = if pgno == 1 { SQLITE_HEADER_BYTES } else { 0 };
            let mut page = self.pages[pgno as usize - 1].clone();
            let mut content_end = usable;
            let mut cell_ptrs = Vec::with_capacity(children.len());

            for &(left_child, key) in children {
                let mut cell = Vec::new();
                cell.extend_from_slice(&left_child.to_be_bytes());
                push_sqlite_varint(&mut cell, key);
                content_end -= cell.len();
                page[content_end..content_end + cell.len()].copy_from_slice(&cell);
                cell_ptrs.push(content_end as u16);
            }

            page[hdr_off] = 0x05;
            page[hdr_off + 3..hdr_off + 5].copy_from_slice(&(children.len() as u16).to_be_bytes());
            page[hdr_off + 5..hdr_off + 7].copy_from_slice(&(content_end as u16).to_be_bytes());
            page[hdr_off + 8..hdr_off + 12].copy_from_slice(&right_child.to_be_bytes());
            for (i, ptr) in cell_ptrs.iter().enumerate() {
                let off = hdr_off + 12 + i * 2;
                page[off..off + 2].copy_from_slice(&ptr.to_be_bytes());
            }
            self.pages[pgno as usize - 1] = page;
        }

        fn to_bytes(&self) -> Vec<u8> {
            let mut out = Vec::with_capacity(self.pages.len() * self.page_size);
            for p in &self.pages {
                out.extend_from_slice(p);
            }
            out
        }
    }

    fn build_wal_bytes(
        page_size: usize,
        big_endian_words: bool,
        salt1: u32,
        salt2: u32,
        frames: &[(u32, u32, &[u8])],
    ) -> Vec<u8> {
        let magic = if big_endian_words {
            WAL_MAGIC_BE_CHECKSUM
        } else {
            WAL_MAGIC_LE_CHECKSUM
        };
        let mut wh = [0u8; 32];
        wh[0..4].copy_from_slice(&magic.to_be_bytes());
        wh[4..8].copy_from_slice(&WAL_FORMAT_VERSION.to_be_bytes());
        wh[8..12].copy_from_slice(&(page_size as u32).to_be_bytes());
        wh[16..20].copy_from_slice(&salt1.to_be_bytes());
        wh[20..24].copy_from_slice(&salt2.to_be_bytes());
        let (mut s0, mut s1) = wal_checksum(&wh[0..24], 0, 0, big_endian_words);
        wh[24..28].copy_from_slice(&s0.to_be_bytes());
        wh[28..32].copy_from_slice(&s1.to_be_bytes());

        let mut out = Vec::from(wh);
        for &(pgno, db_size, page_data) in frames {
            assert_eq!(page_data.len(), page_size);
            let mut fh = [0u8; 24];
            fh[0..4].copy_from_slice(&pgno.to_be_bytes());
            fh[4..8].copy_from_slice(&db_size.to_be_bytes());
            fh[8..12].copy_from_slice(&salt1.to_be_bytes());
            fh[12..16].copy_from_slice(&salt2.to_be_bytes());
            (s0, s1) = wal_checksum(&fh[0..8], s0, s1, big_endian_words);
            (s0, s1) = wal_checksum(page_data, s0, s1, big_endian_words);
            fh[16..20].copy_from_slice(&s0.to_be_bytes());
            fh[20..24].copy_from_slice(&s1.to_be_bytes());
            out.extend_from_slice(&fh);
            out.extend_from_slice(page_data);
        }
        out
    }

    #[test]
    fn reads_gen_metadata_with_multi_page_overflow_skip_and_interior_btree() {
        let tmp = TempDirGuard::new("overflow");
        let db_path = tmp.0.join("conv.db");

        // Use 4KB pages with reserved=16 so usable=4080 and >350KB blob spans ~90 overflow pages!
        let mut db = DbBuilder::new(4096, 16);
        let gen_root = db.alloc_page();
        let gen_left = db.alloc_page();
        let gen_right = db.alloc_page();
        let exec_root = db.alloc_page();

        let schema_rows = vec![
            (
                1,
                build_sqlite_record(&[
                    SqlCol::Text("table"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Int(gen_root as i64),
                    SqlCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                ]),
            ),
            (
                2,
                build_sqlite_record(&[
                    SqlCol::Text("table"),
                    SqlCol::Text("executor_metadata"),
                    SqlCol::Text("executor_metadata"),
                    SqlCol::Int(exec_root as i64),
                    SqlCol::Text("CREATE TABLE executor_metadata(idx INT, data BLOB)"),
                ]),
            ),
        ];
        db.build_leaf_page(1, &schema_rows);

        // Older primary row on left leaf
        let old_blob = build_gen_blob(
            Some(256_000),
            Some("gemini-4-carbon"),
            Some("Gemini 4 Carbon (High)"),
            Some("MODEL_PLACEHOLDER_M427"),
            None,
            0,
        );
        db.build_leaf_page(
            gen_left,
            &[(
                1,
                build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&old_blob)]),
            )],
        );

        // Newest primary row on right leaf with a 350KB giant field 2 before fields 9/19/20/21!
        let giant_blob = build_gen_blob(
            Some(512_000),
            Some("gemini-4-hydrogen"),
            Some("Gemini 4 Hydrogen (XHigh)"),
            Some("MODEL_PLACEHOLDER_M528"),
            None,
            350_000,
        );
        db.build_leaf_page(
            gen_right,
            &[(
                2,
                build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&giant_blob)]),
            )],
        );
        db.build_interior_page(gen_root, &[(gen_left, 1)], gen_right);

        let exec_blob = build_exec_blob(512_000);
        db.build_leaf_page(
            exec_root,
            &[(
                1,
                build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&exec_blob)]),
            )],
        );

        fs::write(&db_path, db.to_bytes()).unwrap();

        let t0 = Instant::now();
        let obs = read_db_ceiling_from_path(&db_path, None, test_is_shadow)
            .unwrap()
            .expect("observation");
        let elapsed = t0.elapsed();
        assert!(elapsed.as_millis() < 250, "too slow: {elapsed:?}");

        assert_eq!(
            obs,
            DbCeilingObservation {
                ceiling_tokens: 512_000,
                origin: DbCeilingOrigin::GenMetadata,
                model_id: Some("gemini-4-hydrogen".to_string()),
                display_name: Some("Gemini 4 Hydrogen (XHigh)".to_string()),
                model_enum: Some("MODEL_PLACEHOLDER_M528".to_string()),
                subagent_type: None,
            }
        );
    }

    #[test]
    fn wal_overlay_applies_committed_frames_and_discards_uncommitted_and_corrupt_trailing() {
        for big_endian_words in [false, true] {
            let tmp = TempDirGuard::new(if big_endian_words { "wal_be" } else { "wal_le" });
            let db_path = tmp.0.join("wal.db");
            let wal_path = wal_path_for(&db_path);

            let mut db = DbBuilder::new(4096, 0);
            let gen_root = db.alloc_page();
            db.build_leaf_page(
                1,
                &[(
                    1,
                    build_sqlite_record(&[
                        SqlCol::Text("table"),
                        SqlCol::Text("gen_metadata"),
                        SqlCol::Text("gen_metadata"),
                        SqlCol::Int(gen_root as i64),
                        SqlCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                    ]),
                )],
            );

            // Base DB has 256_000
            let base_blob = build_gen_blob(
                Some(256_000),
                Some("gemini-4-carbon"),
                Some("Gemini 4 Carbon (High)"),
                None,
                None,
                0,
            );
            db.build_leaf_page(
                gen_root,
                &[(
                    1,
                    build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&base_blob)]),
                )],
            );
            fs::write(&db_path, db.to_bytes()).unwrap();

            // WAL frame 1 (committed db_size=2): updates gen_root to 840_000
            let mut db_committed = DbBuilder::new(4096, 0);
            let _ = db_committed.alloc_page();
            let committed_blob = build_gen_blob(
                Some(840_000),
                Some("claude-opus-5-5-medium"),
                Some("Claude Opus 5.5 (Medium)"),
                None,
                None,
                0,
            );
            db_committed.build_leaf_page(
                gen_root,
                &[(
                    2,
                    build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&committed_blob)]),
                )],
            );
            let committed_page = db_committed.pages[gen_root as usize - 1].clone();

            // WAL frame 2 (uncommitted db_size=0): tries to change to 128_000
            let mut db_uncommitted = DbBuilder::new(4096, 0);
            let _ = db_uncommitted.alloc_page();
            let uncommitted_blob =
                build_gen_blob(Some(128_000), Some("gemini-3.6-flash"), None, None, None, 0);
            db_uncommitted.build_leaf_page(
                gen_root,
                &[(
                    3,
                    build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&uncommitted_blob)]),
                )],
            );
            let uncommitted_page = db_uncommitted.pages[gen_root as usize - 1].clone();

            let mut wal_bytes = build_wal_bytes(
                4096,
                big_endian_words,
                0x1122_3344,
                0x5566_7788,
                &[
                    (gen_root, 2, &committed_page),
                    (gen_root, 0, &uncommitted_page),
                ],
            );
            // Also append truncated trailing garbage simulating mid-write frame
            wal_bytes.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]);
            fs::write(&wal_path, wal_bytes).unwrap();

            let obs = read_db_ceiling_from_path(&db_path, None, test_is_shadow)
                .unwrap()
                .expect("committed WAL observation");
            assert_eq!(obs.ceiling_tokens, 840_000);
            assert_eq!(obs.model_id.as_deref(), Some("claude-opus-5-5-medium"));
            assert!(!tmp.0.join("wal.db-shm").exists());
        }
    }

    #[test]
    fn skips_shadow_and_subagent_rows_unless_hint_matches_and_falls_back_to_executor() {
        let tmp = TempDirGuard::new("shadow");
        let db_path = tmp.0.join("shadow.db");

        let mut db = DbBuilder::new(4096, 0);
        let gen_root = db.alloc_page();
        let exec_root = db.alloc_page();
        db.build_leaf_page(
            1,
            &[
                (
                    1,
                    build_sqlite_record(&[
                        SqlCol::Text("table"),
                        SqlCol::Text("gen_metadata"),
                        SqlCol::Text("gen_metadata"),
                        SqlCol::Int(gen_root as i64),
                        SqlCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                    ]),
                ),
                (
                    2,
                    build_sqlite_record(&[
                        SqlCol::Text("table"),
                        SqlCol::Text("executor_metadata"),
                        SqlCol::Text("executor_metadata"),
                        SqlCol::Int(exec_root as i64),
                        SqlCol::Text("CREATE TABLE executor_metadata(idx INT, data BLOB)"),
                    ]),
                ),
            ],
        );

        let primary = build_gen_blob(
            Some(512_000),
            Some("gemini-4-hydrogen"),
            Some("Gemini 4 Hydrogen (XHigh)"),
            None,
            None,
            0,
        );
        let shadow_lite = build_gen_blob(
            Some(1_048_576),
            Some("gemini-2.5-flash-lite"),
            Some("Flash Lite"),
            None,
            None,
            0,
        );
        let shadow_m50 = build_gen_blob(
            Some(256_000),
            None,
            None,
            Some("MODEL_PLACEHOLDER_M50"),
            None,
            0,
        );
        let sub_worker = build_gen_blob(
            Some(256_000),
            Some("gemini-4-carbon"),
            Some("Gemini 4 Carbon (High)"),
            None,
            Some("worker"),
            0,
        );
        let sub_image = build_gen_blob(
            Some(256_000),
            Some("gemini-3.8-flash-tiered"),
            None,
            None,
            Some("image-generator"),
            0,
        );

        db.build_leaf_page(
            gen_root,
            &[
                (
                    1,
                    build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&primary)]),
                ),
                (
                    2,
                    build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&shadow_lite)]),
                ),
                (
                    3,
                    build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&shadow_m50)]),
                ),
                (
                    4,
                    build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&sub_worker)]),
                ),
                (
                    5,
                    build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&sub_image)]),
                ),
            ],
        );
        db.build_leaf_page(
            exec_root,
            &[(
                1,
                build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&build_exec_blob(777_000))]),
            )],
        );
        fs::write(&db_path, db.to_bytes()).unwrap();

        // Primary row wins over trailing shadow/subagent rows
        let obs =
            read_db_ceiling_from_path(&db_path, Some("Gemini 4 Hydrogen (XHigh)"), test_is_shadow)
                .unwrap()
                .unwrap();
        assert_eq!(obs.ceiling_tokens, 512_000);
        assert_eq!(obs.origin, DbCeilingOrigin::GenMetadata);

        // If user explicitly runs shadow model gemini-2.5-flash-lite, primary row still isn't shadow,
        // so test a DB with ONLY shadow_lite + subagent + executor_metadata fallback:
        let shadow_only_path = tmp.0.join("shadow_only.db");
        let mut db2 = DbBuilder::new(4096, 0);
        let gr2 = db2.alloc_page();
        let er2 = db2.alloc_page();
        db2.build_leaf_page(
            1,
            &[
                (
                    1,
                    build_sqlite_record(&[
                        SqlCol::Text("table"),
                        SqlCol::Text("gen_metadata"),
                        SqlCol::Text("gen_metadata"),
                        SqlCol::Int(gr2 as i64),
                        SqlCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                    ]),
                ),
                (
                    2,
                    build_sqlite_record(&[
                        SqlCol::Text("table"),
                        SqlCol::Text("executor_metadata"),
                        SqlCol::Text("executor_metadata"),
                        SqlCol::Int(er2 as i64),
                        SqlCol::Text("CREATE TABLE executor_metadata(idx INT, data BLOB)"),
                    ]),
                ),
            ],
        );
        db2.build_leaf_page(
            gr2,
            &[
                (
                    1,
                    build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&shadow_lite)]),
                ),
                (
                    2,
                    build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&sub_worker)]),
                ),
            ],
        );
        db2.build_leaf_page(
            er2,
            &[(
                1,
                build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&build_exec_blob(777_000))]),
            )],
        );
        fs::write(&shadow_only_path, db2.to_bytes()).unwrap();

        // Without matching hint -> falls back to executor_metadata (777_000)
        let fallback_obs =
            read_db_ceiling_from_path(&shadow_only_path, Some("gemini-4-hydrogen"), test_is_shadow)
                .unwrap()
                .unwrap();
        assert_eq!(fallback_obs.ceiling_tokens, 777_000);
        assert_eq!(fallback_obs.origin, DbCeilingOrigin::ExecutorMetadata);

        // With hint matching subagent row ("Gemini 4 Carbon (High)") -> selects gen_metadata subagent row
        let hint_sub = read_db_ceiling_from_path(
            &shadow_only_path,
            Some("Gemini 4 Carbon (High)"),
            test_is_shadow,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hint_sub.ceiling_tokens, 256_000);
        assert_eq!(hint_sub.origin, DbCeilingOrigin::GenMetadata);
        assert_eq!(hint_sub.subagent_type.as_deref(), Some("worker"));

        // With hint explicitly matching shadow model ("gemini-2.5-flash-lite") -> selects shadow row
        let hint_shadow = read_db_ceiling_from_path(
            &shadow_only_path,
            Some("gemini-2.5-flash-lite"),
            test_is_shadow,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hint_shadow.ceiling_tokens, 1_048_576);
        assert_eq!(
            hint_shadow.model_id.as_deref(),
            Some("gemini-2.5-flash-lite")
        );
    }

    #[test]
    fn page_size_65536_and_missing_empty_corrupt_dbs_handled_gracefully() {
        let tmp = TempDirGuard::new("edge");

        // 1. Missing DB
        assert_eq!(
            read_db_ceiling_from_path(&tmp.0.join("missing.db"), None, test_is_shadow).unwrap(),
            None
        );

        // 2. Empty DB
        let empty_db = tmp.0.join("empty.db");
        fs::write(&empty_db, b"").unwrap();
        assert_eq!(
            read_db_ceiling_from_path(&empty_db, None, test_is_shadow).unwrap(),
            None
        );

        // 3. Corrupt / truncated DB
        let corrupt_db = tmp.0.join("corrupt.db");
        fs::write(&corrupt_db, b"SQLite format 3\0garbage").unwrap();
        assert_eq!(
            read_db_ceiling_from_path(&corrupt_db, None, test_is_shadow).unwrap(),
            None
        );

        // 4. Valid DB with page_size == 1 (65536 bytes)
        let ps64_db = tmp.0.join("ps64k.db");
        let mut db = DbBuilder::new(65_536, 0);
        let gen_root = db.alloc_page();
        db.build_leaf_page(
            1,
            &[(
                1,
                build_sqlite_record(&[
                    SqlCol::Text("table"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Int(gen_root as i64),
                    SqlCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                ]),
            )],
        );
        let blob = build_gen_blob(
            Some(1_048_576),
            Some("gemini-4-hydrogen"),
            None,
            None,
            None,
            0,
        );
        db.build_leaf_page(
            gen_root,
            &[(1, build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&blob)]))],
        );
        fs::write(&ps64_db, db.to_bytes()).unwrap();
        let obs = read_db_ceiling_from_path(&ps64_db, None, test_is_shadow)
            .unwrap()
            .unwrap();
        assert_eq!(obs.ceiling_tokens, 1_048_576);
    }

    #[test]
    fn cached_reader_skips_db_reread_until_db_or_wal_stat_changes() {
        let tmp = TempDirGuard::new("cache");
        let db_path = tmp.0.join("cached.db");
        let wal_path = wal_path_for(&db_path);
        let cache_path = tmp.0.join("db-ceilings").join("entry.json");

        let mut db = DbBuilder::new(4096, 0);
        let gen_root = db.alloc_page();
        db.build_leaf_page(
            1,
            &[(
                1,
                build_sqlite_record(&[
                    SqlCol::Text("table"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Int(gen_root as i64),
                    SqlCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                ]),
            )],
        );
        let blob_256 = build_gen_blob(Some(256_000), Some("gemini-4-carbon"), None, None, None, 0);
        db.build_leaf_page(
            gen_root,
            &[(
                1,
                build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&blob_256)]),
            )],
        );
        fs::write(&db_path, db.to_bytes()).unwrap();

        // Cold read populates cache
        let first = read_db_ceiling_cached(&db_path, Some(&cache_path), None, test_is_shadow)
            .expect("first observation");
        assert_eq!(first.ceiling_tokens, 256_000);
        assert!(cache_path.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&cache_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Alter cached observation while keeping stat fields identical; hit must return cached value.
        let mut cached_rec: DbCeilingCacheRecord =
            serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
        cached_rec.observation.as_mut().unwrap().ceiling_tokens = 333_000;
        fs::write(&cache_path, serde_json::to_vec(&cached_rec).unwrap()).unwrap();

        let hit = read_db_ceiling_cached(&db_path, Some(&cache_path), None, test_is_shadow)
            .expect("cache hit");
        assert_eq!(hit.ceiling_tokens, 333_000);

        // Changing active_model_hint invalidates cache and refreshes from DB
        let hint_refresh = read_db_ceiling_cached(
            &db_path,
            Some(&cache_path),
            Some("gemini-4-carbon"),
            test_is_shadow,
        )
        .expect("hint miss refreshes");
        assert_eq!(hint_refresh.ceiling_tokens, 256_000);

        // Now append a WAL commit frame updating ceiling to 840_000 -> WAL stat changes -> refreshes!
        let mut db_new = DbBuilder::new(4096, 0);
        let _ = db_new.alloc_page();
        let blob_840 = build_gen_blob(
            Some(840_000),
            Some("claude-opus-5-5-medium"),
            None,
            None,
            None,
            0,
        );
        db_new.build_leaf_page(
            gen_root,
            &[(
                2,
                build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&blob_840)]),
            )],
        );
        let wal_bytes = build_wal_bytes(
            4096,
            false,
            0xdead_beef,
            0xcafe_babe,
            &[(gen_root, 2, &db_new.pages[gen_root as usize - 1])],
        );
        let mut wf = File::create(&wal_path).unwrap();
        wf.write_all(&wal_bytes).unwrap();
        wf.sync_all().unwrap();

        let refreshed = read_db_ceiling_cached(
            &db_path,
            Some(&cache_path),
            Some("claude-opus-5-5-medium"),
            test_is_shadow,
        )
        .expect("refreshed observation");
        assert_eq!(refreshed.ceiling_tokens, 840_000);
    }

    #[test]
    fn overflow_cycle_and_btree_cycle_and_bad_wal_header_handled_safely() {
        let tmp = TempDirGuard::new("cycles");

        // 1. Overflow cycle must terminate cleanly with Ok(None)
        let ov_cycle_db = tmp.0.join("ov_cycle.db");
        let mut db = DbBuilder::new(4096, 0);
        let gen_root = db.alloc_page();
        db.build_leaf_page(
            1,
            &[(
                1,
                build_sqlite_record(&[
                    SqlCol::Text("table"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Int(gen_root as i64),
                    SqlCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                ]),
            )],
        );
        let giant_blob = build_gen_blob(
            Some(512_000),
            Some("gemini-4-hydrogen"),
            None,
            None,
            None,
            20_000,
        );
        db.build_leaf_page(
            gen_root,
            &[(
                1,
                build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&giant_blob)]),
            )],
        );
        // Point page 4 back to page 3 creating an overflow loop
        db.pages[3][0..4].copy_from_slice(&3u32.to_be_bytes());
        fs::write(&ov_cycle_db, db.to_bytes()).unwrap();
        assert_eq!(
            read_db_ceiling_from_path(&ov_cycle_db, None, test_is_shadow).unwrap(),
            None
        );

        // 2. B-tree interior cycle must terminate cleanly with Ok(None)
        let bt_cycle_db = tmp.0.join("bt_cycle.db");
        let mut db2 = DbBuilder::new(4096, 0);
        let loop_root = db2.alloc_page();
        db2.build_leaf_page(
            1,
            &[(
                1,
                build_sqlite_record(&[
                    SqlCol::Text("table"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Int(loop_root as i64),
                    SqlCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                ]),
            )],
        );
        db2.build_interior_page(loop_root, &[], loop_root);
        fs::write(&bt_cycle_db, db2.to_bytes()).unwrap();
        assert_eq!(
            read_db_ceiling_from_path(&bt_cycle_db, None, test_is_shadow).unwrap(),
            None
        );

        // 3. Corrupt WAL header (bad checksum) falls back cleanly to base .db
        let bad_wal_db = tmp.0.join("bad_wal.db");
        let mut db3 = DbBuilder::new(4096, 0);
        let gr3 = db3.alloc_page();
        db3.build_leaf_page(
            1,
            &[(
                1,
                build_sqlite_record(&[
                    SqlCol::Text("table"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Int(gr3 as i64),
                    SqlCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                ]),
            )],
        );
        let base_blob = build_gen_blob(
            Some(512_000),
            Some("gemini-4-hydrogen"),
            None,
            None,
            None,
            0,
        );
        db3.build_leaf_page(
            gr3,
            &[(
                1,
                build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&base_blob)]),
            )],
        );
        fs::write(&bad_wal_db, db3.to_bytes()).unwrap();
        let mut corrupt_wal = build_wal_bytes(4096, false, 1, 2, &[(gr3, 2, &vec![0u8; 4096])]);
        corrupt_wal[24] ^= 0xff; // invalidate WAL header checksum
        fs::write(wal_path_for(&bad_wal_db), corrupt_wal).unwrap();

        let obs = read_db_ceiling_from_path(&bad_wal_db, None, test_is_shadow)
            .unwrap()
            .unwrap();
        assert_eq!(obs.ceiling_tokens, 512_000);
    }

    #[test]
    fn straddled_overflow_boundary_and_reversed_f20_order_decode_cleanly() {
        let tmp = TempDirGuard::new("straddle");
        let db_path = tmp.0.join("straddle.db");

        // Build ChatModel where f20 has field 2 BEFORE field 1, and fields straddle 4KB overflow boundary
        let mut chat = Vec::new();
        push_pb_bytes_field(&mut chat, 2, &vec![b'Z'; 4060]);
        let mut f10 = Vec::new();
        push_pb_varint_field(&mut f10, 4, 840_000);
        let mut f9 = Vec::new();
        push_pb_bytes_field(&mut f9, 10, &f10);
        push_pb_bytes_field(&mut chat, 9, &f9);
        push_pb_bytes_field(&mut chat, 19, b"claude-opus-5-5-medium");

        // Reversed field order inside f20: val (field 2) before key (field 1)
        let mut f20_rev = Vec::new();
        push_pb_bytes_field(&mut f20_rev, 2, b"MODEL_PLACEHOLDER_M401");
        push_pb_bytes_field(&mut f20_rev, 1, b"model_enum");
        push_pb_bytes_field(&mut chat, 20, &f20_rev);
        push_pb_bytes_field(&mut chat, 21, b"Claude Opus 5.5 (Medium)");

        let mut blob = Vec::new();
        push_pb_bytes_field(&mut blob, 1, &chat);

        let mut db = DbBuilder::new(4096, 0);
        let gen_root = db.alloc_page();
        db.build_leaf_page(
            1,
            &[(
                1,
                build_sqlite_record(&[
                    SqlCol::Text("table"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Text("gen_metadata"),
                    SqlCol::Int(gen_root as i64),
                    SqlCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                ]),
            )],
        );
        db.build_leaf_page(
            gen_root,
            &[(1, build_sqlite_record(&[SqlCol::Null, SqlCol::Blob(&blob)]))],
        );
        fs::write(&db_path, db.to_bytes()).unwrap();

        let obs = read_db_ceiling_from_path(&db_path, None, test_is_shadow)
            .unwrap()
            .unwrap();
        assert_eq!(obs.ceiling_tokens, 840_000);
        assert_eq!(obs.model_id.as_deref(), Some("claude-opus-5-5-medium"));
        assert_eq!(obs.model_enum.as_deref(), Some("MODEL_PLACEHOLDER_M401"));
        assert_eq!(
            obs.display_name.as_deref(),
            Some("Claude Opus 5.5 (Medium)")
        );
    }
}
