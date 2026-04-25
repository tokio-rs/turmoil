//! Symlink and hard link tests.

use turmoil::fs::shim::std::fs::{
    canonicalize, create_dir, hard_link, metadata, read, symlink, write,
};
use turmoil::{Builder, Result};

#[test]
fn canonicalize_normalizes_path() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/file.txt", b"hello")?;
        assert_eq!(
            canonicalize("/data/./file.txt")?,
            std::path::PathBuf::from("/data/file.txt")
        );
        assert_eq!(
            canonicalize("/data/../data/file.txt")?,
            std::path::PathBuf::from("/data/file.txt")
        );
        Ok(())
    });
    sim.run()
}

#[test]
fn canonicalize_resolves_symlinks() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/real_file.txt", b"hello")?;
        symlink("/data/real_file.txt", "/link_to_file")?;
        assert_eq!(
            canonicalize("/link_to_file")?,
            std::path::PathBuf::from("/data/real_file.txt")
        );
        Ok(())
    });
    sim.run()
}

#[test]
fn hard_link_basic() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/original.txt", b"hardlink test")?;
        hard_link("/data/original.txt", "/data/link.txt")?;
        assert!(metadata("/data/link.txt").is_ok());
        assert_eq!(read("/data/link.txt")?, b"hardlink test");
        Ok(())
    });
    sim.run()
}

/// Regression test: renaming a file should not make it appear as a directory or symlink
#[test]
fn rename_file_not_dir_or_symlink() -> Result {
    use turmoil::fs::shim::std::fs::{remove_file, rename};
    let mut sim = Builder::new().build();
    sim.client("test", async {
        write("/src.txt", b"data")?;
        rename("/src.txt", "/dst.txt")?;
        remove_file("/dst.txt")?;
        assert!(
            metadata("/dst.txt").is_err(),
            "file should not exist after remove"
        );
        Ok(())
    });
    sim.run()
}

/// Test that symlinks can be removed via remove_file
#[test]
fn remove_symlink() -> Result {
    use turmoil::fs::shim::std::fs::{exists, remove_file, symlink_metadata};
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/target.txt", b"hello")?;
        symlink("/data/target.txt", "/link")?;

        // Symlink exists
        assert!(symlink_metadata("/link").is_ok());
        assert!(symlink_metadata("/link")?.is_symlink());

        // Remove the symlink
        remove_file("/link")?;

        // Symlink should no longer exist
        assert!(symlink_metadata("/link").is_err());

        // But target still exists
        assert!(exists("/data/target.txt"));

        Ok(())
    });
    sim.run()
}

/// Test that rmdir fails on directory containing symlinks
#[test]
fn rmdir_fails_with_symlink() -> Result {
    use turmoil::fs::shim::std::fs::remove_dir;
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        symlink("/nonexistent", "/data/link")?;

        // Should fail because directory contains a symlink
        assert!(remove_dir("/data").is_err());

        Ok(())
    });
    sim.run()
}

/// Test hard link nlink count
#[test]
fn hard_link_nlink_count() -> Result {
    use std::os::unix::fs::MetadataExt;
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/original.txt", b"test")?;

        // Original file starts with nlink=1
        let meta = metadata("/data/original.txt")?;
        assert_eq!(meta.nlink(), 1);

        // After creating hard link, both should have nlink=2
        hard_link("/data/original.txt", "/data/link.txt")?;
        let orig_meta = metadata("/data/original.txt")?;
        let link_meta = metadata("/data/link.txt")?;
        assert_eq!(orig_meta.nlink(), 2);
        assert_eq!(link_meta.nlink(), 2);

        Ok(())
    });
    sim.run()
}

/// Test that symlinks appear in directory listings
#[test]
fn symlinks_in_read_dir() -> Result {
    use std::collections::HashSet;
    use turmoil::fs::shim::std::fs::read_dir;
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/file.txt", b"hello")?;
        symlink("/data/file.txt", "/data/link")?;

        // Read directory entries
        let entries: Vec<_> = read_dir("/data")?.collect::<std::io::Result<Vec<_>>>()?;
        let names: HashSet<_> = entries
            .iter()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        // Both file and symlink should appear
        assert!(names.contains("file.txt"), "file.txt should be in listing");
        assert!(names.contains("link"), "symlink should be in listing");
        assert_eq!(names.len(), 2);

        Ok(())
    });
    sim.run()
}

/// Test renaming a symlink
#[test]
fn rename_symlink() -> Result {
    use turmoil::fs::shim::std::fs::{rename, symlink_metadata};
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/target.txt", b"hello")?;
        symlink("/data/target.txt", "/data/link")?;

        // Rename the symlink
        rename("/data/link", "/data/renamed_link")?;

        // Old name should not exist
        assert!(symlink_metadata("/data/link").is_err());

        // New name should be a symlink
        let meta = symlink_metadata("/data/renamed_link")?;
        assert!(meta.is_symlink());

        // Should still point to the target
        let contents = read("/data/renamed_link")?;
        assert_eq!(contents, b"hello");

        Ok(())
    });
    sim.run()
}

/// Test that symlink cycles are detected and return an error.
#[test]
fn symlink_cycle_detected() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        // Create a direct cycle: link1 -> link2 -> link1
        symlink("/link2", "/link1")?;
        symlink("/link1", "/link2")?;

        // canonicalize should detect the cycle and return an error
        let result = canonicalize("/link1");
        assert!(result.is_err(), "symlink cycle should be detected");

        Ok(())
    });
    sim.run()
}

/// Test that long symlink chains are handled correctly.
#[test]
fn symlink_chain_limit() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        // Create a chain that eventually resolves
        create_dir("/data")?;
        write("/data/file.txt", b"hello")?;

        // Create chain: /chain3 -> /chain2 -> /chain1 -> /data/file.txt
        symlink("/data/file.txt", "/chain1")?;
        symlink("/chain1", "/chain2")?;
        symlink("/chain2", "/chain3")?;

        // Should resolve successfully
        let result = canonicalize("/chain3")?;
        assert_eq!(result, std::path::PathBuf::from("/data/file.txt"));

        Ok(())
    });
    sim.run()
}
