//! System V semaphores: `semget`, `semop`/`semtimedop` and `semctl`.
//!
//! Steam's client guards its shared objects, and the file descriptors its
//! content verifier's threads share, with semaphore sets. Without them each
//! "Thread synchronization object is unuseable", and the verifier's threads
//! seeked and read one descriptor at once, so every staged file failed its
//! checksums with another chunk's bytes.
//!
//! The table is the host process's: every guest process a native host runs
//! shares it, as they share one kernel's. A page has one per Worker, so two
//! processes there do not see each other's sets.
//!
//! A `semop` that cannot proceed does not wait here. The kernel answers
//! [`Op::Blocked`], the caller sleeps briefly with the kernel unlocked and
//! makes the call again, until it proceeds or its timeout passes.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::errno::{Errno, SysResult};
use crate::GuestMem;

const IPC_PRIVATE: i32 = 0;
const IPC_CREAT: u64 = 0o1000;
const IPC_EXCL: u64 = 0o2000;
const IPC_NOWAIT: i16 = 0o4000;
/// `semctl`'s commands. glibc ORs IPC_64 (0x100) into them.
const IPC_RMID: u64 = 0;
const IPC_SET: u64 = 1;
const IPC_STAT: u64 = 2;
const GETPID: u64 = 11;
const GETVAL: u64 = 12;
const GETALL: u64 = 13;
const GETNCNT: u64 = 14;
const GETZCNT: u64 = 15;
const SETVAL: u64 = 16;
const SETALL: u64 = 17;
/// Linux's limits: values, and operations in one call.
const SEMVMX: i32 = 32767;
const SEMOPM: u64 = 500;
const SEMMSL: i32 = 32000;

struct Set {
    key: i32,
    mode: u32,
    vals: Vec<i32>,
    /// The pid of the last process to change each value (`GETPID`).
    pids: Vec<i32>,
}

#[derive(Default)]
struct Table {
    sets: HashMap<i32, Set>,
    last: i32,
    /// When each blocked caller's `semtimedop` gives up, by its address
    /// space and its thread's context: the call is made again after every
    /// short wait, and must not start its timeout over each time. With the
    /// call's own arguments (set, operations, timeout), so a deadline left
    /// by a call that ended some other way ([`forget`] missed, a thread
    /// that died waiting, an address space reused) is not taken for a new
    /// call's.
    deadlines: HashMap<(usize, u64), ((i32, u64, u64), Instant)>,
}

fn table() -> MutexGuard<'static, Table> {
    static TABLE: OnceLock<Mutex<Table>> = OnceLock::new();
    TABLE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// `semget(key, nsems, semflg)`.
pub fn semget(key: i32, nsems: i32, flags: u64) -> SysResult {
    let mut t = table();
    if key != IPC_PRIVATE {
        if let Some((&id, set)) = t.sets.iter().find(|(_, s)| s.key == key) {
            if flags & IPC_CREAT != 0 && flags & IPC_EXCL != 0 {
                return Err(Errno::EEXIST);
            }
            if nsems > set.vals.len() as i32 {
                return Err(Errno::EINVAL);
            }
            return Ok(id as u64);
        }
        if flags & IPC_CREAT == 0 {
            return Err(Errno::ENOENT);
        }
    }
    if nsems <= 0 || nsems > SEMMSL {
        return Err(Errno::EINVAL);
    }
    t.last += 1;
    let id = t.last;
    let n = nsems as usize;
    t.sets.insert(id, Set { key, mode: (flags & 0o777) as u32, vals: vec![0; n], pids: vec![0; n] });
    Ok(id as u64)
}

/// What a `semop` came to.
pub enum Op {
    Done,
    /// Some operation must wait: the caller sleeps and makes the call again.
    Blocked,
}

/// `semtimedop(semid, sops, nsops, timeout)`, the `timeout` a 64-bit
/// timespec or 0 for none; `who` names the calling thread (its address
/// space and context) for the timeout, `pid` is recorded for `GETPID`.
pub fn semop(m: &GuestMem<'_>, who: (usize, u64), pid: i32, id: i32, sops: u64, nsops: u64, timeout: u64) -> Result<Op, Errno> {
    if nsops == 0 {
        return Err(Errno::EINVAL);
    }
    if nsops > SEMOPM {
        return Err(Errno::E2BIG);
    }
    // struct sembuf { unsigned short sem_num; short sem_op; short sem_flg; }
    let mut ops = Vec::with_capacity(nsops as usize);
    for i in 0..nsops {
        let b = m.read(sops + i * 6, 6)?;
        ops.push((u16::from_le_bytes([b[0], b[1]]) as usize, i16::from_le_bytes([b[2], b[3]]), i16::from_le_bytes([b[4], b[5]])));
    }
    let mut t = table();
    let set = t.sets.get_mut(&id).ok_or(Errno::EINVAL)?;
    if ops.iter().any(|&(num, _, _)| num >= set.vals.len()) {
        return Err(Errno::EFBIG);
    }
    // All or nothing: apply to a copy, and keep it only if every one went.
    let mut vals = set.vals.clone();
    let mut wait = None;
    for &(num, op, flg) in &ops {
        let v = &mut vals[num];
        if op > 0 {
            if *v + op as i32 > SEMVMX {
                return Err(Errno::ERANGE);
            }
            *v += op as i32;
        } else if op < 0 {
            if *v < -(op as i32) {
                wait = Some(flg);
                break;
            }
            *v += op as i32;
        } else if *v != 0 {
            wait = Some(flg);
            break;
        }
    }
    match wait {
        None => {
            for &(num, _, _) in &ops {
                set.pids[num] = pid;
            }
            set.vals = vals;
            t.deadlines.remove(&who);
            Ok(Op::Done)
        }
        Some(flg) if flg & IPC_NOWAIT != 0 => {
            t.deadlines.remove(&who);
            Err(Errno::EAGAIN)
        }
        Some(_) if timeout != 0 => {
            let now = Instant::now();
            let call = (id, sops, timeout);
            let ends = match t.deadlines.get(&who) {
                Some(&(c, d)) if c == call => d,
                _ => {
                    let (secs, nanos) = (m.read_u64(timeout)?, m.read_u64(timeout + 8)?);
                    if nanos >= 1_000_000_000 {
                        return Err(Errno::EINVAL);
                    }
                    let d = now + Duration::new(secs, nanos as u32);
                    t.deadlines.insert(who, (call, d));
                    d
                }
            };
            if now >= ends {
                t.deadlines.remove(&who);
                return Err(Errno::EAGAIN);
            }
            Ok(Op::Blocked)
        }
        Some(_) => Ok(Op::Blocked),
    }
}

/// The calling thread's blocked `semtimedop` ended without proceeding (a
/// signal's `EINTR`): its deadline is no longer anyone's.
pub fn forget(who: (usize, u64)) {
    table().deadlines.remove(&who);
}

/// `semctl(semid, semnum, cmd, arg)`: `arg` is the union's value (an int
/// for SETVAL, otherwise a pointer); `compat` for the 32-bit `semid64_ds`.
pub fn semctl(m: &mut GuestMem<'_>, id: i32, num: u64, cmd: u64, arg: u64, compat: bool) -> SysResult {
    let cmd = cmd & !0x100;
    let mut t = table();
    let set = t.sets.get_mut(&id).ok_or(Errno::EINVAL)?;
    let n = set.vals.len();
    let one = |num: u64| if (num as usize) < n { Ok(num as usize) } else { Err(Errno::EINVAL) };
    match cmd {
        IPC_RMID => {
            t.sets.remove(&id);
            Ok(0)
        }
        IPC_SET => Ok(0),
        IPC_STAT => {
            // semid64_ds: its ipc64_perm (key first, mode at 20), then the
            // times, then sem_nsems. i386's has each time's high half beside
            // it (nsems at 52 of 64 bytes); x86-64's an unused word after
            // each time (80 of 104); asm-generic's for 64-bit, aarch64's,
            // the two times alone (64 of 88).
            let (size, nsems_at) = if compat {
                (64u64, 52u64)
            } else if gex_abi::X86 {
                (104, 80)
            } else {
                (88, 64)
            };
            m.write(arg, &vec![0u8; size as usize])?;
            m.write_u32(arg, set.key as u32)?;
            m.write(arg + 20, &(set.mode as u16).to_le_bytes())?;
            if compat {
                m.write_u32(arg + nsems_at, n as u32)?;
            } else {
                m.write_u64(arg + nsems_at, n as u64)?;
            }
            Ok(0)
        }
        GETVAL => Ok(set.vals[one(num)?] as u64),
        GETPID => Ok(set.pids[one(num)?] as u64),
        GETNCNT | GETZCNT => {
            one(num)?;
            Ok(0)
        }
        SETVAL => {
            let v = arg as u32 as i32;
            if !(0..=SEMVMX).contains(&v) {
                return Err(Errno::ERANGE);
            }
            let i = one(num)?;
            set.vals[i] = v;
            Ok(0)
        }
        GETALL => {
            let b: Vec<u8> = set.vals.iter().flat_map(|&v| (v as u16).to_le_bytes()).collect();
            m.write(arg, &b)?;
            Ok(0)
        }
        SETALL => {
            let b = m.read(arg, n * 2)?.to_vec();
            for (i, v) in set.vals.iter_mut().enumerate() {
                *v = u16::from_le_bytes([b[i * 2], b[i * 2 + 1]]) as i32;
            }
            Ok(0)
        }
        _ => Err(Errno::EINVAL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `IPC_STAT` puts `sem_nsems` where the guest's `semid64_ds` has it.
    #[test]
    fn ipc_stat_says_how_many() {
        let mut buf = vec![0u8; 0x1000];
        let mut m = GuestMem::new(&mut buf);
        let id = semget(IPC_PRIVATE, 3, IPC_CREAT | 0o600).unwrap() as i32;
        assert_eq!(semctl(&mut m, id, 0, IPC_STAT, 0x100, false), Ok(0));
        let at = if gex_abi::X86 { 80 } else { 64 };
        assert_eq!(m.read_u64(0x100 + at).unwrap(), 3);
        assert_eq!(semctl(&mut m, id, 0, IPC_STAT, 0x200, true), Ok(0));
        assert_eq!(m.read_u32(0x200 + 52).unwrap(), 3);
        semctl(&mut m, id, 0, IPC_RMID, 0, false).unwrap();
    }

    /// A blocked `semtimedop` keeps its deadline across its retries, and a
    /// new call -- after a signal ended the last one, or with other
    /// arguments -- starts its own rather than inheriting one long past.
    #[test]
    fn a_deadline_is_one_calls() {
        let mut buf = vec![0u8; 0x1000];
        let mut m = GuestMem::new(&mut buf);
        let id = semget(IPC_PRIVATE, 1, IPC_CREAT | 0o600).unwrap() as i32;
        // One `sem_op = -1` at 0x10 (the value is 0, so it waits), and a
        // timeout of 0.05 s at 0x20.
        m.write(0x10, &[0, 0, 0xff, 0xff, 0, 0]).unwrap();
        m.write_u64(0x20, 0).unwrap();
        m.write_u64(0x28, 50_000_000).unwrap();
        let who = (0x1234usize, 0x40u64);
        assert!(matches!(semop(&m, who, 1, id, 0x10, 1, 0x20), Ok(Op::Blocked)));
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(semop(&m, who, 1, id, 0x10, 1, 0x20).err(), Some(Errno::EAGAIN), "the retry keeps the first deadline");
        // A signal ends the next call; the one after must wait afresh.
        assert!(matches!(semop(&m, who, 1, id, 0x10, 1, 0x20), Ok(Op::Blocked)));
        std::thread::sleep(Duration::from_millis(60));
        forget(who);
        assert!(matches!(semop(&m, who, 1, id, 0x10, 1, 0x20), Ok(Op::Blocked)), "a new call, a new deadline");
        // A stale deadline under other arguments is not this call's.
        std::thread::sleep(Duration::from_millis(60));
        m.write_u64(0x38, 50_000_000).unwrap();
        m.write_u64(0x30, 0).unwrap();
        assert!(matches!(semop(&m, who, 1, id, 0x10, 1, 0x30), Ok(Op::Blocked)));
        forget(who);
        semctl(&mut m, id, 0, IPC_RMID, 0, false).unwrap();
    }
}
