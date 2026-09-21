use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FileStoreError {
    #[error("could not create lyrics directory: {0}")]
    CreateDirectory(#[source] io::Error),
    #[error("could not write temporary lyrics file: {0}")]
    Write(#[source] io::Error),
    #[error("could not replace lyrics file: {0}")]
    Replace(#[source] io::Error),
}

pub fn write_atomic(path: impl AsRef<Path>, contents: &str) -> Result<(), FileStoreError> {
    let path = path.as_ref();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(FileStoreError::CreateDirectory)?;

    let temporary_path = temporary_path(path);
    fs::write(&temporary_path, contents).map_err(FileStoreError::Write)?;
    replace_file(&temporary_path, path)
}

fn replace_file(temporary_path: &Path, destination: &Path) -> Result<(), FileStoreError> {
    if let Err(error) = fs::rename(temporary_path, destination) {
        if error.kind() != io::ErrorKind::AlreadyExists {
            let _ = fs::remove_file(temporary_path);
            return Err(FileStoreError::Replace(error));
        }
        fs::remove_file(destination).map_err(FileStoreError::Replace)?;
        fs::rename(temporary_path, destination).map_err(FileStoreError::Replace)?;
    }
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("lyrics");
    path.with_file_name(format!(
        ".{file_name}.{timestamp}.{}.tmp",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing_file_without_leaving_temp_files() {
        let root = std::env::temp_dir().join(format!("synced-lyrics-fs-{}", std::process::id()));
        let destination = root.join("song.lrc");
        write_atomic(&destination, "[00:01]new").unwrap();
        write_atomic(&destination, "[00:02]updated").unwrap();

        assert_eq!(fs::read_to_string(destination).unwrap(), "[00:02]updated");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
