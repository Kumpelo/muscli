//! Resource limits for artwork from downloaded music and sidecar files.
//!
//! Cover art is untrusted input even when it is embedded in an audio file.
//! Decode before thumbnailing only after bounding compressed and pixel sizes.

use std::{
    fs,
    io::{Cursor, Read},
    path::Path,
};

use image::{DynamicImage, ImageReader, Limits};

pub(crate) const MAX_COVER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8_192;
const MAX_PIXELS: u64 = 24_000_000;
const MAX_DECODE_ALLOC: u64 = 128 * 1024 * 1024;

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    // Best effort: some decoders do not enforce allocation limits.
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    limits
}

/// Read a regular sidecar file, not a symlink, with a hard byte ceiling.
pub(crate) fn read_file(path: &Path) -> Option<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_COVER_BYTES {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_COVER_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_COVER_BYTES).then_some(bytes)
}

/// Decode untrusted embedded or sidecar art under size and dimension limits.
pub(crate) fn decode_bytes(data: &[u8]) -> Option<DynamicImage> {
    if data.len() as u64 > MAX_COVER_BYTES {
        return None;
    }

    // Check dimensions without allocating the expanded pixel buffer first.
    let dimensions = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    let (width, height) = dimensions;
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return None;
    }

    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits());
    reader.decode().ok()
}

pub(crate) fn decode_file(path: &Path) -> Option<DynamicImage> {
    decode_bytes(&read_file(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_png_and_rejects_invalid_or_oversized_data() {
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(2, 2)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        assert_eq!(decode_bytes(&encoded.into_inner()).unwrap().width(), 2);
        assert!(decode_bytes(b"not an image").is_none());
        assert!(decode_bytes(&vec![0u8; MAX_COVER_BYTES as usize + 1]).is_none());
    }

    #[test]
    fn rejects_oversized_sidecar_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cover.png");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_COVER_BYTES + 1).unwrap();
        assert!(read_file(&path).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_sidecar_artwork() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("actual.png");
        fs::write(&target, b"image").unwrap();
        let link = directory.path().join("cover.png");
        symlink(&target, &link).unwrap();
        assert!(read_file(&link).is_none());
    }
}
