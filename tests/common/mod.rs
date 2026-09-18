//! Shared helpers for the integration tests.
//!
//! Audio fixtures are generated at test time instead of being committed, so the
//! repository never carries binary media and every fixture is unambiguously
//! original (see CONTRIBUTING.md).

#![allow(dead_code)]

use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};

use lofty::{
    config::WriteOptions,
    picture::{MimeType, Picture, PictureType},
    prelude::{ItemKey, TagExt},
    tag::{Tag, TagType},
};
use muscli::paths::AppPaths;

/// Samples per FLAC frame. Any value is legal because the frame header carries
/// an explicit block size, but a power of two keeps the fixtures conventional.
const BLOCK: usize = 4096;

// ---------------------------------------------------------------------------
// Minimal FLAC encoder
// ---------------------------------------------------------------------------

/// Encode 16-bit mono samples as a FLAC stream.
///
/// Every subframe is VERBATIM (raw samples), which makes the file larger than a
/// real encoder would but keeps the writer small and, more importantly, exactly
/// byte-aligned: the frame header, the subframe header and the samples all land
/// on byte boundaries, so no bit-level writer is needed.
pub fn flac_bytes(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
    assert!(
        !samples.is_empty(),
        "a FLAC stream needs at least one sample"
    );
    assert!(
        sample_rate > 0 && sample_rate < (1 << 20),
        "sample rate out of range"
    );

    let blocks: Vec<&[i16]> = samples.chunks(BLOCK).collect();
    let min_block = blocks.iter().map(|b| b.len()).min().unwrap_or(BLOCK) as u16;
    let max_block = blocks.iter().map(|b| b.len()).max().unwrap_or(BLOCK) as u16;

    let mut out = Vec::with_capacity(samples.len() * 2 + 128);
    out.extend_from_slice(b"fLaC");

    // METADATA_BLOCK_HEADER: last-block = 1, type = 0 (STREAMINFO), length = 34.
    out.push(0x80);
    out.extend_from_slice(&[0x00, 0x00, 0x22]);

    out.extend_from_slice(&min_block.to_be_bytes());
    out.extend_from_slice(&max_block.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0]); // min frame size: unknown
    out.extend_from_slice(&[0, 0, 0]); // max frame size: unknown

    // 20 bits sample rate | 3 bits (channels - 1) | 5 bits (bits per sample - 1)
    // | 36 bits total samples.
    let channels: u64 = 1;
    let bits_per_sample: u64 = 16;
    let packed = ((sample_rate as u64) << 44)
        | ((channels - 1) << 41)
        | ((bits_per_sample - 1) << 36)
        | (samples.len() as u64 & 0xF_FFFF_FFFF);
    out.extend_from_slice(&packed.to_be_bytes());
    out.extend_from_slice(&[0u8; 16]); // MD5 of the unencoded audio: not computed

    for (number, block) in blocks.iter().enumerate() {
        write_frame(&mut out, number as u64, block);
    }
    out
}

fn write_frame(out: &mut Vec<u8>, frame_number: u64, samples: &[i16]) {
    let start = out.len();

    // 14 bits sync | 1 reserved | 1 blocking strategy (fixed) = 0xFF 0xF8,
    // then 4 bits block size (0b0111: 16-bit value after the frame number),
    // 4 bits sample rate (0b0000: from STREAMINFO) = 0x70, then 4 bits channel
    // assignment (mono), 3 bits sample size (from STREAMINFO), 1 reserved = 0x00.
    out.extend_from_slice(&[0xFF, 0xF8, 0x70, 0x00]);
    push_utf8_number(out, frame_number);
    out.extend_from_slice(&((samples.len() - 1) as u16).to_be_bytes());
    let header_crc = crc8(&out[start..]);
    out.push(header_crc);

    // SUBFRAME header: 1 padding bit, 6 bits type (0b000001 = VERBATIM),
    // 1 bit "wasted bits" flag.
    out.push(0b0000_0010);
    for sample in samples {
        out.extend_from_slice(&sample.to_be_bytes());
    }

    let frame_crc = crc16(&out[start..]);
    out.extend_from_slice(&frame_crc.to_be_bytes());
}

/// FLAC codes frame numbers with the same variable-length scheme as UTF-8,
/// extended to 36 bits.
fn push_utf8_number(out: &mut Vec<u8>, value: u64) {
    if value < 0x80 {
        out.push(value as u8);
        return;
    }
    // A `len`-byte sequence carries `5 * len + 1` bits.
    let mut len = 2usize;
    while len < 7 && value >= (1u64 << (5 * len + 1)) {
        len += 1;
    }
    let mut bytes = vec![0u8; len];
    let mut rest = value;
    for slot in bytes.iter_mut().skip(1).rev() {
        *slot = 0x80 | (rest & 0x3F) as u8;
        rest >>= 6;
    }
    bytes[0] = ((0xFFu16 << (8 - len)) as u8) | rest as u8;
    out.extend_from_slice(&bytes);
}

/// CRC-8 with polynomial x^8 + x^2 + x + 1, as the FLAC frame header uses.
fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-16 with polynomial x^16 + x^15 + x^2 + 1, as the FLAC frame footer uses.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in data {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

// ---------------------------------------------------------------------------
// Signal and artwork generation
// ---------------------------------------------------------------------------

/// A sine tone, so the fixtures carry real audio rather than silence (silence
/// would make any future loudness assertion degenerate).
pub fn sine(sample_rate: u32, frequency: f64, millis: u32) -> Vec<i16> {
    let total = (sample_rate as u64 * millis as u64 / 1000).max(1) as usize;
    (0..total)
        .map(|index| {
            let phase = std::f64::consts::TAU * frequency * index as f64 / sample_rate as f64;
            (phase.sin() * 12_000.0) as i16
        })
        .collect()
}

/// A solid-colour PNG, used as embedded or external album art.
pub fn png_bytes(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut image = image::RgbImage::new(width, height);
    for pixel in image.pixels_mut() {
        *pixel = image::Rgb(rgb);
    }
    let mut buffer = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut buffer, image::ImageFormat::Png)
        .expect("encoding a generated PNG cannot fail");
    buffer.into_inner()
}

// ---------------------------------------------------------------------------
// Track fixtures
// ---------------------------------------------------------------------------

/// The tags a generated track should carry. `Default` gives a fully tagged
/// track; override only what a test actually cares about.
#[derive(Debug, Clone)]
pub struct TrackSpec {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub year: Option<i32>,
    pub track_number: u32,
    pub disc_number: u32,
    pub millis: u32,
    pub frequency: f64,
    pub cover: Option<Vec<u8>>,
}

impl Default for TrackSpec {
    fn default() -> Self {
        Self {
            title: "Test Title".into(),
            artist: "Test Artist".into(),
            album: "Test Album".into(),
            album_artist: "Test Artist".into(),
            genre: "Test Genre".into(),
            year: Some(2024),
            track_number: 1,
            disc_number: 1,
            millis: 512,
            frequency: 440.0,
            cover: None,
        }
    }
}

impl TrackSpec {
    pub fn new(title: &str) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    pub fn artist(mut self, artist: &str) -> Self {
        self.artist = artist.into();
        self.album_artist = artist.into();
        self
    }

    pub fn album(mut self, album: &str) -> Self {
        self.album = album.into();
        self
    }

    pub fn genre(mut self, genre: &str) -> Self {
        self.genre = genre.into();
        self
    }

    pub fn track_number(mut self, number: u32) -> Self {
        self.track_number = number;
        self
    }

    pub fn millis(mut self, millis: u32) -> Self {
        self.millis = millis;
        self
    }

    pub fn cover(mut self, cover: Vec<u8>) -> Self {
        self.cover = Some(cover);
        self
    }
}

/// Write a tagged FLAC file. Uses an 8 kHz sample rate so that a fixture with
/// half a second of audio stays around 8 KB even with verbatim subframes.
pub fn write_track(path: &Path, spec: &TrackSpec) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("creating the fixture directory");
    }
    let samples = sine(8_000, spec.frequency, spec.millis);
    fs::write(path, flac_bytes(8_000, &samples)).expect("writing the FLAC fixture");

    let mut tag = Tag::new(TagType::VorbisComments);
    tag.insert_text(ItemKey::TrackTitle, spec.title.clone());
    tag.insert_text(ItemKey::TrackArtist, spec.artist.clone());
    tag.insert_text(ItemKey::AlbumTitle, spec.album.clone());
    tag.insert_text(ItemKey::AlbumArtist, spec.album_artist.clone());
    tag.insert_text(ItemKey::Genre, spec.genre.clone());
    tag.insert_text(ItemKey::TrackNumber, spec.track_number.to_string());
    tag.insert_text(ItemKey::DiscNumber, spec.disc_number.to_string());
    if let Some(year) = spec.year {
        tag.insert_text(ItemKey::RecordingDate, year.to_string());
    }
    if let Some(cover) = &spec.cover {
        tag.push_picture(
            Picture::unchecked(cover.clone())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Png)
                .build(),
        );
    }
    tag.save_to_path(path, WriteOptions::default())
        .expect("writing tags to the FLAC fixture");
}

// ---------------------------------------------------------------------------
// Isolated application directories
// ---------------------------------------------------------------------------

/// A throwaway set of application directories plus a music source directory.
/// Everything lives inside one `TempDir`, so a test never touches the real
/// config, database or cover cache.
pub struct Fixture {
    root: tempfile::TempDir,
    paths: AppPaths,
}

impl Fixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("creating the fixture root");
        let base = root.path();
        let paths = AppPaths {
            config_dir: base.join("config"),
            data_dir: base.join("data"),
            cache_dir: base.join("cache"),
            runtime_dir: base.join("runtime"),
        };
        paths.ensure().expect("creating the fixture directories");
        fs::create_dir_all(base.join("music")).expect("creating the music directory");
        Self { root, paths }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// The directory that stands in for a music drive.
    pub fn source(&self) -> PathBuf {
        self.root.path().join("music")
    }

    pub fn cover_cache_files(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(self.paths.cover_cache_dir()) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        files
    }

    pub fn cover_cache_bytes(&self) -> u64 {
        self.cover_cache_files()
            .iter()
            .filter_map(|path| fs::metadata(path).ok())
            .map(|meta| meta.len())
            .sum()
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
