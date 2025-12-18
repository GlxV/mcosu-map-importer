use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use zip::ZipArchive;

use crate::app_state::{BeatmapEntry, BeatmapMetadata, sanitize_path_component};

#[derive(Debug)]
pub struct ImportResult {
    pub destination: PathBuf,
    pub duplicated: bool,
}

pub fn import_osz(
    entry: &BeatmapEntry,
    meta: &BeatmapMetadata,
    songs_dir: &Path,
    force: bool,
) -> Result<ImportResult> {
    let target_name = build_folder_name(meta, &entry.osz_path);
    let dest = songs_dir.join(target_name);

    if dest.exists() && !force {
        return Ok(ImportResult {
            destination: dest,
            duplicated: true,
        });
    }
    if dest.exists() && force {
        fs::remove_dir_all(&dest).ok();
    }

    fs::create_dir_all(&dest).context("criando pasta de destino")?;
    let file = fs::File::open(&entry.osz_path).context("abrindo arquivo .osz")?;
    let mut archive = ZipArchive::new(file).context("lendo arquivo zip")?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = build_safe_path(&dest, file.name())?;

        if file.is_dir() {
            fs::create_dir_all(&outpath)
                .with_context(|| format!("criando pasta {}", outpath.display()))?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("criando pasta {}", parent.display()))?;
            }
            let mut outfile = fs::File::create(&outpath)
                .with_context(|| format!("criando arquivo {}", outpath.display()))?;
            io::copy(&mut file, &mut outfile).context("gravando arquivo extraido")?;
        }
    }

    Ok(ImportResult {
        destination: dest,
        duplicated: false,
    })
}

fn build_folder_name(meta: &BeatmapMetadata, osz_path: &Path) -> String {
    let mut base = format!("{} - {} ({})", meta.artist, meta.title, meta.creator);
    if let Some(set_id) = meta.beatmap_set_id {
        base.push_str(&format!(" [{}]", set_id));
    }
    let sanitized = sanitize_path_component(&base);
    if sanitized.is_empty() {
        let file = osz_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        return sanitize_path_component(&file);
    }
    sanitized
}

fn build_safe_path(base: &Path, inside_zip: &str) -> Result<PathBuf> {
    let mut clean = PathBuf::new();
    for comp in Path::new(inside_zip).components() {
        match comp {
            Component::Normal(s) => clean.push(sanitize_path_component(&s.to_string_lossy())),
            Component::CurDir => continue,
            _ => return Err(anyhow!("Entrada de ZIP com caminho invalido")),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(anyhow!("Entrada de ZIP vazia"));
    }
    let candidate = base.join(clean);
    let canon_base = fs::canonicalize(base)?;
    let canon_candidate =
        fs::canonicalize(candidate.parent().unwrap_or(base)).unwrap_or(base.to_path_buf());
    if !canon_candidate.starts_with(&canon_base) {
        return Err(anyhow!("Tentativa de Zip Slip detectada"));
    }
    Ok(base.join(candidate.strip_prefix(base).unwrap_or(&candidate)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::BeatmapMetadata;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn build_folder_name_handles_invalid() {
        let meta = BeatmapMetadata {
            title: "A*B".into(),
            artist: "Art?".into(),
            creator: "Mapper".into(),
            difficulties: vec![],
            beatmap_set_id: Some(1),
            beatmap_ids: vec![],
            background_file: None,
            audio_file: None,
        };
        let name = build_folder_name(&meta, Path::new("file.osz"));
        assert!(!name.contains('*'));
        assert!(name.contains("Art"));
    }

    #[test]
    fn import_creates_files() {
        let dir = tempdir().unwrap();
        let osz_path = dir.path().join("test.osz");
        // build small zip
        {
            let file = fs::File::create(&osz_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::FileOptions::default();
            zip.start_file("song.txt", options).unwrap();
            write!(zip, "hello").unwrap();
            zip.finish().unwrap();
        }

        let meta = BeatmapMetadata {
            title: "Title".into(),
            artist: "Artist".into(),
            creator: "Creator".into(),
            difficulties: vec![],
            beatmap_set_id: Some(99),
            beatmap_ids: vec![],
            background_file: None,
            audio_file: None,
        };
        let entry = BeatmapEntry {
            id: 1,
            osz_path: osz_path.clone(),
            status: crate::app_state::ImportStatus::Detected,
            message: None,
            error_detail: None,
            error_short: None,
            metadata: None,
            thumbnail_path: None,
            detected_at: std::time::SystemTime::now(),
            destination: None,
            osz_hash: None,
            audio: AudioPreview::default(),
        };
        let songs_dir = dir.path().join("songs");
        fs::create_dir_all(&songs_dir).unwrap();
        let res = import_osz(&entry, &meta, &songs_dir, false).unwrap();
        assert!(res.destination.exists());
        assert!(res.destination.join("song.txt").exists());
    }

    #[test]
    fn reject_zip_slip_paths() {
        let dir = tempdir().unwrap();
        let osz_path = dir.path().join("evil.osz");
        {
            let file = fs::File::create(&osz_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::FileOptions::default();
            zip.start_file("../bad.txt", options).unwrap();
            write!(zip, "bad").unwrap();
            zip.finish().unwrap();
        }
        let meta = BeatmapMetadata {
            title: "Title".into(),
            artist: "Artist".into(),
            creator: "Creator".into(),
            difficulties: vec![],
            beatmap_set_id: None,
            beatmap_ids: vec![],
            background_file: None,
            audio_file: None,
        };
        let entry = BeatmapEntry {
            id: 1,
            osz_path: osz_path.clone(),
            status: crate::app_state::ImportStatus::Detected,
            message: None,
            error_detail: None,
            error_short: None,
            metadata: None,
            thumbnail_path: None,
            detected_at: std::time::SystemTime::now(),
            destination: None,
            osz_hash: None,
            audio: AudioPreview::default(),
        };
        let songs_dir = dir.path().join("songs");
        fs::create_dir_all(&songs_dir).unwrap();
        let res = import_osz(&entry, &meta, &songs_dir, false);
        assert!(res.is_err());
    }
}
