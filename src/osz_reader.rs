use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Result;
use zip::ZipArchive;

use crate::app_state::BeatmapMetadata;
use crate::cache::{CacheStore, thumbnails_dir};
use crate::osu_parser::parse_osu;

#[derive(Debug)]
pub struct OszMetadata {
    pub metadata: BeatmapMetadata,
    pub thumbnail_path: Option<PathBuf>,
    pub hash: String,
}

pub fn read_osz_metadata(path: &Path, cache: &CacheStore) -> Result<OszMetadata> {
    let mut file = File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let hash = blake3::hash(&buf).to_hex().to_string();

    if let Some(cached) = cache.get_thumbnail(&hash) {
        let metadata = extract_metadata_from_archive(&buf)?;
        return Ok(OszMetadata {
            metadata,
            thumbnail_path: Some(cached),
            hash,
        });
    }

    let metadata = extract_metadata_from_archive(&buf)?;
    let thumb = if let Some(bg) = metadata.background_file.clone() {
        let tmp = load_image_from_archive(&buf, &bg)?;
        if let Some(img) = tmp {
            let thumb = create_thumbnail(&img)?;
            let dir = thumbnails_dir();
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{hash}.png"));
            thumb.save(&path)?;
            cache.insert_thumbnail(hash.clone(), path.clone());
            let _ = cache.save();
            Some(path)
        } else {
            None
        }
    } else {
        None
    };

    Ok(OszMetadata {
        metadata,
        thumbnail_path: thumb,
        hash,
    })
}

fn extract_metadata_from_archive(buf: &[u8]) -> Result<BeatmapMetadata> {
    let cursor = std::io::Cursor::new(buf);
    let mut zip = ZipArchive::new(cursor)?;
    let mut parsed_files = Vec::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        if file.name().ends_with(".osu") {
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            if let Ok(parsed) = parse_osu(&contents) {
                parsed_files.push(parsed);
            }
        }
    }
    if parsed_files.is_empty() {
        return Err(anyhow::anyhow!("Nenhum .osu encontrado"));
    }
    let main = parsed_files.first().cloned().unwrap();
    let difficulties = parsed_files.iter().map(|p| p.version.clone()).collect();
    let beatmap_ids = parsed_files
        .iter()
        .filter_map(|p| p.beatmap_id)
        .collect::<Vec<_>>();

    Ok(BeatmapMetadata {
        title: main.title,
        artist: main.artist,
        creator: main.creator,
        difficulties,
        beatmap_set_id: main.beatmap_set_id,
        beatmap_ids,
        background_file: main.background_file,
        audio_file: main.audio_file,
    })
}

fn load_image_from_archive(buf: &[u8], file_name: &str) -> Result<Option<image::DynamicImage>> {
    let cursor = std::io::Cursor::new(buf);
    let mut zip = ZipArchive::new(cursor)?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        if file.name().ends_with(file_name) || file.name().contains(file_name) {
            let mut data = Vec::new();
            file.read_to_end(&mut data)?;
            let img = image::load_from_memory(&data).ok();
            return Ok(img);
        }
    }
    Ok(None)
}

fn create_thumbnail(img: &image::DynamicImage) -> Result<image::DynamicImage> {
    let thumb = img.thumbnail(256, 256);
    Ok(thumb)
}
