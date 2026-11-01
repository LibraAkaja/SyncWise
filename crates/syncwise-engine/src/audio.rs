use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use lofty::{
    file::{AudioFile, TaggedFileExt},
    probe::Probe,
    tag::ItemKey,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{SyncError, SyncErrorCode};

const HASH_CHUNK_SIZE: usize = 1024 * 1024;
const PARTIAL_SAMPLE_SIZE: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct AudioMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
}

pub(crate) fn extract_audio_metadata(path: &Path) -> Option<AudioMetadata> {
    let tagged_file = Probe::open(path).ok()?.read().ok()?;
    let tag = tagged_file.primary_tag();
    let read = |key| {
        tag.and_then(|tag| tag.get_string(&key))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let duration = tagged_file.properties().duration();
    let duration_ms =
        (!duration.is_zero()).then(|| duration.as_millis().min(u64::MAX as u128) as u64);
    let metadata = AudioMetadata {
        title: read(ItemKey::TrackTitle),
        artist: read(ItemKey::TrackArtist),
        album_artist: read(ItemKey::AlbumArtist),
        album: read(ItemKey::AlbumTitle),
        duration_ms,
    };
    (metadata.title.is_some()
        || metadata.artist.is_some()
        || metadata.album_artist.is_some()
        || metadata.album.is_some()
        || metadata.duration_ms.is_some())
    .then_some(metadata)
}

pub fn partial_hash_file(path: &Path, cancellation: &AtomicBool) -> Result<[u8; 32], SyncError> {
    check_cancelled(cancellation)?;
    let mut file = File::open(path).map_err(|error| {
        SyncError::new(
            SyncErrorCode::IoError,
            &format!("Could not open a file for partial hashing: {error}"),
        )
    })?;
    let length = file
        .metadata()
        .map_err(|error| {
            SyncError::new(
                SyncErrorCode::IoError,
                &format!("Could not inspect a file for partial hashing: {error}"),
            )
        })?
        .len();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&length.to_le_bytes());
    let mut buffer = vec![0; HASH_CHUNK_SIZE.min(PARTIAL_SAMPLE_SIZE as usize)];
    let first_length = length.min(PARTIAL_SAMPLE_SIZE) as usize;
    read_exact_cancellable(&mut file, &mut buffer[..first_length], cancellation)?;
    hasher.update(&buffer[..first_length]);
    if length > PARTIAL_SAMPLE_SIZE {
        check_cancelled(cancellation)?;
        file.seek(SeekFrom::Start(length - PARTIAL_SAMPLE_SIZE))
            .map_err(|error| {
                SyncError::new(
                    SyncErrorCode::IoError,
                    &format!("Could not seek while partially hashing a file: {error}"),
                )
            })?;
        read_exact_cancellable(
            &mut file,
            &mut buffer[..PARTIAL_SAMPLE_SIZE as usize],
            cancellation,
        )?;
        hasher.update(&buffer[..PARTIAL_SAMPLE_SIZE as usize]);
    }
    Ok(*hasher.finalize().as_bytes())
}

pub fn full_hash_file(path: &Path, cancellation: &AtomicBool) -> Result<[u8; 32], SyncError> {
    check_cancelled(cancellation)?;
    let mut file = File::open(path).map_err(|error| {
        SyncError::new(
            SyncErrorCode::IoError,
            &format!("Could not open a file for full hashing: {error}"),
        )
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; HASH_CHUNK_SIZE];
    loop {
        check_cancelled(cancellation)?;
        let count = file.read(&mut buffer).map_err(|error| {
            SyncError::new(
                SyncErrorCode::IoError,
                &format!("Could not read a file for full hashing: {error}"),
            )
        })?;
        if count == 0 {
            return Ok(*hasher.finalize().as_bytes());
        }
        hasher.update(&buffer[..count]);
    }
}

pub fn verify_file_copy(
    source: &Path,
    copy: &Path,
    cancellation: &AtomicBool,
) -> Result<bool, SyncError> {
    Ok(full_hash_file(source, cancellation)? == full_hash_file(copy, cancellation)?)
}

fn read_exact_cancellable(
    file: &mut File,
    mut buffer: &mut [u8],
    cancellation: &AtomicBool,
) -> Result<(), SyncError> {
    while !buffer.is_empty() {
        check_cancelled(cancellation)?;
        let count = file.read(buffer).map_err(|error| {
            SyncError::new(
                SyncErrorCode::IoError,
                &format!("Could not read a file for partial hashing: {error}"),
            )
        })?;
        if count == 0 {
            return Err(SyncError::new(
                SyncErrorCode::IoError,
                "The file changed while it was being partially hashed.",
            ));
        }
        buffer = &mut buffer[count..];
    }
    Ok(())
}

fn check_cancelled(cancellation: &AtomicBool) -> Result<(), SyncError> {
    if cancellation.load(Ordering::Relaxed) {
        Err(SyncError::new(
            SyncErrorCode::OperationCancelled,
            "Content hashing was cancelled.",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::atomic::AtomicBool};

    #[test]
    fn malformed_and_untagged_audio_falls_back_without_failing_scan() {
        let directory = tempfile::tempdir().unwrap();
        let malformed = directory.path().join("broken.mp3");
        fs::write(&malformed, b"not an audio stream").unwrap();
        assert_eq!(extract_audio_metadata(&malformed), None);
        assert_eq!(
            extract_audio_metadata(&directory.path().join("missing.flac")),
            None
        );
        let scan =
            crate::scan_directory(directory.path(), &AtomicBool::new(false), |_| {}).unwrap();
        assert!(scan.issues.is_empty());
        assert_eq!(scan.files[0].audio_metadata, None);
    }

    #[test]
    fn scan_extracts_basic_id3_metadata_and_duration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tagged.mp3");
        let mut frames = Vec::new();
        for (frame_id, value) in [
            (b"TIT2", "Example Song"),
            (b"TPE1", "Example Artist"),
            (b"TALB", "Example Album"),
        ] {
            let mut payload = vec![0];
            payload.extend_from_slice(value.as_bytes());
            frames.extend_from_slice(frame_id);
            frames.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            frames.extend_from_slice(&[0, 0]);
            frames.extend_from_slice(&payload);
        }
        let length = frames.len();
        let mut mp3 = b"ID3\x03\x00\x00".to_vec();
        mp3.extend_from_slice(&[
            ((length >> 21) & 0x7f) as u8,
            ((length >> 14) & 0x7f) as u8,
            ((length >> 7) & 0x7f) as u8,
            (length & 0x7f) as u8,
        ]);
        mp3.extend_from_slice(&frames);
        let mut mpeg_frame = vec![0; 417];
        mpeg_frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
        mp3.extend_from_slice(&mpeg_frame);
        mp3.extend_from_slice(&mpeg_frame);
        fs::write(&path, mp3).unwrap();

        Probe::open(&path).unwrap().read().unwrap();
        let scan =
            crate::scan_directory(directory.path(), &AtomicBool::new(false), |_| {}).unwrap();
        let metadata = scan.files[0].audio_metadata.as_ref().unwrap();
        assert_eq!(metadata.title.as_deref(), Some("Example Song"));
        assert_eq!(metadata.artist.as_deref(), Some("Example Artist"));
        assert_eq!(metadata.album.as_deref(), Some("Example Album"));
        assert!(metadata.duration_ms.is_some());
    }

    #[test]
    fn staged_hashes_distinguish_same_size_files_and_verify_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.bin");
        let equal = directory.path().join("equal.bin");
        let corrupt = directory.path().join("corrupt.bin");
        fs::write(&source, vec![7; 160_000]).unwrap();
        fs::copy(&source, &equal).unwrap();
        let mut bytes = fs::read(&source).unwrap();
        bytes[80_000] = 8;
        fs::write(&corrupt, bytes).unwrap();
        let cancellation = AtomicBool::new(false);
        assert_eq!(
            partial_hash_file(&source, &cancellation).unwrap(),
            partial_hash_file(&equal, &cancellation).unwrap()
        );
        assert_eq!(
            full_hash_file(&source, &cancellation).unwrap(),
            full_hash_file(&equal, &cancellation).unwrap()
        );
        assert!(!verify_file_copy(&source, &corrupt, &cancellation).unwrap());
        assert!(verify_file_copy(&source, &equal, &cancellation).unwrap());
    }

    #[test]
    fn partial_hash_covers_small_files_and_size() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::write(&first, b"abc").unwrap();
        fs::write(&second, b"abcd").unwrap();
        let cancellation = AtomicBool::new(false);
        assert_ne!(
            partial_hash_file(&first, &cancellation).unwrap(),
            partial_hash_file(&second, &cancellation).unwrap()
        );
    }
}
