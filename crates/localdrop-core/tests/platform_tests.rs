//! Platform-specific tests for `LocalDrop`.
//!
//! These tests verify cross-platform compatibility for:
//! - File permissions (Unix vs Windows)
//! - Symlink handling
//! - Network socket options

use std::path::PathBuf;
use tempfile::TempDir;

use localdrop_core::file::{
    apply_permissions, create_symlink, enumerate_files, EnumerateOptions, FileMetadata, SymlinkMode,
};

#[cfg(unix)]
mod unix_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Test that Unix permissions are correctly captured from file metadata.
    #[test]
    fn test_unix_permissions_captured() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let file_path = temp_dir.path().join("test_file.txt");

        std::fs::write(&file_path, "test content").expect("write file");

        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&file_path, perms).expect("set permissions");

        let metadata = FileMetadata::from_path(&file_path, temp_dir.path()).expect("get metadata");

        assert!(
            metadata.permissions.is_some(),
            "Unix permissions should be captured"
        );

        let captured_mode = metadata.permissions.unwrap() & 0o7777;
        assert_eq!(captured_mode, 0o755, "Permissions should match what we set");
    }

    /// Test that permissions can be applied to a file.
    #[test]
    fn test_unix_permissions_applied() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let file_path = temp_dir.path().join("test_file.txt");

        std::fs::write(&file_path, "test content").expect("write file");

        apply_permissions(&file_path, Some(0o644)).expect("apply permissions");

        let metadata = std::fs::metadata(&file_path).expect("get metadata");
        let mode = metadata.permissions().mode() & 0o7777;

        assert_eq!(mode, 0o644, "Permissions should be applied");
    }

    /// Test that executable permission is preserved.
    #[test]
    fn test_unix_executable_permission() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let file_path = temp_dir.path().join("script.sh");

        std::fs::write(&file_path, "#!/bin/bash\necho hello").expect("write file");

        apply_permissions(&file_path, Some(0o755)).expect("apply permissions");

        let metadata = std::fs::metadata(&file_path).expect("get metadata");
        let mode = metadata.permissions().mode();

        assert!(mode & 0o111 != 0, "File should be executable");
    }

    /// Test that symlinks can be created on Unix.
    #[test]
    fn test_unix_symlink_creation() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let target_path = temp_dir.path().join("target.txt");
        let link_path = temp_dir.path().join("link.txt");

        std::fs::write(&target_path, "target content").expect("write target");

        create_symlink(&link_path, &target_path).expect("create symlink");

        let link_metadata = std::fs::symlink_metadata(&link_path).expect("get link metadata");
        assert!(link_metadata.is_symlink(), "Should be a symlink");

        let resolved = std::fs::read_link(&link_path).expect("read link");
        assert_eq!(resolved, target_path, "Should point to target");

        let content = std::fs::read_to_string(&link_path).expect("read through link");
        assert_eq!(content, "target content");
    }

    /// Test that symlink targets are captured in metadata.
    #[test]
    fn test_unix_symlink_target_captured() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let target_path = temp_dir.path().join("target.txt");
        let link_path = temp_dir.path().join("link.txt");

        std::fs::write(&target_path, "target content").expect("write target");

        std::os::unix::fs::symlink(&target_path, &link_path).expect("create symlink");

        let metadata = FileMetadata::from_path(&link_path, temp_dir.path()).expect("get metadata");

        assert!(metadata.is_symlink, "Should be marked as symlink");
        assert!(
            metadata.symlink_target.is_some(),
            "Symlink target should be captured"
        );
        assert_eq!(
            metadata.symlink_target.unwrap(),
            target_path,
            "Target should match"
        );
    }

    /// Test symlink preservation mode in enumerate.
    #[test]
    fn test_unix_symlink_preserve_mode() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let target_path = temp_dir.path().join("target.txt");
        let link_path = temp_dir.path().join("link.txt");

        std::fs::write(&target_path, "target content").expect("write target");

        std::os::unix::fs::symlink(&target_path, &link_path).expect("create symlink");

        let options = EnumerateOptions::preserve_symlinks();
        let files = enumerate_files(&[link_path], &options).expect("enumerate with preserve");

        assert_eq!(files.len(), 1, "Should find the symlink");
        assert!(files[0].is_symlink, "Should be marked as symlink");
    }
}

#[cfg(windows)]
mod windows_tests {
    use super::*;

    /// Test that permission handling is a no-op on Windows.
    #[test]
    fn test_windows_permissions_noop() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let file_path = temp_dir.path().join("test_file.txt");

        std::fs::write(&file_path, "test content").expect("write file");

        let metadata = FileMetadata::from_path(&file_path, temp_dir.path()).expect("get metadata");
        assert!(
            metadata.permissions.is_none(),
            "Windows should not capture Unix permissions"
        );

        let result = apply_permissions(&file_path, Some(0o755));
        assert!(result.is_ok(), "apply_permissions should succeed (no-op)");
    }

    /// Test that symlink creation falls back to copy on Windows.
    #[test]
    fn test_windows_symlink_fallback() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let target_path = temp_dir.path().join("target.txt");
        let link_path = temp_dir.path().join("link.txt");

        std::fs::write(&target_path, "target content").expect("write target");

        let result = create_symlink(&link_path, &target_path);

        if result.is_ok() {
            let content = std::fs::read_to_string(&link_path).expect("read link");
            assert_eq!(content, "target content", "Content should match target");
        }
    }
}

/// Test that follow symlinks mode works on all platforms.
#[test]
fn test_symlink_follow_mode() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let file_path = temp_dir.path().join("regular.txt");

    std::fs::write(&file_path, "regular content").expect("write file");

    let options = EnumerateOptions::follow_symlinks();
    let files = enumerate_files(&[file_path], &options).expect("enumerate");

    assert_eq!(files.len(), 1, "Should find the file");
    assert!(
        !files[0].is_symlink,
        "Regular file should not be marked as symlink"
    );
}

/// Test that skip symlinks mode works on all platforms.
#[test]
fn test_symlink_skip_mode() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let file_path = temp_dir.path().join("regular.txt");

    std::fs::write(&file_path, "regular content").expect("write file");

    let options = EnumerateOptions::skip_symlinks();
    let files = enumerate_files(&[file_path], &options).expect("enumerate");

    assert_eq!(files.len(), 1, "Should find the file");
}

/// Test that `EnumerateOptions` builder methods work correctly.
#[test]
fn test_enumerate_options_builder() {
    let default = EnumerateOptions::default();
    assert!(matches!(default.symlink_mode, SymlinkMode::Follow));
    assert!(!default.include_hidden);
    assert!(default.max_depth.is_none());

    let follow = EnumerateOptions::follow_symlinks();
    assert!(matches!(follow.symlink_mode, SymlinkMode::Follow));

    let preserve = EnumerateOptions::preserve_symlinks();
    assert!(matches!(preserve.symlink_mode, SymlinkMode::Preserve));

    let skip = EnumerateOptions::skip_symlinks();
    assert!(matches!(skip.symlink_mode, SymlinkMode::Skip));

    let chained = EnumerateOptions::follow_symlinks()
        .with_hidden(true)
        .with_max_depth(5);
    assert!(chained.include_hidden);
    assert_eq!(chained.max_depth, Some(5));
}

/// Test that file metadata correctly handles regular files.
#[test]
fn test_file_metadata_regular_file() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let file_path = temp_dir.path().join("test.txt");

    let content = b"Hello, LocalDrop!";
    std::fs::write(&file_path, content).expect("write file");

    let metadata = FileMetadata::from_path(&file_path, temp_dir.path()).expect("get metadata");

    assert_eq!(metadata.relative_path, PathBuf::from("test.txt"));
    assert_eq!(metadata.size, content.len() as u64);
    assert_eq!(metadata.file_name(), "test.txt");
    assert!(!metadata.is_symlink);
    assert!(metadata.symlink_target.is_none());
    assert!(metadata.mime_type.is_some());
    assert!(metadata.created.is_some() || metadata.modified.is_some());
}

/// Test directory enumeration.
#[test]
fn test_directory_enumeration() {
    let temp_dir = TempDir::new().expect("create temp dir");

    let sub_dir = temp_dir.path().join("subdir");
    std::fs::create_dir(&sub_dir).expect("create subdir");

    std::fs::write(temp_dir.path().join("file1.txt"), "content 1").expect("write file1");
    std::fs::write(sub_dir.join("file2.txt"), "content 2").expect("write file2");
    std::fs::write(sub_dir.join("file3.txt"), "content 3").expect("write file3");

    let options = EnumerateOptions::default();
    let files = enumerate_files(&[temp_dir.path().to_path_buf()], &options).expect("enumerate");

    assert_eq!(files.len(), 3, "Should find all 3 files");

    let paths: Vec<_> = files.iter().map(|f| f.relative_path.clone()).collect();
    assert!(paths.iter().any(|p| p.ends_with("file1.txt")));
    assert!(paths.iter().any(|p| p.ends_with("file2.txt")));
    assert!(paths.iter().any(|p| p.ends_with("file3.txt")));
}

/// Test hidden file handling.
#[test]
fn test_hidden_files() {
    let temp_dir = TempDir::new().expect("create temp dir");

    std::fs::write(temp_dir.path().join("visible.txt"), "visible").expect("write visible");
    std::fs::write(temp_dir.path().join(".hidden"), "hidden").expect("write hidden");

    let options = EnumerateOptions::default();
    let files = enumerate_files(&[temp_dir.path().to_path_buf()], &options).expect("enumerate");
    assert_eq!(files.len(), 1, "Should only find visible file");
    assert_eq!(files[0].file_name(), "visible.txt");

    let options = EnumerateOptions::default().with_hidden(true);
    let files = enumerate_files(&[temp_dir.path().to_path_buf()], &options).expect("enumerate");
    assert_eq!(files.len(), 2, "Should find both files");
}

/// Test max depth option.
#[test]
fn test_max_depth() {
    let temp_dir = TempDir::new().expect("create temp dir");

    let level1 = temp_dir.path().join("level1");
    let level2 = level1.join("level2");
    let level3 = level2.join("level3");
    std::fs::create_dir_all(&level3).expect("create dirs");

    std::fs::write(temp_dir.path().join("root.txt"), "root").expect("write root");
    std::fs::write(level1.join("l1.txt"), "level 1").expect("write l1");
    std::fs::write(level2.join("l2.txt"), "level 2").expect("write l2");
    std::fs::write(level3.join("l3.txt"), "level 3").expect("write l3");

    let options = EnumerateOptions::default().with_max_depth(2);
    let files = enumerate_files(&[temp_dir.path().to_path_buf()], &options).expect("enumerate");

    assert_eq!(files.len(), 2, "Should find 2 files within depth limit");

    let names: Vec<_> = files.iter().map(FileMetadata::file_name).collect();
    assert!(names.contains(&"root.txt"));
    assert!(names.contains(&"l1.txt"));
}
