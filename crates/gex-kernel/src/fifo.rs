//! Named pipes (`mkfifo`, `mknod` with `S_IFIFO`).
//!
//! A FIFO is a name in the filesystem that opens as a pipe: every process
//! that opens it gets an end of the same buffer. The filesystems keep only
//! regular files, so `mknod` makes an empty one there (it lists, stats and
//! unlinks like any file) and records the name here; `stat` then says FIFO
//! and `open` hands out a pipe end instead of the file.
//!
//! Linux blocks a FIFO's `open` until the other side opens. Here an open
//! returns at once and the waiting moves to the first `read` or `write`:
//! a new pipe counts one reader and one writer that stand in for the side
//! not yet there, so a reader that came first waits for data rather than
//! seeing end of file, and a writer that came first fills the buffer rather
//! than getting `EPIPE`. Each stand-in leaves when the first real end of its
//! side opens. `O_WRONLY | O_NONBLOCK` with no reader open is `ENXIO`, as
//! on Linux. Once every end has closed, the next open starts a new pipe,
//! except that bytes a writer left before any reader came wait for the
//! first reader (on Linux that writer would have waited in `open` for it).
//!
//! The table is this kernel's: natively that is every process. In the page,
//! where each process is its own kernel, a FIFO joins only one process's
//! ends.

use crate::errno::{Errno, SysResult};
use crate::pipe::Pipe;
use crate::vfs::{FileHandle, FileType, Metadata};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One FIFO's current pipe, and which stand-ins it still has.
#[derive(Debug, Default)]
struct Node {
    pipe: Option<Arc<Pipe>>,
    stand_in_reader: bool,
    stand_in_writer: bool,
}

/// Every FIFO, by its absolute path.
fn table() -> &'static Mutex<HashMap<String, Node>> {
    static T: std::sync::OnceLock<Mutex<HashMap<String, Node>>> = std::sync::OnceLock::new();
    T.get_or_init(Default::default)
}

/// `path` is a FIFO from now on (its file already made by the caller).
pub fn make(path: &str) {
    table().lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_string(), Node::default());
}

/// Whether `path` names a FIFO.
pub fn is_fifo(path: &str) -> bool {
    table().lock().unwrap_or_else(|e| e.into_inner()).contains_key(path)
}

/// `path` was unlinked: no longer a FIFO. Open ends keep their pipe.
pub fn forget(path: &str) {
    table().lock().unwrap_or_else(|e| e.into_inner()).remove(path);
}

/// `from` was renamed to `to`: the FIFO moves with it, and whatever `to`
/// was is gone.
pub fn rename(from: &str, to: &str) {
    let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
    t.remove(to);
    if let Some(node) = t.remove(from) {
        t.insert(to.to_string(), node);
    }
}

/// `stat` of a FIFO: the file's own, as a FIFO with nothing in it.
pub fn metadata(mut md: Metadata) -> Metadata {
    md.file_type = FileType::Fifo;
    md.size = 0;
    md
}

/// Open the FIFO at `path` for reading, writing or both; `None` when the
/// path is not one.
pub fn open(path: &str, read: bool, write: bool, nonblock: bool) -> Option<Result<Arc<dyn FileHandle>, Errno>> {
    let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
    let node = t.get_mut(path)?;
    // The ends really open, without the stand-ins.
    let real = |node: &Node| {
        let (readers, writers) = node.pipe.as_ref().map_or((0, 0), |p| p.ends());
        (readers.saturating_sub(node.stand_in_reader as usize), writers.saturating_sub(node.stand_in_writer as usize))
    };
    let (readers, writers) = real(node);
    // Every real end gone: a new pipe, as Linux drops a FIFO's buffer when
    // its last end closes. Except for bytes a writer left before any reader
    // came (its reader stand-in still there), which on Linux it would have
    // waited in `open` to hand over: they are the first reader's.
    let unread = node.stand_in_reader && node.pipe.as_ref().is_some_and(|p| p.buffered() > 0);
    if node.pipe.is_none() || (readers + writers == 0 && !unread) {
        node.pipe = Some(Arc::new(Pipe::new()));
        node.stand_in_reader = true;
        node.stand_in_writer = true;
    }
    let pipe = Arc::clone(node.pipe.as_ref().unwrap());
    // No reader there now to take what a non-blocking writer writes.
    if write && !read && nonblock && real(node).0 == 0 {
        return Some(Err(Errno::ENXIO));
    }
    // Each side's first real end takes its stand-in's count; later ones add.
    for (side, want) in [(false, read), (true, write)] {
        if !want {
            continue;
        }
        let stand_in = if side { &mut node.stand_in_writer } else { &mut node.stand_in_reader };
        if *stand_in {
            *stand_in = false;
        } else {
            pipe.open_end(side);
        }
    }
    Some(Ok(Arc::new(FifoEnd { pipe, read, write })))
}

/// An open FIFO: one or both ends of its pipe.
#[derive(Debug)]
pub struct FifoEnd {
    pipe: Arc<Pipe>,
    read: bool,
    write: bool,
}

impl FileHandle for FifoEnd {
    fn poll_readable(&self) -> bool {
        self.read && self.pipe.readable()
    }

    fn available(&self) -> Option<u64> {
        Some(if self.read { self.pipe.buffered() as u64 } else { 0 })
    }

    fn poll_writable(&self) -> bool {
        !self.write || self.pipe.writable()
    }

    fn poll_hup(&self) -> bool {
        self.read && !self.write && self.pipe.hung_up()
    }

    fn write_room(&self) -> Option<u64> {
        self.write.then(|| self.pipe.room() as u64)
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> SysResult {
        if !self.read {
            return Err(Errno::EBADF);
        }
        self.pipe.read(buf)
    }

    fn write_at(&self, _offset: u64, buf: &[u8]) -> SysResult {
        if !self.write {
            return Err(Errno::EBADF);
        }
        self.pipe.write(buf)
    }

    fn metadata(&self) -> Result<Metadata, Errno> {
        Ok(Metadata {
            file_type: FileType::Fifo,
            size: 0,
            mode: 0o600,
            ino: crate::vfs::anon_ino(Arc::as_ptr(&self.pipe) as *const u8),
            mtime: 0,
            nlink: 1,
            rdev: 0,
            times: Default::default(),
        })
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn wait_queue(&self) -> u32 {
        self.pipe.wait_queue()
    }
}

impl Drop for FifoEnd {
    fn drop(&mut self) {
        if self.read {
            self.pipe.close_end(false);
        }
        if self.write {
            self.pipe.close_end(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that opens first waits for data rather than seeing end of
    /// file; the writer's bytes reach it; once the writer closes, it sees
    /// end of file; a non-blocking writer with no reader open is `ENXIO`;
    /// a writer's bytes from before any reader reach the first one, and
    /// nothing outlives every end's closing.
    #[test]
    fn a_fifo_joins_its_openers() {
        let path = "/tmp/fifo-test-joins";
        make(path);
        assert!(is_fifo(path));
        assert!(matches!(open(path, false, true, true), Some(Err(Errno::ENXIO))));
        let r = open(path, true, false, true).unwrap().unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(r.read_at(0, &mut buf), Err(Errno::EAGAIN), "no writer yet: wait, not EOF");
        let w = open(path, false, true, true).unwrap().unwrap();
        assert_eq!(w.write_at(0, b"hi"), Ok(2));
        assert_eq!(r.read_at(0, &mut buf), Ok(2));
        assert_eq!(&buf[..2], b"hi");
        drop(w);
        assert_eq!(r.read_at(0, &mut buf), Ok(0), "the writer is gone: EOF");
        drop(r);
        // A reader came and went: nobody reads now, so ENXIO again.
        assert!(matches!(open(path, false, true, true), Some(Err(Errno::ENXIO))));
        // Bytes a writer left before any reader are the first reader's.
        let w = open(path, false, true, false).unwrap().unwrap();
        assert_eq!(w.write_at(0, b"early"), Ok(5));
        drop(w);
        let r = open(path, true, false, true).unwrap().unwrap();
        assert_eq!(r.read_at(0, &mut buf[..2]), Ok(2));
        drop(r);
        // Every end closed with bytes left unread: the next opener starts
        // clean rather than reading them.
        let r = open(path, true, false, true).unwrap().unwrap();
        assert_eq!(r.read_at(0, &mut buf), Err(Errno::EAGAIN), "no stale bytes");
        drop(r);
        rename(path, "/tmp/fifo-test-moved");
        assert!(!is_fifo(path) && is_fifo("/tmp/fifo-test-moved"));
        forget("/tmp/fifo-test-moved");
        assert!(open("/tmp/fifo-test-moved", true, false, true).is_none());
    }
}
