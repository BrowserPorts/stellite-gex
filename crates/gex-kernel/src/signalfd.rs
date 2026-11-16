//! `signalfd`: a descriptor that reads the process's pending signals.
//!
//! A program blocks the signals it wants and reads them off a descriptor in
//! its event loop instead of taking them in a handler: GLib's
//! `g_unix_signal_add` does, and so Steam's `steam-runtime-launcher-service`,
//! which gave up ("Unable to watch signals") and was restarted forever
//! without one.
//!
//! A read takes the lowest pending signal in the mask, from the reading
//! thread's own set or the process's shared one -- the same claim
//! `rt_sigtimedwait` makes -- and gives a 128-byte `signalfd_siginfo` for it,
//! as many as fit. Nothing pending: `EAGAIN`, or for a blocking descriptor a
//! wait on [`poll_readable`] like a pipe's.
//!
//! [`poll_readable`]: crate::vfs::FileHandle::poll_readable

use crate::errno::{Errno, SysResult};
use crate::signal::Signals;
use crate::vfs::{FileHandle, FileType, Metadata};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

/// The size of one `struct signalfd_siginfo`, on every ABI.
pub const SIGINFO_BYTES: usize = 128;

/// A `signalfd` object: the signals it reads, and the process they are
/// read from (as the thread that made it sees it).
#[derive(Debug)]
pub struct SignalFd {
    mask: AtomicU64,
    signals: Signals,
    queue: crate::wait::Queue,
}

/// Every live signalfd by its handle's address, so `signalfd(fd, ...)` on
/// one already made changes its mask: a `FileHandle` cannot be asked what it
/// is.
fn registry() -> &'static Mutex<HashMap<usize, Weak<SignalFd>>> {
    static R: std::sync::OnceLock<Mutex<HashMap<usize, Weak<SignalFd>>>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

impl SignalFd {
    /// A new one reading `mask` from `signals`'s process, registered for
    /// [`SignalFd::find`].
    pub fn new(signals: Signals, mask: u64) -> Arc<SignalFd> {
        let fd = Arc::new(SignalFd { mask: AtomicU64::new(mask), signals, queue: crate::wait::Queue::new() });
        let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
        r.retain(|_, w| w.strong_count() > 0);
        r.insert(Arc::as_ptr(&fd) as *const () as usize, Arc::downgrade(&fd));
        fd
    }

    /// The signalfd behind `handle`, if it is one.
    pub fn find(handle: &Arc<dyn FileHandle>) -> Option<Arc<SignalFd>> {
        let key = Arc::as_ptr(handle) as *const () as usize;
        registry().lock().unwrap_or_else(|e| e.into_inner()).get(&key).and_then(Weak::upgrade)
    }

    /// `signalfd` on this one again: its new mask.
    pub fn set_mask(&self, mask: u64) {
        self.mask.store(mask, Ordering::Release);
        self.queue.notify();
    }
}

impl FileHandle for SignalFd {
    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> SysResult {
        if buf.len() < SIGINFO_BYTES {
            return Err(Errno::EINVAL);
        }
        let mask = self.mask.load(Ordering::Acquire);
        let mut n = 0;
        while n + SIGINFO_BYTES <= buf.len() {
            let Some((signal, source)) = self.signals.claim_in(mask) else { break };
            let info = &mut buf[n..n + SIGINFO_BYTES];
            info.fill(0);
            // ssi_signo, ssi_errno, ssi_code, ssi_pid, ssi_uid: the sender for
            // one a process sent, SI_KERNEL (0x80) otherwise.
            info[0..4].copy_from_slice(&signal.to_le_bytes());
            match source {
                Some(src) => {
                    info[8..12].copy_from_slice(&(src.code as u32).to_le_bytes());
                    info[12..16].copy_from_slice(&(src.pid as u32).to_le_bytes());
                    info[16..20].copy_from_slice(&src.uid.to_le_bytes());
                }
                None => info[8..12].copy_from_slice(&0x80u32.to_le_bytes()),
            }
            n += SIGINFO_BYTES;
        }
        if n == 0 {
            return Err(Errno::EAGAIN);
        }
        Ok(n as u64)
    }

    fn write_at(&self, _offset: u64, _buf: &[u8]) -> SysResult {
        Err(Errno::EINVAL)
    }

    fn metadata(&self) -> Result<Metadata, Errno> {
        // An anonymous inode, reported as a fifo like eventfd's.
        Ok(Metadata { file_type: FileType::Fifo, size: 0, mode: 0o600, ino: 0, mtime: 0, nlink: 1, rdev: 0, times: Default::default() })
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn poll_readable(&self) -> bool {
        self.signals.pending_raw() & self.mask.load(Ordering::Acquire) != 0
    }

    fn wait_queue(&self) -> u32 {
        self.queue.id()
    }

    fn poll_writable(&self) -> bool {
        false
    }

    fn available(&self) -> Option<u64> {
        Some(if self.poll_readable() { SIGINFO_BYTES as u64 } else { 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGUSR1: u32 = 10;
    const SIGTERM: u32 = 15;
    const fn bit(s: u32) -> u64 {
        1 << (s - 1)
    }

    #[test]
    fn a_read_takes_the_masked_signals_only() {
        let signals = Signals::new();
        let fd = SignalFd::new(signals.share(), bit(SIGTERM));
        assert!(!fd.poll_readable());
        let mut buf = [0u8; 2 * SIGINFO_BYTES];
        assert_eq!(fd.read_at(0, &mut buf), Err(Errno::EAGAIN));
        signals.raise_shared(SIGUSR1, None);
        assert!(!fd.poll_readable(), "SIGUSR1 is not in the mask");
        signals.raise_shared(SIGTERM, None);
        assert!(fd.poll_readable());
        assert_eq!(fd.read_at(0, &mut buf), Ok(SIGINFO_BYTES as u64));
        assert_eq!(u32::from_le_bytes(buf[0..4].try_into().unwrap()), SIGTERM);
        assert!(!fd.poll_readable(), "the read took it");
        assert_ne!(signals.pending_raw() & bit(SIGUSR1), 0, "and left SIGUSR1 alone");
    }

    #[test]
    fn a_new_mask_is_found_by_the_handle() {
        let signals = Signals::new();
        let fd = SignalFd::new(signals.share(), 0);
        let handle: Arc<dyn FileHandle> = fd.clone();
        SignalFd::find(&handle).expect("registered").set_mask(bit(SIGUSR1));
        signals.raise_shared(SIGUSR1, None);
        assert!(fd.poll_readable());
    }

    #[test]
    fn a_short_buffer_is_einval() {
        let fd = SignalFd::new(Signals::new(), bit(SIGTERM));
        assert_eq!(fd.read_at(0, &mut [0u8; 64]), Err(Errno::EINVAL));
    }
}
