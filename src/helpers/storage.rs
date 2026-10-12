use std::collections::HashMap;
use std::io;
use std::path::Path;

use crate::config;

pub fn load_statuses() -> HashMap<String, String> {
    load_from(Path::new(config::STATUS_FILE))
}

pub async fn save_statuses(statuses: &HashMap<String, String>) -> io::Result<()> {
    save_to(Path::new(config::STATUS_FILE), statuses).await
}

/// Loads the status map. A missing file is normal; a corrupt file is backed up
/// (`<file>.corrupt`) instead of being silently replaced by an empty map.
pub fn load_from(path: &Path) -> HashMap<String, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return HashMap::new(),
        Err(e) => {
            log::error!("Could not read {}: {}", path.display(), e);
            return HashMap::new();
        }
    };

    match serde_json::from_str(&raw) {
        Ok(map) => map,
        Err(e) => {
            let backup = path.with_extension("json.corrupt");
            log::error!(
                "{} is not valid JSON ({}); keeping a copy at {} and starting empty.",
                path.display(),
                e,
                backup.display()
            );
            if let Err(copy_err) = std::fs::copy(path, &backup) {
                log::error!("Could not back up corrupt status file: {}", copy_err);
            }
            HashMap::new()
        }
    }
}

/// Atomic save: write a temp file next to the target, then rename over it,
/// so a crash can never leave a half-written status file.
pub async fn save_to(path: &Path, statuses: &HashMap<String, String>) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(statuses).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, &json).await?;
    tokio::fs::rename(&tmp, path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("clover_bot_test_{}_{}", std::process::id(), name))
    }

    #[tokio::test]
    async fn roundtrip_and_missing_file() {
        let path = temp_path("status.json");
        let _ = std::fs::remove_file(&path);
        assert!(load_from(&path).is_empty());

        let mut map = HashMap::new();
        map.insert("fog".to_string(), "Inactive".to_string());
        save_to(&path, &map).await.unwrap();
        assert_eq!(load_from(&path), map);
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_is_backed_up_not_lost() {
        let path = temp_path("corrupt.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_from(&path).is_empty());
        let backup = path.with_extension("json.corrupt");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "{ not json");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);
    }
}
