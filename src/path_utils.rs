use std::path::{Component, Path, PathBuf};

/// Normalize a path by removing `.` and resolving `..` components without hitting the filesystem.
fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) => normalized.push(comp.as_os_str()),
            Component::Normal(s) => normalized.push(s),
        }
    }
    if normalized.as_os_str().is_empty() {
        path.to_path_buf()
    } else {
        normalized
    }
}

/// Returns true if `candidate` is inside (or equal to) `base`.
pub fn is_within_dir(base: &Path, candidate: &Path) -> bool {
    let base_norm = normalize_path(base);
    let cand_norm = normalize_path(candidate);
    cand_norm.starts_with(&base_norm)
}

/// Detects unsafe overlap between the downloads and songs folders.
/// Returns a warning string when Songs is inside Downloads or both folders are the same.
pub fn downloads_songs_conflict(downloads: &Path, songs: &Path) -> Option<String> {
    let dl_norm = normalize_path(downloads);
    let songs_norm = normalize_path(songs);
    let guidance = "Escolha uma pasta Songs diferente via Steam > McOsu > Gerenciar > Procurar arquivos locais.";
    if dl_norm == songs_norm {
        Some(format!(
            "Downloads e Songs apontam para a mesma pasta. {}",
            guidance
        ))
    } else if songs_norm.starts_with(&dl_norm) {
        Some(format!(
            "A pasta Songs esta dentro da pasta de Downloads. {}",
            guidance
        ))
    } else if dl_norm.starts_with(&songs_norm) {
        Some(format!(
            "A pasta de Downloads esta dentro da pasta Songs; se mover ou limpar, voce perde mapas. {}",
            guidance
        ))
    } else {
        None
    }
}

/// Returns an error string when the songs directory is inside (or equal to) the downloads dir.
pub fn validate_songs_choice(downloads: &Path, songs: &Path) -> Result<(), String> {
    if let Some(msg) = downloads_songs_conflict(downloads, songs) {
        Err(msg)
    } else {
        Ok(())
    }
}

/// Allow deleting the source only when it is inside Downloads and there is no overlap with Songs.
pub fn can_delete_source(downloads: &Path, songs: &Path, source: &Path) -> bool {
    if downloads_songs_conflict(downloads, songs).is_some() {
        return false;
    }
    is_within_dir(downloads, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn delete_allowed_only_within_downloads() {
        let downloads = PathBuf::from("/home/user/Downloads");
        let songs = PathBuf::from("/home/user/McOsu/Songs");
        let inside = PathBuf::from("/home/user/Downloads/map.osz");
        let outside = PathBuf::from("/home/user/Desktop/map.osz");

        assert!(can_delete_source(&downloads, &songs, &inside));
        assert!(!can_delete_source(&downloads, &songs, &outside));

        // Conflict disables deletion even if under downloads
        let songs_inside = PathBuf::from("/home/user/Downloads/Songs");
        assert!(!can_delete_source(&downloads, &songs_inside, &inside));
    }

    #[test]
    fn conflict_detects_equal_and_nested() {
        let downloads = PathBuf::from("/tmp/dl");
        let same = PathBuf::from("/tmp/dl");
        let nested = PathBuf::from("/tmp/dl/Songs");
        let separate = PathBuf::from("/games/McOsu/Songs");

        let equal = downloads_songs_conflict(&downloads, &same).unwrap();
        assert!(equal.contains("Downloads e Songs"));
        let nested_msg = downloads_songs_conflict(&downloads, &nested).unwrap();
        assert!(nested_msg.contains("Songs esta dentro"));
        assert!(downloads_songs_conflict(&downloads, &separate).is_none());
    }

    #[test]
    fn validate_songs_choice_blocks_overlap() {
        let downloads = PathBuf::from("/tmp/dl");
        let nested = PathBuf::from("/tmp/dl/Songs");
        let separate = PathBuf::from("/games/McOsu/Songs");

        assert!(validate_songs_choice(&downloads, &nested).is_err());
        assert!(validate_songs_choice(&downloads, &downloads).is_err());
        assert!(validate_songs_choice(&downloads, &separate).is_ok());
    }
}
