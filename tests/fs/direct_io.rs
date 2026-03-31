//! Tests for O_DIRECT alignment enforcement.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::os::unix::fs::FileExt;
use turmoil::fs::shim::std::fs::{create_dir_all, OpenOptions};
use turmoil::{Builder, Result};

const TEST_PATH: &str = "/test/data.db";

/// Allocate a zeroed buffer with the given size and alignment.
/// Returns (pointer, layout) — caller must dealloc.
unsafe fn aligned_buf(size: usize, align: usize) -> (*mut u8, Layout) {
    let layout = Layout::from_size_align(size, align).unwrap();
    let ptr = alloc_zeroed(layout);
    assert!(!ptr.is_null(), "allocation failed");
    (ptr, layout)
}

// ---------------------------------------------------------------------------
// Aligned I/O succeeds
// ---------------------------------------------------------------------------

#[test]
fn aligned_read_succeeds_default_512() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;

        // Pre-create file with data
        let setup = OpenOptions::new()
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        setup.write_all_at(&[0xAB; 8192], 0)?;
        drop(setup);

        let file = OpenOptions::new()
            .read(true)
            .direct_io(true)
            .open(TEST_PATH)?;

        unsafe {
            let (ptr, layout) = aligned_buf(512, 512);
            let buf = std::slice::from_raw_parts_mut(ptr, 512);
            file.read_at(buf, 0)?;
            dealloc(ptr, layout);
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn aligned_write_succeeds_default_512() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .direct_io(true)
            .open(TEST_PATH)?;

        unsafe {
            let (ptr, layout) = aligned_buf(512, 512);
            let buf = std::slice::from_raw_parts(ptr, 512);
            file.write_all_at(buf, 0)?;
            dealloc(ptr, layout);
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn aligned_io_succeeds_4096() -> Result {
    let mut builder = Builder::new();
    builder.fs().direct_io_alignment(4096);
    let mut sim = builder.build();

    sim.client("test", async {
        create_dir_all("/test")?;

        // Pre-create file
        let setup = OpenOptions::new()
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        setup.write_all_at(&[0xAB; 8192], 0)?;
        drop(setup);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .direct_io(true)
            .open(TEST_PATH)?;

        unsafe {
            let (ptr, layout) = aligned_buf(4096, 4096);
            let wbuf = std::slice::from_raw_parts(ptr, 4096);
            file.write_all_at(wbuf, 0)?;
            let rbuf = std::slice::from_raw_parts_mut(ptr, 4096);
            file.read_at(rbuf, 0)?;
            dealloc(ptr, layout);
        }
        Ok(())
    });
    sim.run()
}

// ---------------------------------------------------------------------------
// Misaligned buffer length
// ---------------------------------------------------------------------------

#[test]
fn misaligned_write_length_fails() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .direct_io(true)
            .open(TEST_PATH)?;

        // 100 bytes is not a multiple of 512
        unsafe {
            let (ptr, layout) = aligned_buf(512, 512);
            let buf = std::slice::from_raw_parts(ptr, 100); // length not aligned
            let res = file.write_all_at(buf, 0);
            dealloc(ptr, layout);
            assert!(res.is_err(), "expected EINVAL for misaligned length");
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn misaligned_read_length_fails() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;

        let setup = OpenOptions::new()
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        setup.write_all_at(&[0xAB; 8192], 0)?;
        drop(setup);

        let file = OpenOptions::new()
            .read(true)
            .direct_io(true)
            .open(TEST_PATH)?;

        unsafe {
            let (ptr, layout) = aligned_buf(512, 512);
            let buf = std::slice::from_raw_parts_mut(ptr, 100); // length not aligned
            let res = file.read_at(buf, 0);
            dealloc(ptr, layout);
            assert!(res.is_err(), "expected EINVAL for misaligned length");
        }
        Ok(())
    });
    sim.run()
}

// ---------------------------------------------------------------------------
// Misaligned offset
// ---------------------------------------------------------------------------

#[test]
fn misaligned_write_offset_fails() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .direct_io(true)
            .open(TEST_PATH)?;

        unsafe {
            let (ptr, layout) = aligned_buf(512, 512);
            let buf = std::slice::from_raw_parts(ptr, 512);
            let res = file.write_all_at(buf, 1); // offset not aligned
            dealloc(ptr, layout);
            assert!(res.is_err(), "expected EINVAL for misaligned offset");
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn misaligned_read_offset_fails() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;

        let setup = OpenOptions::new()
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        setup.write_all_at(&[0xAB; 8192], 0)?;
        drop(setup);

        let file = OpenOptions::new()
            .read(true)
            .direct_io(true)
            .open(TEST_PATH)?;

        unsafe {
            let (ptr, layout) = aligned_buf(512, 512);
            let buf = std::slice::from_raw_parts_mut(ptr, 512);
            let res = file.read_at(buf, 7); // offset not aligned
            dealloc(ptr, layout);
            assert!(res.is_err(), "expected EINVAL for misaligned offset");
        }
        Ok(())
    });
    sim.run()
}

// ---------------------------------------------------------------------------
// Misaligned buffer pointer
// ---------------------------------------------------------------------------

#[test]
fn misaligned_buffer_pointer_fails() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .direct_io(true)
            .open(TEST_PATH)?;

        unsafe {
            // Allocate 1024 bytes aligned to 512, then offset pointer by 1 byte
            let (ptr, layout) = aligned_buf(1024, 512);
            let misaligned_ptr = ptr.add(1);
            let buf = std::slice::from_raw_parts(misaligned_ptr, 512);
            let res = file.write_all_at(buf, 0);
            dealloc(ptr, layout);
            assert!(
                res.is_err(),
                "expected EINVAL for misaligned buffer pointer"
            );
        }
        Ok(())
    });
    sim.run()
}

// ---------------------------------------------------------------------------
// Non-direct_io files are unaffected
// ---------------------------------------------------------------------------

#[test]
fn non_direct_io_unaligned_succeeds() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(TEST_PATH)?;

        // Unaligned length and offset — should succeed without direct_io.
        file.write_all_at(b"hello", 3)?;
        let mut buf = [0u8; 5];
        file.read_at(&mut buf, 3)?;
        assert_eq!(&buf, b"hello");
        Ok(())
    });
    sim.run()
}

// ---------------------------------------------------------------------------
// Custom alignment (4096) rejects smaller alignments
// ---------------------------------------------------------------------------

#[test]
fn custom_alignment_rejects_512_buffer() -> Result {
    let mut builder = Builder::new();
    builder.fs().direct_io_alignment(4096);
    let mut sim = builder.build();

    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .direct_io(true)
            .open(TEST_PATH)?;

        // 512 is aligned to 512 but not to 4096
        unsafe {
            let (ptr, layout) = aligned_buf(4096, 4096);
            let buf = std::slice::from_raw_parts(ptr, 512); // length not aligned to 4096
            let res = file.write_all_at(buf, 0);
            dealloc(ptr, layout);
            assert!(
                res.is_err(),
                "expected EINVAL: 512-byte buffer with 4096 alignment"
            );
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn custom_alignment_rejects_misaligned_offset() -> Result {
    let mut builder = Builder::new();
    builder.fs().direct_io_alignment(4096);
    let mut sim = builder.build();

    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .direct_io(true)
            .open(TEST_PATH)?;

        unsafe {
            let (ptr, layout) = aligned_buf(4096, 4096);
            let buf = std::slice::from_raw_parts(ptr, 4096);
            let res = file.write_all_at(buf, 512); // 512 not aligned to 4096
            dealloc(ptr, layout);
            assert!(
                res.is_err(),
                "expected EINVAL: offset 512 with 4096 alignment"
            );
        }
        Ok(())
    });
    sim.run()
}
