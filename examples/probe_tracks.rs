//! Временная диагностика: теги и декодирование треков из state.json.
//! Только читает файлы, ничего не изменяет.

use lofty::{Accessor, TaggedFileExt, read_from_path};
use rodio::{Decoder, Source};
use std::fmt::Write as _;
use std::fs::File;
use std::io::BufReader;

fn main() {
    let appdata = std::env::var("APPDATA").expect("APPDATA");
    let state_path = format!("{appdata}\\EndlessDream\\state.json");
    let data = std::fs::read_to_string(&state_path).expect("state.json");
    let json: serde_json::Value = serde_json::from_str(&data).expect("json");
    let tracks = json["track_paths"].as_array().expect("track_paths");

    let mut out = String::new();

    for (index, value) in tracks.iter().enumerate() {
        let path = value.as_str().expect("path");
        let _ = writeln!(out, "\n{:02}. {}", index + 1, path);

        match read_from_path(path) {
            Ok(tagged) => {
                let _ = writeln!(
                    out,
                    "  tags={} primary={:?}",
                    tagged.tags().len(),
                    tagged.primary_tag().map(|tag| tag.tag_type())
                );
                for tag in tagged.tags() {
                    let _ = writeln!(
                        out,
                        "  [{:?}] title={:?} artist={:?} album={:?} pictures={}",
                        tag.tag_type(),
                        tag.title().as_deref(),
                        tag.artist().as_deref(),
                        tag.album().as_deref(),
                        tag.pictures().len()
                    );
                }
            }
            Err(error) => {
                let _ = writeln!(out, "  TAG ERROR: {error}");
            }
        }

        match File::open(path) {
            Ok(file) => match Decoder::new(BufReader::new(file)) {
                Ok(mut decoder) => {
                    let channels = decoder.channels();
                    let sample_rate = decoder.sample_rate();
                    let duration = decoder.total_duration();
                    let limit = sample_rate as usize * channels as usize * 2;
                    let mut count = 0usize;
                    let mut nonzero = 0usize;
                    let mut peak = 0i32;
                    for sample in decoder.by_ref().take(limit) {
                        count += 1;
                        let value = i32::from(sample);
                        if value != 0 {
                            nonzero += 1;
                        }
                        peak = peak.max(value.abs());
                    }
                    let _ = writeln!(
                        out,
                        "  decode OK: ch={channels} sr={sample_rate} dur={duration:?} first2s_samples={count} nonzero={nonzero} peak={peak}"
                    );
                    if count == 0 {
                        let _ = writeln!(out, "  WARNING: decoder produced 0 samples");
                    }
                }
                Err(error) => {
                    let _ = writeln!(out, "  DECODE ERROR: {error}");
                }
            },
            Err(error) => {
                let _ = writeln!(out, "  FILE OPEN ERROR: {error}");
            }
        }
    }

    let out_path = "C:\\Users\\Rom4i\\AppData\\Local\\Temp\\opencode\\probe_tracks.txt";
    std::fs::write(out_path, out).expect("write probe result");
    println!("written {out_path}");
}
