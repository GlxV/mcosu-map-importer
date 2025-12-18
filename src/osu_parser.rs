use std::collections::HashMap;

use anyhow::Result;
use regex::Regex;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedOsu {
    pub title: String,
    pub artist: String,
    pub creator: String,
    pub version: String,
    pub beatmap_set_id: Option<i32>,
    pub beatmap_id: Option<i32>,
    pub background_file: Option<String>,
    pub audio_file: Option<String>,
}

pub fn parse_osu(content: &str) -> Result<ParsedOsu> {
    let mut sections: HashMap<String, Vec<&str>> = HashMap::new();
    let mut current = String::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            current = line.trim_matches(&['[', ']'][..]).to_string();
            continue;
        }
        if current.is_empty() || line.is_empty() || line.starts_with("//") {
            continue;
        }
        sections.entry(current.clone()).or_default().push(line);
    }

    let metadata = sections.get("Metadata").cloned().unwrap_or_default();
    let general = sections.get("General").cloned().unwrap_or_default();
    let events = sections.get("Events").cloned().unwrap_or_default();

    let mut parsed = ParsedOsu::default();
    let kv_re = Regex::new(r"^([A-Za-z]+)\s*:\s*(.*)$").unwrap();
    for line in metadata {
        if let Some(caps) = kv_re.captures(line) {
            let key = caps.get(1).unwrap().as_str();
            let val = caps.get(2).unwrap().as_str().trim().to_string();
            match key {
                "Title" | "TitleUnicode" if parsed.title.is_empty() => parsed.title = val,
                "Artist" | "ArtistUnicode" if parsed.artist.is_empty() => parsed.artist = val,
                "Creator" => parsed.creator = val,
                "Version" => parsed.version = val,
                "BeatmapSetID" => {
                    if let Ok(id) = val.parse::<i32>() {
                        parsed.beatmap_set_id = Some(id);
                    }
                }
                "BeatmapID" => {
                    if let Ok(id) = val.parse::<i32>() {
                        parsed.beatmap_id = Some(id);
                    }
                }
                _ => {}
            }
        }
    }

    for line in general {
        if let Some(caps) = kv_re.captures(line) {
            let key = caps.get(1).unwrap().as_str();
            let val = caps.get(2).unwrap().as_str().trim().to_string();
            if key == "AudioFilename" && parsed.audio_file.is_none() && !val.is_empty() {
                parsed.audio_file = Some(val);
            }
        }
    }

    // Parse background event: 0,0,"bg.jpg",0,0
    for line in events {
        if line.starts_with("0,") || line.starts_with("Background") {
            // split respecting quoted filename
            let parts: Vec<&str> = line.split(',').collect();
            for part in parts {
                if part.contains(".jpg") || part.contains(".png") {
                    let cleaned = part.trim().trim_matches('"').to_string();
                    parsed.background_file = Some(cleaned);
                    break;
                }
            }
            if parsed.background_file.is_some() {
                break;
            }
        }
    }

    // Basic validation
    if parsed.title.is_empty() && parsed.artist.is_empty() {
        return Err(anyhow::anyhow!("Incomplete metadata"));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_metadata_and_events() {
        let text = r#"
        [Metadata]
        Title:Test Song
        Artist:Tester
        Creator:Mapper
        Version:Hard
        BeatmapSetID:123
        BeatmapID:456

        [Events]
        0,0,"bg.jpg",0,0
        "#;
        let parsed = parse_osu(text).unwrap();
        assert_eq!(parsed.title, "Test Song");
        assert_eq!(parsed.artist, "Tester");
        assert_eq!(parsed.creator, "Mapper");
        assert_eq!(parsed.version, "Hard");
        assert_eq!(parsed.beatmap_set_id, Some(123));
        assert_eq!(parsed.background_file.as_deref(), Some("bg.jpg"));
    }
}
