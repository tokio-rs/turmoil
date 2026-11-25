//! The cases of the filesystem conformance suite.
//!
//! Each arm declares this file as a module of its own, below the filesystem
//! functions it brings into scope, so that the same code runs against the
//! shim and against `std::fs`. Every case reports a line, and the lines of
//! the two arms have to be the same.

// An arm may not need every import, and the cases open files in every way
// there is, the ways that are usually a mistake included.
#![allow(unused_imports, clippy::suspicious_open_options)]

use super::*;
use std::ffi::OsString;
use std::fmt::Debug;
use std::io::SeekFrom::{Current, End, Start};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, DirEntryExt, FileExt, FileTypeExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Puts a path of the cases under the root of the arm.
type At<'a> = &'a dyn Fn(&str) -> String;
type Options = fn(&mut OpenOptions) -> &mut OpenOptions;

/// What an operation came to: its value, or the kind of its error and
/// the number the system gave it, if it came from there.
fn show<T: Debug>(result: io::Result<T>) -> String {
    let error = match result {
        Ok(value) => return format!("{value:?}"),
        Err(error) => error,
    };
    match error.raw_os_error() {
        Some(errno) => format!("{:?}({errno})", error.kind()),
        None => format!("{:?}", error.kind()),
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\0', "_")
}

fn open(path: impl AsRef<Path>, options: Options) -> io::Result<File> {
    options(&mut OpenOptions::new()).open(path)
}

/// Opens `path` to write, and to do what else `options` add.
fn writer(path: impl AsRef<Path>, options: Options) -> io::Result<File> {
    options(OpenOptions::new().write(true)).open(path)
}

fn opens(path: impl AsRef<Path>, options: Options) -> io::Result<()> {
    writer(path, options).map(drop)
}

/// Opens `path` with flags that `std::fs` has no option for, and writes
/// to it if it opened for that.
fn flagged(path: &str, options: Options, flags: i32) -> io::Result<()> {
    let mut file = options(OpenOptions::new().custom_flags(flags)).open(path)?;
    match file.write(b"ab") {
        Err(unwritable) if unwritable.raw_os_error() == Some(libc::EBADF) => Ok(()),
        written => written.map(drop),
    }
}

/// A name of `len` bytes.
fn long(len: usize) -> String {
    "x".repeat(len)
}

fn truncated(path: String) -> io::Result<()> {
    opens(path, |o| o.truncate(true))
}

fn resized(path: String, len: u64) -> io::Result<()> {
    writer(path, |o| o)?.set_len(len)
}

fn rewritten(path: String, bytes: &[u8]) -> io::Result<()> {
    writer(path, |o| o)?.write_all(bytes)
}

fn appended(path: String, bytes: &[u8]) -> io::Result<()> {
    writer(path, |o| o.append(true))?.write_all(bytes)
}

fn copied(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    copy(from, to).map(drop)
}

fn chmod(path: impl AsRef<Path>, mode: u32) -> io::Result<()> {
    set_permissions(path, Permissions::from_mode(mode))
}

/// Sets the mode of the file at `path` through a handle on it.
fn fchmod(path: String, mode: u32) -> io::Result<()> {
    File::open(path)?.set_permissions(Permissions::from_mode(mode))
}

/// A time to set a file to, `secs` after the epoch and a little.
fn early(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::new(secs, 7)
}

/// Sets the times of the file at `path` through a handle on it.
fn retimed(path: String, times: FileTimes) -> io::Result<()> {
    File::open(path)?.set_times(times)
}

/// When `file` was last read and last written: as times since the epoch,
/// and as the seconds and nanoseconds of each.
fn stamped(file: &File) -> io::Result<String> {
    let meta = file.metadata()?;
    let since = |time: SystemTime| time.duration_since(UNIX_EPOCH).unwrap();
    let read = (since(meta.accessed()?), meta.atime(), meta.atime_nsec());
    let written = (since(meta.modified()?), meta.mtime(), meta.mtime_nsec());
    Ok(format!("{read:?} {written:?}"))
}

/// The mode of `file`, with the bits that tell what kind of file it is.
fn whole_mode(file: &File) -> io::Result<String> {
    let meta = file.metadata()?;
    Ok(format!("{:o} {:o}", meta.permissions().mode(), meta.mode()))
}

fn mode(path: String) -> io::Result<u32> {
    Ok(metadata(path)?.permissions().mode() & 0o7777)
}

/// The names in the directory at `path`, in order.
fn names(path: String) -> io::Result<Vec<String>> {
    let name = |entry: io::Result<DirEntry>| entry.unwrap().file_name();
    let text = |name: OsString| name.into_string().unwrap();
    let mut names: Vec<_> = read_dir(path)?.map(name).collect();
    names.sort();
    Ok(names.into_iter().map(text).collect())
}

/// Describes everything under `dir`, one line per entry.
fn describe(root: &str, dir: &str, out: &mut Vec<String>) {
    let names = match names(format!("{root}{dir}")) {
        Ok(names) => names,
        Err(error) => return out.push(format!("{dir}: {:?}", error.kind())),
    };
    for name in names {
        let path = format!("{dir}/{name}");
        let full = format!("{root}{path}");
        let meta = symlink_metadata(&full).unwrap();
        let attrs = format!("{:o} x{}", meta.permissions().mode() & 0o7777, meta.nlink());
        if meta.is_dir() {
            out.push(format!("{path}/ {attrs}"));
            describe(root, &path, out);
        } else if meta.is_symlink() {
            let target = read_link(&full).unwrap();
            let target = target.to_str().unwrap();
            let sized = meta.len() == target.len() as u64;
            let target = target.strip_prefix(root).unwrap_or(target);
            out.push(format!("{path} -> {target} {attrs} sized:{sized}"));
        } else {
            out.push(format!("{path} = {} {attrs}", text(&read(&full).unwrap())));
        }
    }
}

/// What `after` has that `before` lacks, and the other way around.
fn changes(before: &[String], after: &[String]) -> String {
    let gone: Vec<_> = before.iter().filter(|line| !after.contains(line)).collect();
    let new: Vec<_> = after.iter().filter(|line| !before.contains(line)).collect();
    match gone.is_empty() && new.is_empty() {
        true => String::new(),
        false => format!("  -{gone:?} +{new:?}"),
    }
}

/// Renames. Linux lets a filesystem report a directory that is not empty,
/// and so cannot be replaced, as one that exists. xfs does.
fn renamed(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    rename(from, to).map_err(|error| match error.kind() {
        ErrorKind::AlreadyExists => io::Error::from_raw_os_error(libc::ENOTEMPTY),
        _ => error,
    })
}

type PathOp = fn(&str, At) -> io::Result<()>;

const PATH_OPS: &[(&str, PathOp)] = &[
    ("create_dir", |t, _| create_dir(t)),
    ("create_dir_all", |t, _| create_dir_all(t)),
    ("symlink(other, T)", |t, p| symlink(p("/t/other"), t)),
    ("hard_link(other, T)", |t, p| hard_link(p("/t/other"), t)),
    ("hard_link(T, linked)", |t, p| hard_link(t, p("/t/linked"))),
    ("hard_link(T, T)", |t, _| hard_link(t, t)),
    ("open read", |t, _| File::open(t).map(drop)),
    ("open write", |t, _| opens(t, |o| o)),
    ("open append", |t, _| opens(t, |o| o.append(true))),
    ("open truncate", |t, _| opens(t, |o| o.truncate(true))),
    ("open create", |t, _| opens(t, |o| o.create(true))),
    ("open create_new", |t, _| opens(t, |o| o.create_new(true))),
    ("rename(other, T)", |t, p| renamed(p("/t/other"), t)),
    ("rename(spare, T)", |t, p| renamed(p("/t/spare"), t)),
    ("rename(missing, T)", |t, p| renamed(p("/t/missing"), t)),
    ("rename(T, moved)", |t, p| renamed(t, p("/t/moved"))),
    ("rename(T, spare/in)", |t, p| renamed(t, p("/t/spare/in"))),
    ("rename(T, gone/in)", |t, p| renamed(t, p("/t/gone/in"))),
    ("rename(T, T)", |t, _| renamed(t, t)),
    ("rename(T, T/in)", |t, _| renamed(t, format!("{t}/in"))),
    ("remove_file", |t, _| remove_file(t)),
    ("remove_dir", |t, _| remove_dir(t)),
    ("remove_dir_all", |t, _| remove_dir_all(t)),
    ("metadata", |t, _| metadata(t).map(drop)),
    ("symlink_metadata", |t, _| symlink_metadata(t).map(drop)),
    ("read", |t, _| read(t).map(drop)),
    ("read_to_string", |t, _| read_to_string(t).map(drop)),
    ("read_dir", |t, _| read_dir(t).map(drop)),
    ("read_link", |t, _| read_link(t).map(drop)),
    ("canonicalize", |t, _| canonicalize(t).map(drop)),
    ("write", |t, _| write(t, b"z")),
    ("copy(T, copied)", |t, p| copied(t, p("/t/copied"))),
    ("copy(other, T)", |t, p| copied(p("/t/other"), t)),
    ("copy(T, T)", |t, _| copied(t, t)),
    ("set_permissions", |t, _| chmod(t, 0o700)),
    ("open+sync_all", |t, _| File::open(t)?.sync_all()),
    ("rename(T, long)", |t, p| renamed(t, p("/t/") + &long(256))),
    ("rename(T, too long)", |t, p| {
        renamed(t, p("/t/") + &long(5000))
    }),
    ("rename(T, nothing)", |t, _| renamed(t, "")),
    ("rename(T, nul)", |t, p| renamed(t, p("/t/nu\0l"))),
    ("hard_link(T, long)", |t, p| {
        hard_link(t, p("/t/") + &long(256))
    }),
    ("hard_link(T, too long)", |t, p| {
        hard_link(t, p("/t/") + &long(5000))
    }),
    ("hard_link(T, nul)", |t, p| hard_link(t, p("/t/nu\0l"))),
    ("symlink(T, nul)", |t, p| symlink(t, p("/t/nu\0l"))),
    ("copy(T, nul)", |t, p| copied(t, p("/t/nu\0l"))),
    ("open O_DIRECTORY", |t, _| {
        flagged(t, |o| o.read(true), libc::O_DIRECTORY)
    }),
    ("open O_NOFOLLOW", |t, _| {
        flagged(t, |o| o.read(true), libc::O_NOFOLLOW)
    }),
    ("open O_NOFOLLOW|O_DIRECTORY", |t, _| {
        flagged(t, |o| o.read(true), libc::O_NOFOLLOW | libc::O_DIRECTORY)
    }),
    ("open read O_TRUNC", |t, _| {
        flagged(t, |o| o.read(true), libc::O_TRUNC)
    }),
    ("open read O_APPEND", |t, _| {
        flagged(t, |o| o.read(true), libc::O_APPEND)
    }),
    ("open write O_APPEND", |t, _| {
        flagged(t, |o| o.write(true), libc::O_APPEND)
    }),
    ("open write O_CREAT", |t, _| {
        flagged(t, |o| o.write(true), libc::O_CREAT)
    }),
    ("open create O_EXCL", |t, _| {
        flagged(t, |o| o.write(true).create(true), libc::O_EXCL)
    }),
    ("open create O_DIRECTORY", |t, _| {
        flagged(t, |o| o.write(true).create(true), libc::O_DIRECTORY)
    }),
    ("open create O_NOFOLLOW", |t, _| {
        flagged(t, |o| o.write(true).create(true), libc::O_NOFOLLOW)
    }),
    ("open O_DIRECT", |t, _| {
        flagged(t, |o| o.read(true), libc::O_DIRECT)
    }),
    ("open write O_SYNC", |t, _| {
        flagged(t, |o| o.write(true), libc::O_SYNC)
    }),
    ("open write O_DSYNC", |t, _| {
        flagged(t, |o| o.write(true), libc::O_DSYNC)
    }),
];

/// Every kind of path there is to give an operation. `<long>` stands for
/// a name longer than a directory holds.
const TARGETS: &str = "/file /dir /full /missing /to_dir /to_file /to_full /dangling /loop \
    /file/ /dir/ /full/ /missing/ /to_dir/ /to_file/ /to_full/ /dangling/ /loop/ \
    /dir/. /full/. /file/. /missing/. /full/sub/.. /file/.. /missing/.. /to_dir/. /to_full/.. \
    /dir/./ /full/sub/../ /dir// //dir /full/sub/../inner /to_full/inner /to_full/sub/ \
    /file/x /file/x/ /missing/x /missing/x/ /dangling/x /to_file/x /loop/x \
    /<long> /<long>/ /<long>/x /dir/<long> /file/<long> /missing/<long>";

/// Every operation on a path, with every kind of path.
fn paths(root: &str, out: &mut Vec<String>) {
    let p = |rest: &str| format!("{root}{rest}");
    let reset = || {
        let _ = remove_dir_all(p("/t"));
        create_dir_all(p("/t/full/sub")).unwrap();
        create_dir(p("/t/dir")).unwrap();
        create_dir(p("/t/spare")).unwrap();
        write(p("/t/full/inner"), b"i").unwrap();
        write(p("/t/file"), b"x").unwrap();
        write(p("/t/other"), b"y").unwrap();
        symlink(p("/t/dir"), p("/t/to_dir")).unwrap();
        symlink(p("/t/file"), p("/t/to_file")).unwrap();
        symlink(p("/t/full"), p("/t/to_full")).unwrap();
        symlink(p("/t/nowhere"), p("/t/dangling")).unwrap();
        symlink(p("/t/loop"), p("/t/loop")).unwrap();
    };
    reset();
    let mut pristine = Vec::new();
    describe(root, "/t", &mut pristine);
    out.extend(pristine.iter().map(|line| format!("pristine {line}")));
    for target in TARGETS.split_whitespace() {
        let path = p(&format!("/t{target}")).replace("<long>", &long(256));
        let exists = show(exists(&path));
        out.push(format!("{target:20} {:24} {exists}", "exists"));
        for (name, op) in PATH_OPS {
            reset();
            let result = show(op(&path, &p));
            let mut after = Vec::new();
            describe(root, "/t", &mut after);
            let changes = changes(&pristine, &after);
            out.push(format!("{target:20} {name:24} {result}{changes}"));
        }
    }
    remove_dir_all(p("/t")).unwrap();
}

const FLAGS: &str = "read write append truncate create create_new";

/// Every combination of open options, on a file, a missing name and a
/// directory.
fn options(root: &str, out: &mut Vec<String>) {
    let p = |rest: &str| format!("{root}{rest}");
    for bits in 0..1 << FLAGS.split(' ').count() {
        let on = |flag: usize| bits & (1 << flag) != 0;
        let mut options = OpenOptions::new();
        options.read(on(0)).write(on(1)).append(on(2));
        options.truncate(on(3)).create(on(4)).create_new(on(5));
        let set = FLAGS.split(' ').enumerate().filter(|(flag, _)| on(*flag));
        let set: Vec<_> = set.map(|(_, name)| name).collect();
        for target in ["/o/file", "/o/missing", "/o/dir"] {
            let _ = remove_dir_all(p("/o"));
            create_dir_all(p("/o/dir")).unwrap();
            write(p("/o/file"), b"0123456789").unwrap();
            let mut opened = options.open(p(target));
            let wrote = opened.as_mut().ok().map(|file| show(file.write(b"ab")));
            let result = show(opened.map(drop));
            let mut after = Vec::new();
            describe(root, "/o", &mut after);
            let found = format!("{result} write:{wrote:?} {after:?}");
            out.push(format!("open {target} {set:?}: {found}"));
        }
    }
    remove_dir_all(p("/o")).unwrap();
}

/// Reads up to four bytes at the cursor.
fn read4(file: &mut File) -> io::Result<String> {
    let mut buf = [0; 4];
    file.read(&mut buf).map(|n| text(&buf[..n]))
}

fn read4_at(file: &File, offset: u64) -> io::Result<String> {
    let mut buf = [0; 4];
    file.read_at(&mut buf, offset).map(|n| text(&buf[..n]))
}

fn read_all(file: &mut File) -> io::Result<String> {
    let mut all = Vec::new();
    file.read_to_end(&mut all).map(|_| text(&all))
}

fn put(file: &mut File) -> io::Result<usize> {
    file.write(b"ab")
}

fn put_at(file: &File, offset: u64) -> io::Result<usize> {
    file.write_at(b"ab", offset)
}

/// Reads up to four bytes at the cursor, through a shared reference.
fn shared_read4(mut file: &File) -> io::Result<String> {
    let mut buf = [0; 4];
    file.read(&mut buf).map(|n| text(&buf[..n]))
}

fn shared_put(mut file: &File) -> io::Result<usize> {
    file.write(b"ab")
}

fn shared_seek(mut file: &File, to: SeekFrom) -> io::Result<u64> {
    file.seek(to)
}

/// What `op` comes to once the cursor is moved to `to`.
fn at<T>(file: &mut File, to: SeekFrom, op: fn(&mut File) -> io::Result<T>) -> io::Result<T> {
    file.seek(to)?;
    op(file)
}

/// The cursor of a clone of `file`, after `op` on the clone.
fn cloned<T>(file: &File, op: fn(&mut File) -> io::Result<T>) -> io::Result<u64> {
    let mut clone = file.try_clone()?;
    op(&mut clone)?;
    clone.stream_position()
}

/// Whether the handle is on a directory, and how many links that has.
fn kind(file: &File) -> io::Result<(bool, u64)> {
    let meta = file.metadata()?;
    Ok((meta.is_dir(), meta.nlink()))
}

/// Whether there is anything at `path`.
fn found(path: String) -> String {
    show(metadata(path).map(drop))
}

type HandleOp = fn(&mut File) -> String;

/// The last offset there is: offsets are signed numbers. Seeking there is
/// left out, since how far a file may be sought depends on the filesystem.
const LAST: u64 = i64::MAX as u64;

const HANDLE_OPS: &[(&str, HandleOp)] = &[
    ("read", |f| show(read4(f))),
    ("read_at", |f| show(read4_at(f, 2))),
    ("read_to_end", |f| show(read_all(f))),
    ("write", |f| show(put(f))),
    ("write_at", |f| show(f.write_at(b"ab", 2))),
    ("write_at past the end", |f| show(f.write_at(b"ab", 12))),
    ("seek+write", |f| show(at(f, Start(3), put))),
    ("seek+read", |f| show(at(f, End(-3), read4))),
    ("seek past+write", |f| show(at(f, Start(12), put))),
    ("seek past+read", |f| show(at(f, End(5), read4))),
    ("seek before the start", |f| show(f.seek(Current(-1)))),
    ("seek before it from the end", |f| show(f.seek(End(-11)))),
    ("seek too far", |f| show(f.seek(Start(u64::MAX)))),
    ("set_len shorter", |f| show(f.set_len(4))),
    ("set_len longer", |f| show(f.set_len(12))),
    ("set_len too long", |f| show(f.set_len(u64::MAX))),
    ("read+set_len+read", |f| {
        let (first, cut) = (show(read4(f)), show(f.set_len(2)));
        let (at, rest) = (show(f.stream_position()), show(read4(f)));
        format!("{first} {cut} at {at} {rest}")
    }),
    ("sync_all", |f| show(f.sync_all())),
    ("sync_data", |f| show(f.sync_data())),
    ("flush", |f| show(f.flush())),
    ("metadata", |f| show(f.metadata().map(|meta| meta.len()))),
    ("read_at too far", |f| show(read4_at(f, u64::MAX))),
    ("write_at too far", |f| show(f.write_at(b"ab", u64::MAX))),
    ("empty read", |f| show(f.read(&mut []))),
    ("empty write", |f| show(f.write(b""))),
    ("clone shares the cursor", |f| {
        let clone = cloned(f, |clone| clone.read_exact(&mut [0; 2]));
        format!("{} {}", show(clone), show(f.stream_position()))
    }),
    ("clone follows a seek", |f| {
        let seek = f.seek(Start(5));
        format!("{} {}", show(seek), show(cloned(f, |_| Ok(()))))
    }),
    ("clone writes", |f| {
        let clone = cloned(f, put);
        format!("{} {}", show(clone), show(f.write(b"cd")))
    }),
    ("read_at up to LAST", |f| show(read4_at(f, LAST - 4))),
    ("read_at across LAST", |f| show(read4_at(f, LAST - 3))),
    ("write_at at LAST", |f| show(f.write_at(b"a", LAST))),
    ("write_at across LAST", |f| show(put_at(f, LAST - 1))),
    ("read by reference", |f| show(shared_read4(f))),
    ("write by reference", |f| show(shared_put(f))),
    ("seek by reference", |f| show(shared_seek(f, End(-1)))),
    ("set_permissions", |f| {
        let set = f.set_permissions(Permissions::from_mode(0o600));
        format!("{} {}", show(set), show(whole_mode(f)))
    }),
    ("set_times", |f| {
        let times = FileTimes::new().set_accessed(early(2));
        let set = f.set_times(times.set_modified(early(3)));
        format!("{} {}", show(set), show(stamped(f)))
    }),
    ("set_times, then set_modified", |f| {
        let times = FileTimes::new().set_accessed(early(2));
        let set = f.set_times(times.set_modified(early(3)));
        let again = f.set_modified(early(4));
        format!("{} {} {}", show(set), show(again), show(stamped(f)))
    }),
];

const MODES: &[(&str, Options)] = &[
    ("r", |o| o.read(true)),
    ("w", |o| o.write(true)),
    ("rw", |o| o.read(true).write(true)),
    ("a", |o| o.append(true)),
    ("ra", |o| o.read(true).append(true)),
];

/// Every operation on a handle, for every way a file can be open.
fn handles(root: &str, out: &mut Vec<String>) {
    let path = format!("{root}/handle");
    for (mode, options) in MODES {
        for (name, op) in HANDLE_OPS {
            write(&path, b"0123456789").unwrap();
            let mut file = open(&path, *options).unwrap();
            let result = op(&mut file);
            let at = show(file.stream_position());
            let content = text(&read(&path).unwrap());
            out.push(format!("{mode:2} {name:28} {result} | at {at} | {content}"));
        }
    }
    remove_file(&path).unwrap();
}

/// What a handle on a directory does.
fn directories(root: &str, out: &mut Vec<String>) {
    let path = format!("{root}/handled");
    create_dir(&path).unwrap();
    write(format!("{path}/file"), b"x").unwrap();
    let mut dir = File::open(&path).unwrap();
    let mut note = |what: &str, value: String| out.push(format!("a directory's {what}: {value}"));
    note("kind", show(kind(&dir)));
    note("read", show(read4(&mut dir)));
    note("read_at", show(read4_at(&dir, 0)));
    note("read_at across LAST", show(read4_at(&dir, LAST - 3)));
    note("empty read", show(dir.read(&mut [])));
    note("write", show(put(&mut dir)));
    note("write_at", show(dir.write_at(b"ab", 0)));
    note("set_len", show(dir.set_len(0)));
    note("seek", show(dir.seek(Start(3))));
    note("sync_all", show(dir.sync_all()));
    note("sync_data", show(dir.sync_data()));
    note("clone", show(cloned(&dir, |_| Ok(()))));
    remove_dir_all(&path).unwrap();
    note("kind once removed", show(kind(&dir)));
    note("sync once removed", show(dir.sync_all()));
}

/// What the metadata says, and what moves it.
fn attributes(root: &str, out: &mut Vec<String>) {
    let m = |rest: &str| format!("{root}/m/{rest}");
    let mut note = |what: &str, value: String| out.push(format!("{what}: {value}"));
    let meta = |path: &str| symlink_metadata(m(path));
    let links = |path: &str| show(meta(path).map(|meta| meta.nlink()));
    let ino = |path: &str| meta(path).unwrap().ino();
    let len = |path: &str| show(meta(path).map(|meta| meta.len()));
    let content = |path: &str| show(read(m(path)).map(|bytes| text(&bytes)));
    let mode = |path: &str| show(mode(m(path)));

    create_dir_all(m("dir/sub")).unwrap();
    create_dir(m("dir/more")).unwrap();
    write(m("dir/file"), b"abc").unwrap();
    note("links of a full directory", links("dir"));
    note("links of an empty directory", links("dir/sub"));
    note("links of a file", links("dir/file"));
    hard_link(m("dir/file"), m("twin")).unwrap();
    note("links of a file with a twin", links("twin"));
    let file = ino("dir/file");
    let shared = file == ino("twin");
    note("a twin is the same file", shared.to_string());
    rename(m("dir/file"), m("dir/moved")).unwrap();
    let kept = file == ino("dir/moved");
    note("a rename keeps the file", kept.to_string());
    remove_file(m("twin")).unwrap();
    note("links of a file without its twin", links("dir/moved"));
    rename(m("dir/more"), m("more")).unwrap();
    note("links of a directory that lost one", links("dir"));

    symlink("moved", m("dir/relative")).unwrap();
    symlink("../dir/moved", m("dir/upward")).unwrap();
    note("a relative link", content("dir/relative"));
    note("a link that goes up", content("dir/upward"));
    note("length of a link", len("dir/relative"));
    note("length of a file", len("dir/moved"));
    let canonical = canonicalize(m("dir/sub/../upward"));
    let canonical = canonical.map(|path| path.to_str().unwrap().replace(root, ""));
    note("canonical path", show(canonical));

    let whole = |path: &str| {
        let meta = meta(path).unwrap();
        format!("{:o} {:o}", meta.permissions().mode(), meta.mode())
    };
    note("the whole mode of a file", whole("dir/moved"));
    note("the whole mode of a directory", whole("dir"));
    note("the whole mode of a link", whole("dir/relative"));
    let set = |with: u32| {
        let set = chmod(m("dir/moved"), with);
        format!("{} {}", show(set), whole("dir/moved"))
    };
    note("a mode set with the kind of a file", set(0o100_604));
    note("a mode set with the kind of a directory", set(0o040_640));
    note("a mode set with every bit", set(0o7777));
    for bits in [0o644, 0o444, 0o464, 0o020, 0] {
        let mut perm = Permissions::from_mode(bits);
        let was = perm.readonly();
        perm.set_readonly(true);
        let locked = perm.mode();
        // That this lets everybody write is the case.
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let told = format!("{was} {locked:o} {:o}", perm.mode());
        note(&format!("read-only, of {bits:o}"), told);
    }

    chmod(m("dir/moved"), 0o600).unwrap();
    note("copy", show(copy(m("dir/moved"), m("copy"))));
    note("mode of a copy", mode("copy"));
    write(m("loud"), b"0123456789").unwrap();
    chmod(m("loud"), 0o777).unwrap();
    note("copy over a file", show(copy(m("dir/moved"), m("loud"))));
    note("mode of a file copied over", mode("loud"));
    note("content of a file copied over", content("loud"));
    chmod(m("dir/relative"), 0o640).unwrap();
    note("mode set through a link", mode("dir/moved"));
    let made = |path: &str, with: u32| {
        let mut options = OpenOptions::new();
        options.write(true).create(true).mode(with);
        format!("{} {}", show(options.open(m(path)).map(drop)), mode(path))
    };
    note("a file made with a mode", made("made", 0o604));
    note("a file opened with another", made("made", 0o777));
    note("a file made open to all", made("open", 0o777));
    note("a file made with every bit", made("every", 0o7777));
    let fresh = |path: &str| {
        let fresh = File::create_new(m(path)).and_then(|mut file| {
            file.write_all(b"new")?;
            at(&mut file, Start(1), read4)
        });
        format!("{} {}", show(fresh), mode(path))
    };
    note("a new file", fresh("fresh"));
    note("a new file where one is", fresh("fresh"));
    note("a new file by a link to one", fresh("dir/relative"));
    let options = File::options().read(true).open(m("fresh"));
    let read = options.and_then(|mut file| read4(&mut file));
    note("a file opened by its options", show(read));
    let built = |path: &str, with: u32| {
        let mut builder = DirBuilder::new();
        let built = builder.mode(with).recursive(true).create(m(path));
        format!("{} {}", show(built), mode(path))
    };
    note("a directory made open to all", built("wide", 0o777));
    note("a directory made with every bit", built("whole", 0o7777));
    note("directories made in one go", built("deep/er", 0o700));
    note("the first of them", mode("deep"));

    let mut held = File::open(m("loud")).unwrap();
    rename(m("dir/moved"), m("loud")).unwrap();
    note("a replaced file that is open", show(kind(&held)));
    note("reading a replaced file", show(read_all(&mut held)));
    let mut held = File::open(m("loud")).unwrap();
    remove_file(m("loud")).unwrap();
    note("a removed file that is open", show(kind(&held)));
    note("reading a removed file", show(read_all(&mut held)));

    create_dir(m("chain")).unwrap();
    write(m("chain/0"), b"end").unwrap();
    for hop in 1..=41 {
        symlink(m(&format!("chain/{}", hop - 1)), m(&format!("chain/{hop}"))).unwrap();
    }
    note("through forty links", content("chain/40"));
    note("through forty-one links", content("chain/41"));
    remove_dir_all(m("")).unwrap();
}

/// Names and paths at and past the lengths Linux allows, and ones that no
/// path can hold.
fn limits(root: &str, out: &mut Vec<String>) {
    let n = |rest: &str| format!("{root}/n/{rest}");
    create_dir(n("")).unwrap();
    write(n("short"), b"x").unwrap();
    for (what, len) in [("the longest name", 255), ("a name too long", 256)] {
        let name = |tag: &str| format!("{tag}{}", "x".repeat(len - 1));
        let at = |tag: &str| n(&name(tag));
        let rows = [
            ("a file", show(write(at("f"), b"x"))),
            ("a lookup", found(at("f"))),
            ("an open", show(File::open(at("f")).map(drop))),
            ("a directory", show(create_dir(at("d")))),
            ("directories", show(create_dir_all(at("a") + "/below"))),
            ("a link", show(symlink(n(""), at("l")))),
            ("a hard link", show(hard_link(n("short"), at("h")))),
            ("a rename to it", show(rename(at("h"), at("r")))),
            ("a rename from it", show(rename(at("m"), n("moved")))),
            ("a removal", show(remove_file(at("r")))),
            ("a path through it", found(at("d") + "/below")),
            ("a path through a file", found(n("short/") + &name("f"))),
        ];
        out.extend(rows.map(|(row, result)| format!("{row} by {what}: {result}")));
    }
    let name = "/".to_owned() + &"d".repeat(249);
    let (long, too_long) = (n("deep") + &name.repeat(12), n("deep") + &name.repeat(20));
    // How long a link's target may be depends on the filesystem, up to a
    // path's limit. Every one takes a thousand bytes.
    let (far, too_far) = ("t".repeat(1000), "t".repeat(4096));
    let len = |path: String| show(symlink_metadata(path).map(|meta| meta.len()));
    let rows = [
        ("a path of 3000 bytes", show(create_dir_all(&long))),
        ("a lookup of it", found(long)),
        ("a removal of it", show(remove_dir_all(n("deep")))),
        ("a path of 5000 bytes", show(create_dir_all(&too_long))),
        ("a lookup of that", found(too_long)),
        ("a removal of that", show(remove_dir_all(n("deep")))),
        ("a lookup of dots too long", found(n(&"./".repeat(2500)))),
        ("a link to a long path", show(symlink(far, n("far")))),
        ("its length", len(n("far"))),
        ("a link too far", show(symlink(too_far, n("farther")))),
        ("a link to nothing", show(symlink("", n("empty")))),
        ("a name with a nul", show(write(n("nu\0l"), b"x"))),
        ("a lookup of a nul", found(n("\0"))),
        ("a link to a nul", show(symlink("nu\0l", n("nul")))),
        ("a link by a nul", show(symlink("target", n("nu\0l")))),
        ("a rename to a nul", show(rename(n("short"), n("nu\0l")))),
        ("an odd name", show(write(n(" a b\n\u{e9}.. "), b"x"))),
    ];
    out.extend(rows.map(|(row, result)| format!("{row}: {result}")));
    let mut there = Vec::new();
    describe(root, "/n", &mut there);
    let there = format!("{there:?}").replace(&"t".repeat(1000), "<long>");
    let there = there.replace(&"x".repeat(254), "<x>");
    out.push(format!("what is there: {there}"));
    remove_dir_all(n("")).unwrap();
}

/// How a directory lists an entry: by the path asked for and its name.
fn listed(root: &str, entry: io::Result<DirEntry>) -> String {
    let entry = entry.unwrap();
    let (kind, meta) = (entry.file_type().unwrap(), entry.metadata().unwrap());
    let kinds = (kind.is_dir(), kind.is_file(), kind.is_symlink());
    let device = kind.is_block_device() || kind.is_char_device();
    let special = device || kind.is_fifo() || kind.is_socket();
    let (link, numbered) = (meta.is_symlink(), entry.ino() == meta.ino());
    let path = entry.path().to_str().unwrap().replace(root, "");
    let name = entry.file_name();
    format!("{path} {name:?} {kinds:?} {special} {link} {numbered}")
}

/// What a directory lists, by the path it was asked through.
fn listings(root: &str, out: &mut Vec<String>) {
    let p = |rest: &str| format!("{root}{rest}");
    create_dir_all(p("/l/dir/sub")).unwrap();
    write(p("/l/dir/file"), b"x").unwrap();
    symlink(p("/l/dir/file"), p("/l/dir/link")).unwrap();
    symlink(p("/l/dir"), p("/l/to_dir")).unwrap();
    let paths = "/l/dir /l/dir/ /l/to_dir /l/to_dir/ /l/dir/. /l/dir/sub/.. /l//dir";
    for path in paths.split_whitespace() {
        let entries = read_dir(p(path)).map(|entries| {
            let mut entries: Vec<_> = entries.map(|entry| listed(root, entry)).collect();
            entries.sort();
            entries
        });
        out.push(format!("listing {path}: {}", show(entries)));
    }
    remove_dir_all(p("/l")).unwrap();
}

type TimedOp = fn(At) -> io::Result<()>;

const TIMED_OPS: &[(&str, TimedOp)] = &[
    ("nothing", |_| Ok(())),
    ("write", |c| write(c("a/file"), b"new")),
    ("write nothing", |c| rewritten(c("a/file"), b"")),
    ("write by a handle", |c| rewritten(c("a/file"), b"n")),
    ("append", |c| appended(c("a/file"), b"n")),
    ("open to write", |c| opens(c("a/file"), |o| o)),
    ("open to truncate", |c| truncated(c("a/file"))),
    ("open to truncate nothing", |c| truncated(c("a/empty"))),
    ("open to create", |c| opens(c("a/file"), |o| o.create(true))),
    ("set_len to the same", |c| resized(c("a/file"), 3)),
    ("set_len", |c| resized(c("a/file"), 1)),
    ("read", |c| read(c("a/file")).map(drop)),
    ("read_dir", |c| names(c("a")).map(drop)),
    ("metadata", |c| metadata(c("a/file")).map(drop)),
    ("sync", |c| File::open(c("a/file"))?.sync_all()),
    ("sync a directory", |c| File::open(c("a"))?.sync_all()),
    ("create a file", |c| write(c("a/new"), b"")),
    ("create a directory", |c| create_dir(c("a/new"))),
    ("create directories", |c| create_dir_all(c("a/new/er"))),
    ("create what is there", |c| create_dir_all(c("a/sub"))),
    ("create a link", |c| symlink("file", c("a/new"))),
    ("create a twin", |c| hard_link(c("a/file"), c("b/twin"))),
    ("remove a file", |c| remove_file(c("a/file"))),
    ("remove a twin", |c| remove_file(c("a/twin"))),
    ("remove a link", |c| remove_file(c("a/link"))),
    ("remove a directory", |c| remove_dir(c("a/sub"))),
    ("remove a tree", |c| remove_dir_all(c("a"))),
    ("rename in place", |c| rename(c("a/file"), c("a/moved"))),
    ("rename across", |c| rename(c("a/file"), c("b/moved"))),
    ("rename over a file", |c| rename(c("a/file"), c("a/empty"))),
    ("rename onto its twin", |c| rename(c("a/file"), c("a/twin"))),
    ("rename onto itself", |c| rename(c("a/file"), c("a/file"))),
    ("rename a directory", |c| rename(c("a/sub"), c("b/sub"))),
    ("rename one in place", |c| rename(c("a/sub"), c("a/moved"))),
    ("rename one over one", |c| rename(c("a/sub"), c("b/empty"))),
    ("rename a link", |c| rename(c("a/link"), c("b/link"))),
    ("set permissions", |c| chmod(c("a/file"), 0o600)),
    ("set the same permissions", |c| chmod(c("a/file"), 0o644)),
    ("set a directory's", |c| chmod(c("a"), 0o700)),
    ("set them through a link", |c| chmod(c("a/link"), 0o600)),
    ("set them by a handle", |c| fchmod(c("a/file"), 0o600)),
    ("set the modified time", |c| {
        retimed(c("a/file"), FileTimes::new().set_modified(early(5)))
    }),
    ("set the accessed time", |c| {
        retimed(c("a/file"), FileTimes::new().set_accessed(early(5)))
    }),
    ("set no time", |c| retimed(c("a/file"), FileTimes::new())),
    ("set a directory's time", |c| {
        retimed(c("a"), FileTimes::new().set_modified(early(5)))
    }),
    ("copy", |c| copied(c("a/file"), c("b/copy"))),
    ("copy over a file", |c| copied(c("a/file"), c("a/empty"))),
    ("fail to create", |c| create_dir(c("a/sub"))),
    ("fail to remove", |c| remove_dir(c("a"))),
    ("fail to rename", |c| rename(c("a"), c("a/sub/in"))),
];

/// When an inode was last modified, and when it last changed.
type Stamps = ([i64; 2], [i64; 2]);

/// The path of an entry, the inode it names, and the stamps of that.
type Stamped = (String, u64, Stamps);

/// Everything under `dir`, and `dir` itself.
fn stamps(root: &str, dir: &str, out: &mut Vec<Stamped>) {
    let stamp = |path: &str| {
        let meta = symlink_metadata(format!("{root}{path}")).unwrap();
        let modified = [meta.mtime(), meta.mtime_nsec()];
        let stamps = (modified, [meta.ctime(), meta.ctime_nsec()]);
        (meta.is_dir(), (path.to_owned(), meta.ino(), stamps))
    };
    out.push(stamp(dir).1);
    for name in names(format!("{root}{dir}")).unwrap() {
        let path = format!("{dir}/{name}");
        match stamp(&path) {
            (true, _) => stamps(root, &path, out),
            (false, entry) => out.push(entry),
        }
    }
}

/// Which stamps of an entry differ from those its inode had `before`: `m`
/// for modified, `c` for changed. An inode that is new has both. Going by
/// the inode keeps a rename from comparing two files, made a moment apart.
fn moved(before: &[Stamped], (path, ino, now): &Stamped) -> Option<String> {
    let was = before.iter().find(|(_, old, _)| old == ino);
    let was = was.map(|(.., was)| was);
    let modified = was.is_none_or(|was| was.0 != now.0);
    let changed = was.is_none_or(|was| was.1 != now.1);
    let marks = ["", "m"][modified as usize].to_owned() + ["", "c"][changed as usize];
    (!marks.is_empty()).then(|| format!("{path}:{marks}"))
}

/// Which timestamps each operation moves. `tick` lets time pass.
fn times(root: &str, tick: &dyn Fn(), out: &mut Vec<String>) {
    let c = |rest: &str| format!("{root}/c/{rest}");
    for (name, op) in TIMED_OPS {
        let _ = remove_dir_all(c(""));
        create_dir_all(c("a/sub")).unwrap();
        create_dir_all(c("b/empty")).unwrap();
        write(c("a/file"), b"abc").unwrap();
        write(c("a/empty"), b"").unwrap();
        hard_link(c("a/file"), c("a/twin")).unwrap();
        symlink("file", c("a/link")).unwrap();
        tick();
        let (mut before, mut after) = (Vec::new(), Vec::new());
        stamps(root, "/c", &mut before);
        let result = show(op(&c));
        stamps(root, "/c", &mut after);
        let moved = after.iter().filter_map(|now| moved(&before, now));
        let moved: Vec<_> = moved.collect();
        out.push(format!("times of {name}: {result} {moved:?}"));
    }
    remove_dir_all(c("")).unwrap();
}

/// Runs every case under `root`, and reports one line for each. `tick`
/// lets enough time pass for a timestamp to tell before from after.
pub fn run(root: &str, tick: &dyn Fn()) -> Vec<String> {
    let mut out = Vec::new();
    paths(root, &mut out);
    options(root, &mut out);
    handles(root, &mut out);
    directories(root, &mut out);
    attributes(root, &mut out);
    limits(root, &mut out);
    listings(root, &mut out);
    times(root, tick, &mut out);
    out
}
