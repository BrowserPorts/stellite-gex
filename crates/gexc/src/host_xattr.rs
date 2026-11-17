//! The host's `l*xattr`/`f*xattr` calls for gex-kernel's HostFs, which
//! forbids `unsafe` and so takes them as a hook (`set_host_xattr`).

use gex_kernel::errno::Errno;
use gex_kernel::vfs::hostfs::host_xattr::{set_host_xattr, On};
use gex_kernel::vfs::XattrOp;
use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;

pub fn install() {
    set_host_xattr(call);
}

fn call(on: &On<'_>, op: XattrOp<'_>) -> Result<usize, Errno> {
    let path = match on {
        On::Path(p) => Some(CString::new(p.as_os_str().as_bytes()).map_err(|_| Errno::EINVAL)?),
        On::Fd(_) => None,
    };
    let fd = match on { On::Fd(f) => f.as_raw_fd(), On::Path(_) => -1 };
    let name_c = |n: &str| CString::new(n).map_err(|_| Errno::EINVAL);
    // SAFETY: every pointer is a live CString or slice for the call's length.
    let r: isize = unsafe {
        match op {
            XattrOp::Get { name, buf } => {
                let n = name_c(name)?;
                let (b, l) = (buf.as_mut_ptr() as *mut libc::c_void, buf.len());
                sys::get(path.as_deref(), fd, &n, b, l)
            }
            XattrOp::Set { name, value, flags } => {
                let n = name_c(name)?;
                let (v, l) = (value.as_ptr() as *const libc::c_void, value.len());
                sys::set(path.as_deref(), fd, &n, v, l, sys::set_flags(flags)) as isize
            }
            XattrOp::List { buf } => {
                let (b, l) = (buf.as_mut_ptr() as *mut libc::c_char, buf.len());
                sys::list(path.as_deref(), fd, b, l)
            }
            XattrOp::Remove { name } => {
                let n = name_c(name)?;
                sys::remove(path.as_deref(), fd, &n) as isize
            }
        }
    };
    if r < 0 {
        let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO);
        return Err(Errno::from_host(e));
    }
    Ok(r as usize)
}

/// Linux's calls: `l*` on a path, `f*` on a descriptor.
#[cfg(target_os = "linux")]
mod sys {
    use std::ffi::CStr;

    /// Linux's `XATTR_CREATE` and `XATTR_REPLACE` are the guest's.
    pub fn set_flags(flags: u32) -> i32 {
        flags as i32
    }

    pub unsafe fn get(path: Option<&CStr>, fd: i32, n: &CStr, b: *mut libc::c_void, l: usize) -> isize {
        match path {
            Some(p) => libc::lgetxattr(p.as_ptr(), n.as_ptr(), b, l),
            None => libc::fgetxattr(fd, n.as_ptr(), b, l),
        }
    }

    pub unsafe fn set(path: Option<&CStr>, fd: i32, n: &CStr, v: *const libc::c_void, l: usize, flags: i32) -> i32 {
        match path {
            Some(p) => libc::lsetxattr(p.as_ptr(), n.as_ptr(), v, l, flags),
            None => libc::fsetxattr(fd, n.as_ptr(), v, l, flags),
        }
    }

    pub unsafe fn list(path: Option<&CStr>, fd: i32, b: *mut libc::c_char, l: usize) -> isize {
        match path {
            Some(p) => libc::llistxattr(p.as_ptr(), b, l),
            None => libc::flistxattr(fd, b, l),
        }
    }

    pub unsafe fn remove(path: Option<&CStr>, fd: i32, n: &CStr) -> i32 {
        match path {
            Some(p) => libc::lremovexattr(p.as_ptr(), n.as_ptr()),
            None => libc::fremovexattr(fd, n.as_ptr()),
        }
    }
}

/// XNU's calls (sys/xattr.h): one call for a path and one for a
/// descriptor, a resource-fork `position` (0 for every other attribute) and
/// an options word, where `XATTR_NOFOLLOW` is what Linux's `l*` names mean.
#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::CStr;

    /// The guest's `XATTR_CREATE` (1) and `XATTR_REPLACE` (2) as XNU's (2, 4).
    pub fn set_flags(flags: u32) -> i32 {
        let mut o = 0;
        if flags & gex_kernel::vfs::XATTR_CREATE != 0 {
            o |= libc::XATTR_CREATE;
        }
        if flags & gex_kernel::vfs::XATTR_REPLACE != 0 {
            o |= libc::XATTR_REPLACE;
        }
        o
    }

    pub unsafe fn get(path: Option<&CStr>, fd: i32, n: &CStr, b: *mut libc::c_void, l: usize) -> isize {
        match path {
            Some(p) => libc::getxattr(p.as_ptr(), n.as_ptr(), b, l, 0, libc::XATTR_NOFOLLOW),
            None => libc::fgetxattr(fd, n.as_ptr(), b, l, 0, 0),
        }
    }

    pub unsafe fn set(path: Option<&CStr>, fd: i32, n: &CStr, v: *const libc::c_void, l: usize, flags: i32) -> i32 {
        match path {
            Some(p) => libc::setxattr(p.as_ptr(), n.as_ptr(), v, l, 0, flags | libc::XATTR_NOFOLLOW),
            None => libc::fsetxattr(fd, n.as_ptr(), v, l, 0, flags),
        }
    }

    pub unsafe fn list(path: Option<&CStr>, fd: i32, b: *mut libc::c_char, l: usize) -> isize {
        match path {
            Some(p) => libc::listxattr(p.as_ptr(), b, l, libc::XATTR_NOFOLLOW),
            None => libc::flistxattr(fd, b, l, 0),
        }
    }

    pub unsafe fn remove(path: Option<&CStr>, fd: i32, n: &CStr) -> i32 {
        match path {
            Some(p) => libc::removexattr(p.as_ptr(), n.as_ptr(), libc::XATTR_NOFOLLOW),
            None => libc::fremovexattr(fd, n.as_ptr(), 0),
        }
    }
}
