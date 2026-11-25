//! The error numbers that simulated system calls fail with.

use std::io;

/// An error number of the host. The error made from one is the error that
/// `std` makes for a system call that fails: it has the number, and the
/// kind and the text that the host gives the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Errno(i32);

impl From<Errno> for io::Error {
    fn from(errno: Errno) -> Self {
        io::Error::from_raw_os_error(errno.0)
    }
}

macro_rules! errnos {
    ($($name:ident)*) => {
        $(pub(crate) const $name: Errno = Errno(libc::$name);)*
    };
}

errnos!(EBADF EBUSY EEXIST EFBIG EINVAL EIO EISDIR ELOOP ENAMETOOLONG);
errnos!(ENOENT ENOSPC ENOTDIR ENOTEMPTY EPERM);
