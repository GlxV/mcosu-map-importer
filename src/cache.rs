use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::app_state::AppConfig;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CacheData {
    pub thumbnails: HashMap<String, PathBuf>,
    pub beatmap_sets: HashMap<i32, PathBuf>,
    pub osz_hashes: HashMap<String, PathBuf>,
    #[serde(default)]
    pub audio_files: HashMap<String, PathBuf>,
}

#[derive(Debug)]
pub struct CacheStore {
    inner: Mutex<CacheData>,
}

impl CacheStore {
    pub fn load() -> Self {
        let _ = std::fs::create_dir_all(cache_dir());
        migrate_legacy_files().ok();
        let data = fs::read_to_string(cache_path()).ok();
        let inner = data
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Mutex::new(inner),
        }
    }

    pub fn get_thumbnail(&self, key: &str) -> Option<PathBuf> {
        self.inner.lock().ok()?.thumbnails.get(key).cloned()
    }

    pub fn insert_thumbnail(&self, key: String, path: PathBuf) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.thumbnails.insert(key, path);
        }
    }

    pub fn register_beatmap_set(&self, set_id: i32, path: PathBuf) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.beatmap_sets.insert(set_id, path);
        }
    }

    pub fn register_hash(&self, hash: String, path: PathBuf) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.osz_hashes.insert(hash, path);
        }
    }

    pub fn register_audio(&self, hash: String, path: PathBuf) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.audio_files.insert(hash, path);
        }
    }

    pub fn find_set(&self, set_id: i32) -> Option<PathBuf> {
        self.inner.lock().ok()?.beatmap_sets.get(&set_id).cloned()
    }

    pub fn find_hash(&self, hash: &str) -> Option<PathBuf> {
        self.inner.lock().ok()?.osz_hashes.get(hash).cloned()
    }

    pub fn find_audio(&self, hash: &str) -> Option<PathBuf> {
        self.inner.lock().ok()?.audio_files.get(hash).cloned()
    }

    pub fn save(&self) -> Result<()> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| anyhow!("cache lock poisoned: {e}"))?;
        let json = serde_json::to_string_pretty(&*guard)?;
        fs::write(cache_path(), json)?;
        Ok(())
    }
}

pub fn load_config() -> AppConfig {
    migrate_legacy_files().ok();
    let data = fs::read_to_string(config_path()).ok();
    data.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(cfg: &AppConfig) -> Result<()> {
    let json = serde_json::to_string_pretty(cfg)?;
    if let Some(dir) = config_path().parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(config_path(), json).context("failed to write config.json")?;
    Ok(())
}

pub fn base_dir() -> PathBuf {
    let proj = ProjectDirs::from("dev", "mcosu", "mcosu-importer");
    proj.map(|p| p.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn cache_dir() -> PathBuf {
    base_dir().join("cache")
}

pub fn thumbnails_dir() -> PathBuf {
    cache_dir().join("thumbnails")
}

pub fn audio_cache_dir() -> PathBuf {
    cache_dir().join("audio")
}

pub fn preview_dir() -> PathBuf {
    cache_dir().join("preview")
}

pub fn logs_dir() -> PathBuf {
    base_dir().join("logs")
}

fn config_path() -> PathBuf {
    base_dir().join("config.json")
}

fn cache_path() -> PathBuf {
    cache_dir().join("cache.json")
}

fn migrate_legacy_files() -> Result<()> {
    let legacy_config = PathBuf::from("config.json");
    let legacy_cache = PathBuf::from("cache.json");
    if legacy_config.exists() && !config_path().exists() {
        fs::create_dir_all(
            config_path()
                .parent()
                .ok_or_else(|| anyhow!("config parent missing"))?,
        )?;
        fs::copy(&legacy_config, config_path())?;
        warn!("config.json encontrado na pasta atual; migrando para diretório de dados");
    }
    if legacy_cache.exists() && !cache_path().exists() {
        fs::create_dir_all(cache_dir())?;
        fs::copy(&legacy_cache, cache_path())?;
        warn!("cache.json encontrado na pasta atual; migrando para diretório de dados");
    }
    Ok(())
}
