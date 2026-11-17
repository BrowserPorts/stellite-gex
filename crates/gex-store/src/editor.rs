//! The store edited while no system holds it: its records read and changed
//! by path, and written as the next generation ([`Editor::commit`]), as the
//! kernel writes one. Only the paths edited change: every other record (a
//! run's removals of the sources' files, its modes, its links) is written
//! again as it was read.
//!
//! A file's bytes go into blocks no generation names (never over a block
//! the last generation names: a block of a file the last generation holds
//! is copied before it is changed), and become its once the generation
//! naming them is written. A store whose generations do not read is never
//! written ([`Editor::open`] fails with EIO).
//!
//! Paths are absolute (`/home/user/a`), and kept without the leading `/`.

use crate::alloc::Allocator;
use crate::format::{self, Blocks, Body, Found, Journal, Rec};
use crate::{Errno, OP_READ, OP_WRITE};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// The store file, as the editor's host reaches it: one call per operation,
/// as `blocks.mjs` `handleOp` answers it.
pub trait Io {
    fn op(&mut self, op: u32, off: u64, buf: &mut [u8]) -> Result<(), Errno>;
}

/// The inode numbers the editor gives files: above any a kernel gives, so
/// that a file written here is never taken for a hard link of one a run
/// wrote (records name links by inode within a generation).
const EDITOR_INO: u64 = 1 << 62;

struct Store<I: Io> {
    io: I,
    bs: u64,
    alloc: Allocator,
}

impl<I: Io> Store<I> {
    fn alloc_place(&mut self) -> Result<u32, Errno> {
        let bs = self.bs;
        let Store { io, alloc, .. } = self;
        alloc.alloc_place(bs, &mut |len| io.op(crate::OP_GROW, len, &mut []))
    }
}

impl<I: Io> Blocks for Store<I> {
    fn block(&self) -> u64 {
        self.bs
    }
    fn raw(&mut self, op: u32, off: u64, buf: &mut [u8]) -> Result<(), Errno> {
        self.io.op(op, off, buf)
    }
    fn alloc_meta(&mut self) -> Result<u32, Errno> {
        let bs = self.bs;
        let Store { io, alloc, .. } = self;
        alloc.alloc_meta(bs, &mut |len| io.op(crate::OP_GROW, len, &mut []))
    }
    fn release(&mut self, v: Vec<u32>) {
        self.alloc.release(v);
    }
    fn high(&self) -> u32 {
        self.alloc.high()
    }
}

/// One entry of a listing.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    pub size: u64,
    pub mode: u32,
    pub mtime: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    File,
    Dir,
    Link,
    /// A path of the sources removed.
    Gone,
    /// A file of the sources whose mode or attributes changed.
    Attrs,
    /// A file whose bytes are fetched from a URL (not readable here).
    Lazy,
}

pub struct Editor<I: Io> {
    s: Store<I>,
    journal: Journal,
    found: Found,
    recs: BTreeMap<String, Rec>,
    /// Paths whose records changed, and paths whose records went, since the
    /// last generation.
    puts: BTreeSet<String>,
    drops: BTreeSet<String>,
    /// Blocks taken since the last generation: no generation names them, so
    /// they may be written over.
    fresh: HashSet<u32>,
    next_ino: u64,
    inline: u64,
}

/// A path as a record names it: absolute, made plain, without the `/`.
pub fn key(path: &str) -> Result<String, Errno> {
    if !path.starts_with('/') {
        return Err(Errno::EINVAL);
    }
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    if out.is_empty() {
        return Err(Errno::EINVAL);
    }
    Ok(out.join("/"))
}

fn under(p: &str, at: &str) -> bool {
    at.is_empty() || p == at || (p.len() > at.len() && p.starts_with(at) && p.as_bytes()[at.len()] == b'/')
}

impl<I: Io> Editor<I> {
    /// The store behind `io`, its blocks `bs` bytes (the kernel's: 64 KiB
    /// for an `OPFSFilesystem`), files up to `inline` bytes kept in their
    /// records. EIO when something is kept but no generation reads.
    pub fn open(io: I, bs: u64, inline: u64) -> Result<Editor<I>, Errno> {
        if bs < 4096 || !bs.is_power_of_two() || bs > 1 << 24 {
            return Err(Errno::EINVAL);
        }
        let mut s = Store { io, bs, alloc: Allocator::new() };
        s.alloc.start();
        let l = format::load(&mut s);
        if l.found == Found::Unreadable {
            return Err(Errno::EIO);
        }
        let high = if l.found == Found::Fresh { 1 } else { l.journal.sb.high };
        s.alloc.rebuild(high, &l.used);
        s.alloc.defer_frees();
        let next_ino = l
            .recs
            .values()
            .filter_map(|r| match r {
                Rec::File { ino, .. } if *ino >= EDITOR_INO => Some(ino + 1),
                _ => None,
            })
            .max()
            .unwrap_or(EDITOR_INO);
        Ok(Editor { s, journal: l.journal, found: l.found, recs: l.recs, puts: BTreeSet::new(), drops: BTreeSet::new(), fresh: HashSet::new(), next_ino, inline })
    }

    /// What the open found: a fresh store, its newest generation, or the
    /// one before it (the newest torn).
    pub fn found(&self) -> Found {
        self.found
    }

    /// The generation the records were read from, or last written.
    pub fn generation(&self) -> u64 {
        self.journal.sb.gen
    }

    /// Whether anything changed since the last generation.
    pub fn dirty(&self) -> bool {
        !self.puts.is_empty() || !self.drops.is_empty()
    }

    fn entry(p: &str, r: &Rec) -> Entry {
        let (kind, size, mode, mtime) = match r {
            Rec::Gone => (Kind::Gone, 0, 0, 0),
            Rec::Dir { mode, mtime, .. } => (Kind::Dir, 0, *mode, *mtime),
            Rec::Link { target } => (Kind::Link, target.len() as u64, 0o777, 0),
            Rec::Mode { mode } | Rec::Attrs { mode, .. } => (Kind::Attrs, 0, *mode, 0),
            Rec::File { mode, mtime, body, .. } => (if matches!(body, Body::Lazy(..)) { Kind::Lazy } else { Kind::File }, body.size(), *mode, *mtime),
        };
        Entry { path: format!("/{p}"), kind, size, mode, mtime }
    }

    /// Every record at or under `prefix` (an absolute path, `/` for all),
    /// in path order.
    pub fn list(&self, prefix: &str) -> Result<Vec<Entry>, Errno> {
        let at = if prefix.trim_matches('/').is_empty() { String::new() } else { key(prefix)? };
        Ok(self.recs.range(at.clone()..).take_while(|(p, _)| at.is_empty() || p.starts_with(&at)).filter(|(p, _)| under(p, &at)).map(|(p, r)| Self::entry(p, r)).collect())
    }

    /// The record at `path`.
    pub fn stat(&self, path: &str) -> Result<Entry, Errno> {
        let k = key(path)?;
        self.recs.get(&k).map(|r| Self::entry(&k, r)).ok_or(Errno::ENOENT)
    }

    /// The link's target at `path`.
    pub fn readlink(&self, path: &str) -> Result<String, Errno> {
        match self.recs.get(&key(path)?) {
            Some(Rec::Link { target }) => Ok(target.clone()),
            Some(_) => Err(Errno::EINVAL),
            None => Err(Errno::ENOENT),
        }
    }

    /// `buf.len()` bytes of the file at `path` from `offset`, fewer at its
    /// end: how many.
    pub fn read(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let k = key(path)?;
        let (size, map) = match self.recs.get(&k) {
            Some(Rec::File { body: Body::Inline(d), .. }) => {
                let from = (offset as usize).min(d.len());
                let n = (d.len() - from).min(buf.len());
                buf[..n].copy_from_slice(&d[from..from + n]);
                return Ok(n);
            }
            Some(Rec::File { body: Body::Blocks(size, map), .. }) => (*size, map.clone()),
            Some(Rec::File { body: Body::Lazy(..), .. }) => return Err(Errno::ENXIO),
            Some(Rec::Dir { .. }) => return Err(Errno::EISDIR),
            Some(_) => return Err(Errno::EINVAL),
            None => return Err(Errno::ENOENT),
        };
        let bs = self.s.bs;
        let end = (offset + buf.len() as u64).min(size);
        let mut at = offset;
        while at < end {
            let b = at / bs;
            let from = at % bs;
            // A run of blocks that sit one after another in the store, in
            // one read.
            let mut n = (end - at).min(bs - from);
            let place = map.get(b as usize).copied().unwrap_or(0);
            let o = (at - offset) as usize;
            if place == 0 {
                buf[o..o + n as usize].fill(0);
            } else {
                let mut k = 1;
                while from == 0 && at + n < end && map.get((b + k) as usize).copied() == Some(place + k as u32) {
                    n += (end - at - n).min(bs);
                    k += 1;
                }
                self.s.io.op(OP_READ, (place - 1) as u64 * bs + from, &mut buf[o..o + n as usize])?;
            }
            at += n;
        }
        Ok((end.max(offset) - offset) as usize)
    }

    fn put(&mut self, k: String, r: Rec) {
        self.drops.remove(&k);
        self.recs.insert(k.clone(), r);
        self.puts.insert(k);
    }

    fn drop_rec(&mut self, k: &str) {
        if let Some(Rec::File { body: Body::Blocks(_, map), ino, .. }) = self.recs.remove(k) {
            // A hard link's blocks stay while another path names its inode.
            let linked = self.recs.values().any(|r| matches!(r, Rec::File { ino: i, .. } if *i == ino));
            if !linked {
                for b in map.into_iter().filter(|&b| b != 0) {
                    self.free(b - 1);
                }
            }
        }
        self.puts.remove(k);
        self.drops.insert(k.to_string());
    }

    fn free(&mut self, b: u32) {
        if self.fresh.remove(&b) {
            // Never named by a generation: free at once.
            self.s.alloc.release([b]);
        } else {
            self.s.alloc.free(b);
        }
    }

    /// A block of the file that may be written over: its own if no
    /// generation names it, else a new one (the old one's bytes copied in
    /// when `keep`), the old freed with the next generation.
    fn writable(&mut self, map: &mut Vec<u32>, b: usize, keep: bool) -> Result<u32, Errno> {
        if map.len() <= b {
            map.resize(b + 1, 0);
        }
        let old = map[b];
        if old != 0 && self.fresh.contains(&(old - 1)) {
            return Ok(old - 1);
        }
        let new = self.s.alloc_place()?;
        self.fresh.insert(new);
        if old != 0 {
            if keep {
                let bs = self.s.bs;
                let mut d = vec![0u8; bs as usize];
                self.s.io.op(OP_READ, (old - 1) as u64 * bs, &mut d)?;
                self.s.io.op(OP_WRITE, new as u64 * bs, &mut d)?;
            }
            self.free(old - 1);
        }
        map[b] = new + 1;
        Ok(new)
    }

    /// Write `data` into the file at `path` at `offset`, made (`mode`) if
    /// missing or `truncate`d first; the file's time set to `mtime`
    /// (seconds). Its parents need no records: a boot makes them.
    pub fn write(&mut self, path: &str, offset: u64, data: &[u8], mode: u32, mtime: i64, truncate: bool) -> Result<(), Errno> {
        let k = key(path)?;
        let (ino, mode, xattrs, mut size, mut map, mut inline) = match self.recs.get(&k) {
            Some(Rec::File { ino, mode: m, body, xattrs, .. }) if !truncate => match body {
                Body::Inline(d) => (*ino, *m, xattrs.clone(), d.len() as u64, Vec::new(), Some(d.clone())),
                Body::Blocks(size, map) => (*ino, *m, xattrs.clone(), *size, map.clone(), None),
                Body::Lazy(..) => return Err(Errno::ENXIO),
            },
            Some(Rec::Dir { .. }) => return Err(Errno::EISDIR),
            existing => {
                // A new file: an old one's blocks freed, its inode kept for
                // any link to it.
                let ino = match existing {
                    Some(Rec::File { ino, .. }) => *ino,
                    _ => {
                        self.next_ino += 1;
                        self.next_ino - 1
                    }
                };
                if let Some(Rec::File { body: Body::Blocks(_, map), .. }) = existing {
                    let map = map.clone();
                    for b in map.into_iter().filter(|&b| b != 0) {
                        self.free(b - 1);
                    }
                }
                (ino, mode, Vec::new(), 0, Vec::new(), Some(Vec::new()))
            }
        };
        let end = offset.checked_add(data.len() as u64).ok_or(Errno::EFBIG)?;
        let new_size = size.max(end);
        // A small file stays in its record.
        if let Some(d) = inline.as_mut() {
            if new_size <= self.inline {
                if d.len() < new_size as usize {
                    d.resize(new_size as usize, 0);
                }
                d[offset as usize..end as usize].copy_from_slice(data);
                let body = Body::Inline(std::mem::take(d));
                self.put(k, Rec::File { ino, mode, mtime, body, xattrs });
                return Ok(());
            }
        }
        let bs = self.s.bs;
        // An inline file growing past the bound moves into blocks first.
        if let Some(d) = inline.take() {
            let d = d.to_vec();
            if !d.is_empty() {
                self.write_blocks(&mut map, 0, &d, 0)?;
            }
        }
        self.write_blocks(&mut map, offset, data, size)?;
        size = new_size;
        map.truncate(size.div_ceil(bs) as usize);
        self.put(k, Rec::File { ino, mode, mtime, body: Body::Blocks(size, map), xattrs });
        Ok(())
    }

    /// `data` into the blocks of `map` at `offset`, of a file `size` bytes
    /// long: a partial block's other bytes kept, runs of new blocks written
    /// in one request where they sit together.
    fn write_blocks(&mut self, map: &mut Vec<u32>, offset: u64, data: &[u8], size: u64) -> Result<(), Errno> {
        let bs = self.s.bs;
        let end = offset + data.len() as u64;
        let mut at = offset;
        while at < end {
            let b = (at / bs) as usize;
            let from = at % bs;
            let n = (end - at).min(bs - from);
            // Whole blocks: a run of them, placed together where the
            // allocator allows, written in one request.
            if from == 0 && n == bs {
                let first = self.writable(map, b, false)?;
                let mut k = 1u64;
                while at + (k + 1) * bs <= end && k < 256 {
                    let next = self.writable(map, b + k as usize, false)?;
                    if next != first + k as u32 {
                        // Not adjacent: written on its own next time round.
                        break;
                    }
                    k += 1;
                }
                let o = (at - offset) as usize;
                let mut chunk = data[o..o + (k * bs) as usize].to_vec();
                self.s.io.op(OP_WRITE, first as u64 * bs, &mut chunk)?;
                at += k * bs;
                continue;
            }
            // Part of a block: the rest of it kept where the file has bytes.
            let keep = (b as u64) * bs < size;
            let p = self.writable(map, b, keep)?;
            let mut blk = vec![0u8; bs as usize];
            if keep {
                self.s.io.op(OP_READ, p as u64 * bs, &mut blk)?;
            }
            let o = (at - offset) as usize;
            blk[from as usize..(from + n) as usize].copy_from_slice(&data[o..o + n as usize]);
            self.s.io.op(OP_WRITE, p as u64 * bs, &mut blk)?;
            at += n;
        }
        Ok(())
    }

    /// The record at `path` and every one under it gone; with `hide`, a
    /// removal of what the sources have there instead (a boot lays the
    /// sources, then takes the path away).
    pub fn remove(&mut self, path: &str, hide: bool) -> Result<(), Errno> {
        let k = key(path)?;
        let under_k: Vec<String> = self.recs.range(k.clone()..).take_while(|(p, _)| p.starts_with(&k)).filter(|(p, _)| under(p, &k)).map(|(p, _)| p.clone()).collect();
        if under_k.is_empty() && !hide {
            return Err(Errno::ENOENT);
        }
        for p in under_k {
            self.drop_rec(&p);
        }
        if hide {
            self.put(k, Rec::Gone);
        }
        Ok(())
    }

    /// A directory record at `path`.
    pub fn mkdir(&mut self, path: &str, mode: u32, mtime: i64) -> Result<(), Errno> {
        let k = key(path)?;
        match self.recs.get(&k) {
            Some(Rec::Dir { .. }) => return Ok(()),
            Some(Rec::Gone) | None => {}
            Some(_) => return Err(Errno::EEXIST),
        }
        self.put(k, Rec::Dir { mode, mtime, xattrs: Vec::new() });
        Ok(())
    }

    /// A symlink record at `path`.
    pub fn symlink(&mut self, target: &str, path: &str) -> Result<(), Errno> {
        let k = key(path)?;
        if matches!(self.recs.get(&k), Some(Rec::Dir { .. })) {
            return Err(Errno::EISDIR);
        }
        if self.recs.contains_key(&k) {
            self.drop_rec(&k);
        }
        self.put(k, Rec::Link { target: target.to_string() });
        Ok(())
    }

    /// A file's or directory's mode, where it has a record.
    pub fn chmod(&mut self, path: &str, mode: u32) -> Result<(), Errno> {
        let k = key(path)?;
        let mut r = self.recs.get(&k).cloned().ok_or(Errno::ENOENT)?;
        match &mut r {
            Rec::File { mode: m, .. } | Rec::Dir { mode: m, .. } | Rec::Attrs { mode: m, .. } | Rec::Mode { mode: m } => *m = mode & 0o7777,
            _ => return Err(Errno::EINVAL),
        }
        self.put(k, r);
        Ok(())
    }

    /// The records at and under `from` moved to `to` (whatever `to` had
    /// gone first). The sources' files are not moved: only records.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), Errno> {
        let (f, t) = (key(from)?, key(to)?);
        if under(&t, &f) {
            return Err(Errno::EINVAL);
        }
        let moving: Vec<(String, Rec)> = self.recs.range(f.clone()..).take_while(|(p, _)| p.starts_with(&f)).filter(|(p, _)| under(p, &f)).map(|(p, r)| (p.clone(), r.clone())).collect();
        if moving.is_empty() {
            return Err(Errno::ENOENT);
        }
        let _ = self.remove(to, false);
        for (p, r) in moving {
            self.recs.remove(&p);
            self.puts.remove(&p);
            self.drops.insert(p.clone());
            self.put(format!("{t}{}", &p[f.len()..]), r);
        }
        Ok(())
    }

    /// What changed written as the next generation: the files' bytes are
    /// in already, then the records, a flush, the superblock, a flush. A
    /// segment of the changes, or every record (`checkpoint`, or when the
    /// journal is past its bound). Blocks freed since become free once it
    /// is written. Nothing changed and no checkpoint asked: nothing written.
    pub fn commit(&mut self, checkpoint: bool) -> Result<bool, Errno> {
        if !self.dirty() && !checkpoint {
            return Ok(false);
        }
        let gen = self.journal.next_gen();
        let seg = format::segment(gen, self.puts.iter().map(|p| (p.as_str(), self.recs[p].clone())), self.drops.iter().map(|p| p.as_str()));
        let fold = checkpoint || self.journal.fold(seg.len(), self.s.bs);
        let payload = if fold { format::checkpoint(gen, self.recs.iter().map(|(p, r)| (p.as_str(), r.clone()))) } else { seg };
        let pending = self.s.alloc.take_pending();
        match self.journal.write(&mut self.s, &payload, fold) {
            Ok(()) => {
                self.s.alloc.release(pending);
                self.puts.clear();
                self.drops.clear();
                self.fresh.clear();
                self.found = Found::Restored;
                Ok(true)
            }
            Err(e) => {
                self.s.alloc.repend(pending);
                Err(e)
            }
        }
    }

    /// The host, back.
    pub fn into_io(self) -> I {
        self.s.io
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store file in memory.
    #[derive(Default)]
    struct Mem(Vec<u8>);
    impl Io for &mut Mem {
        fn op(&mut self, op: u32, off: u64, buf: &mut [u8]) -> Result<(), Errno> {
            let (a, b) = (off as usize, off as usize + buf.len());
            match op {
                OP_READ => {
                    for (i, x) in buf.iter_mut().enumerate() {
                        *x = self.0.get(a + i).copied().unwrap_or(0);
                    }
                }
                OP_WRITE => {
                    if self.0.len() < b {
                        self.0.resize(b, 0);
                    }
                    self.0[a..b].copy_from_slice(buf);
                }
                crate::OP_GROW => {
                    if self.0.len() < a {
                        self.0.resize(a, 0);
                    }
                }
                _ => {}
            }
            Ok(())
        }
    }

    const BS: u64 = 4096;

    fn read_all(e: &mut Editor<&mut Mem>, p: &str) -> Vec<u8> {
        let size = e.stat(p).unwrap().size as usize;
        let mut out = vec![0u8; size];
        assert_eq!(e.read(p, 0, &mut out).unwrap(), size);
        out
    }

    fn big(n: usize, k: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(k)).collect()
    }

    #[test]
    fn files_written_read_back_after_a_reopen() {
        let mut m = Mem::default();
        let mut e = Editor::open(&mut m, BS, 512).unwrap();
        assert_eq!(e.found(), Found::Fresh);
        e.write("/home/a", 0, b"small", 0o644, 7, true).unwrap();
        e.write("/home/big", 0, &big(3 * BS as usize + 100, 1), 0o755, 8, true).unwrap();
        e.symlink("big", "/home/link").unwrap();
        e.mkdir("/home/d", 0o700, 9).unwrap();
        e.remove("/etc/old", true).unwrap();
        assert!(e.commit(false).unwrap());
        let mut e = Editor::open(&mut m, BS, 512).unwrap();
        assert_eq!(e.found(), Found::Restored);
        assert_eq!(read_all(&mut e, "/home/a"), b"small");
        assert_eq!(read_all(&mut e, "/home/big"), big(3 * BS as usize + 100, 1));
        assert_eq!(e.readlink("/home/link").unwrap(), "big");
        assert_eq!(e.stat("/home/d").unwrap().kind, Kind::Dir);
        assert_eq!(e.stat("/etc/old").unwrap().kind, Kind::Gone);
        assert_eq!(e.stat("/home/big").unwrap().mode, 0o755);
        let paths: Vec<String> = e.list("/home").unwrap().into_iter().map(|x| x.path).collect();
        assert_eq!(paths, ["/home/a", "/home/big", "/home/d", "/home/link"]);
    }

    #[test]
    fn a_changed_block_of_a_kept_file_is_copied_not_written_over() {
        let mut m = Mem::default();
        let mut e = Editor::open(&mut m, BS, 512).unwrap();
        e.write("/f", 0, &big(2 * BS as usize, 3), 0o644, 0, true).unwrap();
        e.commit(false).unwrap();
        let mut e = Editor::open(&mut m, BS, 512).unwrap();
        let before = match &e.recs["f"] {
            Rec::File { body: Body::Blocks(_, map), .. } => map.clone(),
            _ => panic!(),
        };
        e.write("/f", 10, b"changed", 0o644, 0, false).unwrap();
        e.write("/f", 2 * BS, b"appended", 0o644, 0, false).unwrap();
        let after = match &e.recs["f"] {
            Rec::File { body: Body::Blocks(_, map), .. } => map.clone(),
            _ => panic!(),
        };
        assert_ne!(before[0], after[0], "the first block, which the last generation names, is a new one");
        assert_eq!(before[1], after[1], "the second, unchanged, is shared");
        e.commit(false).unwrap();
        let mut e = Editor::open(&mut m, BS, 512).unwrap();
        let mut want = big(2 * BS as usize, 3);
        want[10..17].copy_from_slice(b"changed");
        want.extend_from_slice(b"appended");
        assert_eq!(read_all(&mut e, "/f"), want);
    }

    #[test]
    fn untouched_records_survive_and_a_torn_superblock_falls_back() {
        let mut m = Mem::default();
        let mut e = Editor::open(&mut m, BS, 512).unwrap();
        e.remove("/etc/gone", true).unwrap();
        e.write("/a", 0, b"one", 0o644, 0, true).unwrap();
        e.commit(false).unwrap();
        let g1 = e.generation();
        e.write("/b", 0, b"two", 0o644, 0, true).unwrap();
        e.commit(true).unwrap();
        let g2 = e.generation();
        assert_eq!(g2, g1 + 1);
        drop(e);
        // The newest superblock torn: the generation before is read.
        let at = format::sb_at(g2, BS) as usize;
        m.0[at + 20] ^= 0xff;
        let e = Editor::open(&mut m, BS, 512).unwrap();
        assert_eq!(e.found(), Found::FellBack);
        assert_eq!(e.generation(), g1);
        assert_eq!(e.stat("/etc/gone").unwrap().kind, Kind::Gone);
        assert!(e.stat("/b").is_err());
    }
}
