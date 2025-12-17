use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StabilityConfig {
    #[serde(default = "StabilityConfig::default_checks")]
    pub consecutive_checks: u32,
    #[serde(default = "StabilityConfig::default_interval_ms")]
    pub interval_ms: u64,
    #[serde(default = "StabilityConfig::default_timeout_secs")]
    pub timeout_secs: u64,
}

impl StabilityConfig {
    pub fn default_checks() -> u32 {
        3
    }
    pub fn default_interval_ms() -> u64 {
        700
    }
    pub fn default_timeout_secs() -> u64 {
        120
    }
}

impl Default for StabilityConfig {
    fn default() -> Self {
        Self {
            consecutive_checks: Self::default_checks(),
            interval_ms: Self::default_interval_ms(),
            timeout_secs: Self::default_timeout_secs(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub downloads_dir: PathBuf,
    pub songs_dir: PathBuf,
    pub auto_import: bool,
    #[serde(default)]
    pub stability: StabilityConfig,
    #[serde(default)]
    pub auto_delete_source: bool,
    #[serde(default)]
    pub suppress_delete_prompt: bool,
    #[serde(default)]
    pub last_link: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        let downloads = directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let songs = downloads.join("McOsuSongs");
        Self {
            downloads_dir: downloads,
            songs_dir: songs,
            auto_import: false,
            stability: StabilityConfig::default(),
            auto_delete_source: false,
            suppress_delete_prompt: false,
            last_link: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ImportStatus {
    Detected,
    WaitingStable,
    ReadingMetadata,
    Importing,
    Completed,
    DuplicateSkipped,
    Failed,
}

impl ImportStatus {
    pub fn as_display(&self) -> &'static str {
        match self {
            ImportStatus::Detected => "Detectado",
            ImportStatus::WaitingStable => "Aguardando",
            ImportStatus::ReadingMetadata => "Metadados",
            ImportStatus::Importing => "Importando",
            ImportStatus::Completed => "Concluido",
            ImportStatus::DuplicateSkipped => "Duplicado",
            ImportStatus::Failed => "Falhou",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BeatmapMetadata {
    pub title: String,
    pub artist: String,
    pub creator: String,
    pub difficulties: Vec<String>,
    pub beatmap_set_id: Option<i32>,
    pub beatmap_ids: Vec<i32>,
    pub background_file: Option<String>,
    #[serde(default)]
    pub audio_file: Option<String>,
}

impl BeatmapMetadata {
    pub fn display_title(&self) -> String {
        format!("{} - {}", self.artist, self.title)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BeatmapEntry {
    pub id: u64,
    pub osz_path: PathBuf,
    pub status: ImportStatus,
    pub message: Option<String>,
    pub error_detail: Option<String>,
    pub error_short: Option<String>,
    pub metadata: Option<BeatmapMetadata>,
    pub thumbnail_path: Option<PathBuf>,
    pub detected_at: SystemTime,
    pub destination: Option<PathBuf>,
    pub osz_hash: Option<String>,
    #[serde(default)]
    pub audio: AudioPreview,
}

impl BeatmapEntry {
    pub fn source_file_name(&self) -> String {
        self.osz_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AudioPreviewStatus {
    Unknown,
    Loading,
    Ready,
    Playing,
    Paused,
    Unavailable,
}

impl Default for AudioPreviewStatus {
    fn default() -> Self {
        AudioPreviewStatus::Unknown
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioPreview {
    #[serde(default)]
    pub status: AudioPreviewStatus,
    #[serde(default)]
    pub cached_path: Option<PathBuf>,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl Default for AudioPreview {
    fn default() -> Self {
        Self {
            status: AudioPreviewStatus::Unknown,
            cached_path: None,
            last_error: None,
        }
    }
}

pub fn sanitize_path_component(name: &str) -> String {
    let mut cleaned = name
        .chars()
        .map(|c| match c {
            ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\\' | '/' => '_',
            _ => c,
        })
        .collect::<String>();
    cleaned.truncate(100);
    cleaned.trim().trim_matches('.').trim().to_string()
}

pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_path_component_replaces_illegal_chars() {
        let name = sanitize_path_component("Artist:Title?*<>|/\\");
        assert!(name.contains('_'));
        assert!(!name.contains(':'));
        assert!(!name.contains('?'));
    }
}
