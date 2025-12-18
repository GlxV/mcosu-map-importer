use std::fs::{self, File};
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::app_state::sanitize_path_component;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DownloadProvider {
    Gatari,
    BeatConnect,
}

impl DownloadProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            DownloadProvider::Gatari => "Gatari",
            DownloadProvider::BeatConnect => "BeatConnect",
        }
    }

    pub fn from_index(idx: i32) -> Self {
        match idx {
            1 => DownloadProvider::BeatConnect,
            _ => DownloadProvider::Gatari,
        }
    }

    pub fn to_index(&self) -> i32 {
        match self {
            DownloadProvider::Gatari => 0,
            DownloadProvider::BeatConnect => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DownloadStatus {
    Pending,
    Resolving,
    Downloading,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadStatus {
    pub fn as_display(&self) -> &'static str {
        match self {
            DownloadStatus::Pending => "Na fila",
            DownloadStatus::Resolving => "Preparando",
            DownloadStatus::Downloading => "Baixando",
            DownloadStatus::Completed => "Concluido",
            DownloadStatus::Failed => "Falhou",
            DownloadStatus::Cancelled => "Cancelado",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadJob {
    pub id: u64,
    pub provider: DownloadProvider,
    pub input: String,
    pub final_url: Option<String>,
    pub status: DownloadStatus,
    pub progress_bytes: u64,
    pub total_bytes_opt: Option<u64>,
    pub error_opt: Option<String>,
    pub out_path_opt: Option<PathBuf>,
    #[serde(skip)]
    pub cancel_flag: Arc<AtomicBool>,
}

impl DownloadJob {
    pub fn new(id: u64, provider: DownloadProvider, input: String) -> Self {
        Self {
            id,
            provider,
            input,
            final_url: None,
            status: DownloadStatus::Pending,
            progress_bytes: 0,
            total_bytes_opt: None,
            error_opt: None,
            out_path_opt: None,
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn title(&self) -> String {
        let provider = self.provider.as_str();
        if let Some(url) = self.final_url.as_ref() {
            format!("{provider} {}", shorten_middle(url, 42))
        } else {
            format!("{provider} {}", shorten_middle(&self.input, 42))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedDownload {
    pub url: String,
    pub set_id: Option<String>,
}

pub fn resolve_download(provider: DownloadProvider, input: &str) -> Result<ResolvedDownload> {
    match provider {
        DownloadProvider::Gatari => parse_gatari_input(input),
        DownloadProvider::BeatConnect => parse_beatconnect_input(input),
    }
}

pub fn parse_gatari_input(input: &str) -> Result<ResolvedDownload> {
    let trimmed = input.trim();
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Ok(ResolvedDownload {
            url: format!("https://osu.gatari.pw/d/{trimmed}"),
            set_id: Some(trimmed.to_string()),
        });
    }
    let url_like = prepend_scheme_if_missing(trimmed);
    let re_direct = Regex::new(r"/d/(\d+)")?;
    if let Some(cap) = re_direct.captures(&url_like) {
        let id = cap.get(1).unwrap().as_str();
        return Ok(ResolvedDownload {
            url: format!("https://osu.gatari.pw/d/{id}"),
            set_id: Some(id.to_string()),
        });
    }
    let re_generic = Regex::new(r"/(?:s|beatmapsets)/(\d+)")?;
    if let Some(cap) = re_generic.captures(&url_like) {
        let id = cap.get(1).unwrap().as_str();
        return Ok(ResolvedDownload {
            url: format!("https://osu.gatari.pw/d/{id}"),
            set_id: Some(id.to_string()),
        });
    }
    let re_digits = Regex::new(r"(\d{2,})")?;
    if let Some(cap) = re_digits.captures(&url_like) {
        let id = cap.get(1).unwrap().as_str();
        return Ok(ResolvedDownload {
            url: format!("https://osu.gatari.pw/d/{id}"),
            set_id: Some(id.to_string()),
        });
    }
    Err(anyhow!(
        "Nao foi possivel extrair o BeatmapSetID do link informado"
    ))
}

pub fn parse_beatconnect_input(input: &str) -> Result<ResolvedDownload> {
    let trimmed = input.trim();
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Ok(ResolvedDownload {
            url: format!("https://beatconnect.io/b/{trimmed}"),
            set_id: Some(trimmed.to_string()),
        });
    }
    let normalized = prepend_scheme_if_missing(trimmed);
    if !normalized.contains("beatconnect.io/b/") {
        bail!("Cole um link do BeatConnect ou informe o ID numerico");
    }
    let re = Regex::new(r"/b/(\d+)")?;
    let set_id = re
        .captures(&normalized)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()));
    Ok(ResolvedDownload {
        url: normalized,
        set_id,
    })
}

fn prepend_scheme_if_missing(text: &str) -> String {
    if text.starts_with("http://") || text.starts_with("https://") {
        text.to_string()
    } else {
        format!("https://{}", text.trim_start_matches("://"))
    }
}

pub fn choose_file_name(
    provider: DownloadProvider,
    set_id: Option<&str>,
    content_disposition: Option<&str>,
) -> String {
    if let Some(raw) = content_disposition.and_then(extract_filename) {
        let sanitized = sanitize_osz_name(&raw);
        if !sanitized.is_empty() {
            return sanitized;
        }
    }
    let fallback_id = set_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| unix_ts().to_string());
    format!(
        "{}_{}.osz",
        provider.as_str().to_lowercase(),
        fallback_id
    )
}

fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sanitize_osz_name(name: &str) -> String {
    let cleaned = sanitize_path_component(name);
    let with_ext = if cleaned.to_lowercase().ends_with(".osz") {
        cleaned
    } else {
        format!("{cleaned}.osz")
    };
    with_ext
}

fn extract_filename(header: &str) -> Option<String> {
    header
        .split(';')
        .find_map(|part| part.trim().strip_prefix("filename="))
        .map(|v| v.trim_matches('"').to_string())
        .or_else(|| {
            header
                .split(';')
                .find_map(|part| part.trim().strip_prefix("filename*="))
                .map(|v| v.split('\'').last().unwrap_or(v).to_string())
        })
}

pub fn unique_path(base_dir: &Path, file_name: &str) -> PathBuf {
    let mut candidate = base_dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = candidate
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let mut counter = 1usize;
    let ext = candidate
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("osz")
        .to_string();
    loop {
        let name = format!("{stem} ({counter}).{ext}");
        candidate = base_dir.join(&name);
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

pub fn download_to_path(
    job: &mut DownloadJob,
    downloads_dir: &Path,
    client: &Client,
    notify: &mut dyn FnMut(&DownloadJob),
) -> Result<()> {
    job.status = DownloadStatus::Resolving;
    notify(job);
    let resolved = resolve_download(job.provider, &job.input)?;
    job.final_url = Some(resolved.url.clone());
    notify(job);
    info!(
        "Iniciando download {} de {}",
        job.id,
        job.final_url.as_deref().unwrap_or_default()
    );

    let mut response = client
        .get(&resolved.url)
        .header(USER_AGENT, "mcosu-importer/1.0")
        .send()
        .with_context(|| format!("Requisicao falhou para {}", resolved.url))?;
    let status = response.status();
    if !status.is_success() {
        bail!("HTTP {} ao baixar {}", status.as_u16(), resolved.url);
    }
    let total = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());
    job.total_bytes_opt = total;
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let disp = response
        .headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok());
    let file_name = choose_file_name(job.provider, resolved.set_id.as_deref(), disp);
    let target = unique_path(downloads_dir, &file_name);
    let part_path = target.with_extension("osz.part");
    job.status = DownloadStatus::Downloading;
    job.out_path_opt = Some(target.clone());
    notify(job);

    let mut file =
        File::create(&part_path).with_context(|| format!("Criando arquivo {}", part_path.display()))?;
    let mut downloaded: u64 = 0;
    let mut buffer = [0u8; 16 * 1024];
    let mut first_chunk = Vec::new();
    loop {
        if job.cancel_flag.load(Ordering::SeqCst) {
            warn!("Download {} cancelado pelo usuario", job.id);
            job.status = DownloadStatus::Cancelled;
            job.error_opt = None;
            notify(job);
            let _ = fs::remove_file(&part_path);
            return Ok(());
        }
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if downloaded == 0 {
            first_chunk.extend_from_slice(&buffer[..read.min(512)]);
            if looks_like_html(&first_chunk, content_type) {
                let _ = fs::remove_file(&part_path);
                bail!(beatconnect_html_error(job.provider));
            }
        }
        file.write_all(&buffer[..read])?;
        downloaded += read as u64;
        job.progress_bytes = downloaded;
        notify(job);
    }
    file.flush()?;
    fs::rename(&part_path, &target)?;
    job.status = DownloadStatus::Completed;
    job.progress_bytes = downloaded;
    job.out_path_opt = Some(target.clone());
    notify(job);
    info!(
        "Download {} concluido em {} ({} bytes)",
        job.id,
        target.display(),
        downloaded
    );
    Ok(())
}

fn looks_like_html(snippet: &[u8], content_type: &str) -> bool {
    if content_type.to_lowercase().contains("text/html") {
        return true;
    }
    let start = String::from_utf8_lossy(snippet).to_lowercase();
    start.contains("<html") || start.contains("<!doctype html")
}

fn beatconnect_html_error(provider: DownloadProvider) -> anyhow::Error {
    if matches!(provider, DownloadProvider::BeatConnect) {
        anyhow!("Resposta nao parece um .osz. No BeatConnect, cole o link completo de download (/b/<id>/<token>/) se o ID sozinho falhar.")
    } else {
        anyhow!("Resposta nao parece um .osz.")
    }
}

fn shorten_middle(text: &str, max_len: usize) -> String {
    let len = text.chars().count();
    if len <= max_len {
        return text.to_string();
    }
    let head = (max_len.saturating_sub(3)) / 2;
    let tail = max_len.saturating_sub(3) - head;
    let start = text.chars().take(head).collect::<String>();
    let end = text.chars().rev().take(tail).collect::<String>();
    format!("{start}...{}", end.chars().rev().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn parse_gatari_variants() {
        let direct = parse_gatari_input("12345").unwrap();
        assert!(direct.url.contains("/d/12345"));
        let url = parse_gatari_input("https://osu.gatari.pw/s/987").unwrap();
        assert!(url.url.contains("/d/987"));
        let dlink = parse_gatari_input("https://osu.gatari.pw/d/555").unwrap();
        assert!(dlink.url.contains("/d/555"));
    }

    #[test]
    fn parse_beatconnect_variants() {
        let num = parse_beatconnect_input("54321").unwrap();
        assert!(num.url.contains("/b/54321"));
        let full =
            parse_beatconnect_input("https://beatconnect.io/b/777/token/").unwrap();
        assert!(full.url.contains("/b/777/"));
        assert_eq!(full.set_id.unwrap(), "777");
    }

    #[test]
    fn content_disposition_filename_sanitized() {
        let name = choose_file_name(
            DownloadProvider::Gatari,
            Some("1"),
            Some("attachment; filename=\"A*B?.osz\""),
        );
        assert!(name.ends_with(".osz"));
        assert!(!name.contains('*'));
        assert!(!name.contains('?'));
    }

    #[test]
    fn unique_path_adds_suffix() {
        let dir = tempdir().unwrap();
        let first = dir.path().join("file.osz");
        {
            let mut f = File::create(&first).unwrap();
            writeln!(f, "hi").unwrap();
        }
        let second = unique_path(dir.path(), "file.osz");
        assert_ne!(second, first);
        assert!(second.file_name().unwrap().to_string_lossy().contains("(1)"));
    }
}
