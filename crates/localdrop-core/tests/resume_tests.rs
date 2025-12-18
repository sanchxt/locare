//! Tests for transfer resume functionality.
//!
//! These tests verify the resume state persistence and recovery mechanisms.

use std::path::PathBuf;

use tempfile::TempDir;
use uuid::Uuid;

use localdrop_core::file::FileMetadata;
use localdrop_core::transfer::resume::ResumeManager;
use localdrop_core::transfer::ResumeState;

// ============================================================================
// Resume State Tests
// ============================================================================

/// Test that ResumeState can be serialized and deserialized correctly.
#[test]
fn test_resume_state_serialization() {
    let state = create_test_resume_state("TEST-123");

    // Serialize to JSON
    let json = serde_json::to_string_pretty(&state).expect("serialize");

    // Deserialize back
    let restored: ResumeState = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(restored.transfer_id, state.transfer_id);
    assert_eq!(restored.code, state.code);
    assert_eq!(restored.files.len(), state.files.len());
    assert_eq!(restored.sender_device, state.sender_device);
    assert_eq!(restored.output_dir, state.output_dir);
    assert_eq!(restored.bytes_received, state.bytes_received);
    assert_eq!(restored.total_bytes, state.total_bytes);
    assert_eq!(restored.protocol_version, state.protocol_version);
}

/// Test that ResumeState correctly tracks chunk completion.
#[test]
fn test_resume_state_chunk_tracking() {
    let mut state = create_test_resume_state("TEST-456");

    // Initially no chunks completed
    assert!(state.completed_chunks.is_empty());
    assert_eq!(state.bytes_received, 0);

    // Mark some chunks as completed
    state.mark_chunk_completed(0, 0, 1024);
    state.mark_chunk_completed(0, 1, 1024);
    state.mark_chunk_completed(0, 2, 512); // Last chunk might be smaller

    // Verify tracking
    let chunks = state.get_completed_chunks(0);
    assert_eq!(chunks.len(), 3);
    assert!(chunks.contains(&0));
    assert!(chunks.contains(&1));
    assert!(chunks.contains(&2));

    assert_eq!(state.bytes_received, 2560);
}

/// Test that duplicate chunk completions don't double-count bytes.
#[test]
fn test_resume_state_duplicate_chunks() {
    let mut state = create_test_resume_state("TEST-789");

    // Mark same chunk twice
    state.mark_chunk_completed(0, 0, 1024);
    state.mark_chunk_completed(0, 0, 1024);

    // Should only count once
    let chunks = state.get_completed_chunks(0);
    assert_eq!(chunks.len(), 1);
    assert_eq!(state.bytes_received, 1024);
}

/// Test that file completion tracking works correctly.
#[test]
fn test_resume_state_file_completion() {
    let mut state = create_test_resume_state("TEST-ABC");

    // File not completed initially
    assert!(!state.is_file_completed(0));
    assert!(!state.is_transfer_completed());

    // Mark file as completed with hash
    let test_hash: [u8; 32] = [0x42; 32];
    state.mark_file_completed(0, &test_hash);

    // Now it should be marked complete
    assert!(state.is_file_completed(0));

    // But not the other file
    assert!(!state.is_file_completed(1));
    assert!(!state.is_transfer_completed());

    // Complete the second file
    state.mark_file_completed(1, &test_hash);
    assert!(state.is_transfer_completed());
}

/// Test progress percentage calculation.
#[test]
fn test_resume_state_progress() {
    let mut state = create_test_resume_state("TEST-XYZ");

    // Initially 0%
    assert_eq!(state.progress_percentage(), 0.0);

    // Mark half the bytes as received
    let half_bytes = state.total_bytes / 2;
    state.bytes_received = half_bytes;
    let progress = state.progress_percentage();
    assert!(progress > 49.0 && progress < 51.0, "Progress should be ~50%");

    // Mark all bytes as received
    state.bytes_received = state.total_bytes;
    assert!((state.progress_percentage() - 100.0).abs() < 0.01);
}

/// Test empty transfer progress.
#[test]
fn test_resume_state_empty_transfer() {
    let state = ResumeState::new(
        Uuid::new_v4(),
        "EMPTY",
        vec![],
        "TestDevice",
        Uuid::new_v4(),
        PathBuf::from("/tmp"),
    );

    // Empty transfer should be 100% complete
    assert_eq!(state.progress_percentage(), 100.0);
    assert!(state.is_transfer_completed());
}

// ============================================================================
// Resume Manager Tests
// ============================================================================

/// Test that ResumeManager can save and load states.
#[tokio::test]
async fn test_resume_manager_persistence() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let manager = ResumeManager::with_dir(temp_dir.path().to_path_buf())
        .await
        .expect("create manager");

    let mut state = create_test_resume_state("PERSIST-1");
    state.mark_chunk_completed(0, 0, 1024);
    state.mark_chunk_completed(0, 1, 1024);
    let transfer_id = state.transfer_id;

    // Save
    manager.save(&state).await.expect("save");

    // Load and verify
    let loaded = manager
        .load(&transfer_id)
        .await
        .expect("load")
        .expect("should exist");

    assert_eq!(loaded.bytes_received, 2048);
    assert_eq!(loaded.get_completed_chunks(0).len(), 2);
}

/// Test cleanup of expired resume states.
#[tokio::test]
async fn test_resume_manager_cleanup_expired() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let manager = ResumeManager::with_dir(temp_dir.path().to_path_buf())
        .await
        .expect("create manager");

    // Create an old state by manipulating updated_at
    let mut old_state = create_test_resume_state("OLD-STATE");
    old_state.updated_at = chrono::Utc::now() - chrono::Duration::days(10);
    manager.save(&old_state).await.expect("save old");

    // Create a recent state
    let recent_state = create_test_resume_state("RECENT-STATE");
    manager.save(&recent_state).await.expect("save recent");

    // Run cleanup
    let cleaned = manager.cleanup_expired().await.expect("cleanup");

    // Old state should be cleaned up
    assert_eq!(cleaned, 1);

    // Recent state should still exist
    let states = manager.list().await.expect("list");
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].code, "RECENT-STATE");
}

/// Test listing multiple resume states.
#[tokio::test]
async fn test_resume_manager_list_ordering() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let manager = ResumeManager::with_dir(temp_dir.path().to_path_buf())
        .await
        .expect("create manager");

    // Create states with different update times
    let mut state1 = create_test_resume_state("FIRST");
    state1.updated_at = chrono::Utc::now() - chrono::Duration::hours(2);

    let mut state2 = create_test_resume_state("SECOND");
    state2.updated_at = chrono::Utc::now() - chrono::Duration::hours(1);

    let state3 = create_test_resume_state("THIRD"); // Most recent

    manager.save(&state1).await.expect("save 1");
    manager.save(&state2).await.expect("save 2");
    manager.save(&state3).await.expect("save 3");

    // List should be sorted by updated_at descending
    let states = manager.list().await.expect("list");
    assert_eq!(states.len(), 3);
    assert_eq!(states[0].code, "THIRD");
    assert_eq!(states[1].code, "SECOND");
    assert_eq!(states[2].code, "FIRST");
}

/// Test finding by code with multiple states.
#[tokio::test]
async fn test_resume_manager_find_by_code_multiple() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let manager = ResumeManager::with_dir(temp_dir.path().to_path_buf())
        .await
        .expect("create manager");

    // Save multiple states
    let state1 = create_test_resume_state("AAA-111");
    let state2 = create_test_resume_state("BBB-222");
    let state3 = create_test_resume_state("CCC-333");

    manager.save(&state1).await.expect("save 1");
    manager.save(&state2).await.expect("save 2");
    manager.save(&state3).await.expect("save 3");

    // Find middle one
    let found = manager
        .find_by_code("BBB-222")
        .await
        .expect("find")
        .expect("should exist");

    assert_eq!(found.transfer_id, state2.transfer_id);
}

/// Test deleting a non-existent state (should not error).
#[tokio::test]
async fn test_resume_manager_delete_nonexistent() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let manager = ResumeManager::with_dir(temp_dir.path().to_path_buf())
        .await
        .expect("create manager");

    // Deleting non-existent state should succeed silently
    let result = manager.delete(&Uuid::new_v4()).await;
    assert!(result.is_ok());
}

// ============================================================================
// FileWriter Resumable Tests
// ============================================================================

/// Test resumable FileWriter creation and offset writing.
#[tokio::test]
async fn test_file_writer_resumable() {
    use localdrop_core::file::FileWriter;

    let temp_dir = TempDir::new().expect("create temp dir");
    let file_path = temp_dir.path().join("test_file.bin");

    // Create initial data
    let chunk_size = 1024;
    let total_size = chunk_size * 3;

    // First, create a normal writer and write first chunk
    {
        let mut writer = FileWriter::new(file_path.clone(), total_size as u64)
            .await
            .expect("create writer");

        let chunk0 = create_test_chunk(0, chunk_size);
        writer.write_chunk(&chunk0).await.expect("write chunk 0");

        writer.finalize().await.expect("finalize");
    }

    // Now "resume" from chunk 1
    {
        let mut writer = FileWriter::new_resumable(
            file_path.clone(),
            total_size as u64,
            chunk_size as u64, // Already wrote chunk 0
        )
        .await
        .expect("create resumable writer");

        let chunk1 = create_test_chunk(1, chunk_size);
        writer
            .write_chunk_at(&chunk1, chunk_size as u64)
            .await
            .expect("write chunk 1");

        let chunk2 = create_test_chunk(2, chunk_size);
        writer
            .write_chunk_at(&chunk2, (chunk_size * 2) as u64)
            .await
            .expect("write chunk 2");

        let hash = writer.finalize_with_full_hash().await.expect("finalize");
        assert_ne!(hash, [0u8; 32], "Hash should not be zeros");
    }

    // Verify the file exists and has correct size
    let metadata = std::fs::metadata(&file_path).expect("get metadata");
    assert_eq!(metadata.len(), total_size as u64);
}

/// Test out-of-order chunk writing.
#[tokio::test]
async fn test_file_writer_out_of_order_chunks() {
    use localdrop_core::file::FileWriter;

    let temp_dir = TempDir::new().expect("create temp dir");
    let file_path = temp_dir.path().join("ooo_file.bin");

    let chunk_size = 512;
    let total_size = chunk_size * 4;

    let mut writer = FileWriter::new_resumable(file_path.clone(), total_size as u64, 0)
        .await
        .expect("create writer");

    // Write chunks out of order: 3, 1, 0, 2
    let chunks = vec![
        (3, create_test_chunk(3, chunk_size)),
        (1, create_test_chunk(1, chunk_size)),
        (0, create_test_chunk(0, chunk_size)),
        (2, create_test_chunk(2, chunk_size)),
    ];

    for (index, chunk) in chunks {
        let offset = index as u64 * chunk_size as u64;
        writer
            .write_chunk_at(&chunk, offset)
            .await
            .expect("write chunk");
    }

    let _hash = writer.finalize_with_full_hash().await.expect("finalize");

    // Verify file size
    let metadata = std::fs::metadata(&file_path).expect("get metadata");
    assert_eq!(metadata.len(), total_size as u64);
}

// ============================================================================
// Helper Functions
// ============================================================================

fn create_test_resume_state(code: &str) -> ResumeState {
    let files = vec![
        FileMetadata {
            relative_path: PathBuf::from("file1.txt"),
            size: 10240,
            mime_type: Some("text/plain".to_string()),
            created: None,
            modified: None,
            permissions: None,
            is_symlink: false,
            symlink_target: None,
        },
        FileMetadata {
            relative_path: PathBuf::from("file2.bin"),
            size: 20480,
            mime_type: Some("application/octet-stream".to_string()),
            created: None,
            modified: None,
            permissions: None,
            is_symlink: false,
            symlink_target: None,
        },
    ];

    ResumeState::new(
        Uuid::new_v4(),
        code,
        files,
        "TestSender",
        Uuid::new_v4(),
        PathBuf::from("/tmp/test_output"),
    )
}

fn create_test_chunk(index: usize, size: usize) -> localdrop_core::file::FileChunk {
    let data: Vec<u8> = (0..size).map(|i| ((i + index) % 256) as u8).collect();
    let checksum = localdrop_core::crypto::xxhash64(&data);

    localdrop_core::file::FileChunk {
        file_index: 0,
        chunk_index: index as u64,
        data,
        checksum,
        is_last: false,
    }
}
