//! M3U and M3U8 playlists.
//!
//! The format is barely a format: a list of paths, optionally with `#EXTINF`
//! lines carrying a duration and a display name. That looseness is the whole
//! problem, so the rules used here are written down rather than implied.

use std::path::{Path, PathBuf};

/// One entry read from a playlist file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    /// The `#EXTINF` display name, when the file carried one. Only used to
    /// report what could not be resolved.
    pub label: Option<String>,
}

fn windows_drive_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'/'
}

/// Parse a playlist.
///
/// Relative entries are resolved against `base`, which is the directory the
/// playlist itself lives in - that is what makes a playlist portable alongside
/// the music it refers to. Comments other than `#EXTINF` are ignored, and a
/// malformed `#EXTINF` costs its label, not its entry.
pub fn parse(source: &str, base: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut pending_label: Option<String> = None;

    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            // "#EXTINF:<seconds>,<name>"; the name is everything after the
            // first comma, which may itself contain commas.
            pending_label = rest.split_once(',').map(|(_, name)| name.trim().to_owned());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        // A Windows-style playlist read on Unix, or the reverse.
        let normalised = line.replace('\\', "/");
        let candidate = Path::new(&normalised);
        let path = if candidate.is_absolute() || windows_drive_absolute(&normalised) {
            candidate.to_path_buf()
        } else {
            base.join(candidate)
        };
        entries.push(Entry {
            path,
            label: pending_label.take(),
        });
    }
    entries
}

/// Render a playlist.
///
/// Paths are written relative to `base` when they sit underneath it, so a
/// playlist exported next to the music stays valid if the whole tree moves.
/// Anything outside is written absolute, because a relative path would be a
/// guess.
pub fn render(tracks: &[(PathBuf, String, u64)], base: Option<&Path>) -> String {
    let mut output = String::from("#EXTM3U\n");
    for (path, label, duration_ms) in tracks {
        let written = base
            .and_then(|base| path.strip_prefix(base).ok())
            .unwrap_or(path.as_path());
        output.push_str(&format!(
            "#EXTINF:{},{}\n{}\n",
            duration_ms / 1000,
            label,
            written.to_string_lossy().replace('\\', "/")
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> PathBuf {
        PathBuf::from("/music")
    }

    #[test]
    fn plain_paths_are_read() {
        let entries = parse("a.flac\nb.flac\n", &base());
        assert_eq!(
            entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
            [base().join("a.flac"), base().join("b.flac")]
        );
    }

    #[test]
    fn relative_entries_resolve_against_the_playlist() {
        // What makes a playlist portable with the music it points at.
        let entries = parse("Album/track.flac\n", &base());
        assert_eq!(entries[0].path, base().join("Album/track.flac"));
    }

    #[test]
    fn absolute_entries_are_left_alone() {
        let entries = parse("/elsewhere/track.flac\n", &base());
        assert_eq!(entries[0].path, PathBuf::from("/elsewhere/track.flac"));
    }

    #[test]
    fn windows_drive_paths_are_absolute_even_on_unix() {
        let entries = parse("C:\\Music\\song.flac\n", &base());
        assert_eq!(entries[0].path, PathBuf::from("C:/Music/song.flac"));
    }

    #[test]
    fn extinf_labels_attach_to_the_entry_that_follows() {
        let entries = parse("#EXTINF:210,Artist - Title\na.flac\n", &base());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label.as_deref(), Some("Artist - Title"));
    }

    #[test]
    fn a_label_containing_commas_survives() {
        let entries = parse("#EXTINF:210,Artist - Hello, Goodbye\na.flac\n", &base());
        assert_eq!(entries[0].label.as_deref(), Some("Artist - Hello, Goodbye"));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let entries = parse("#EXTM3U\n\n# a note\na.flac\n", &base());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, None);
    }

    #[test]
    fn a_malformed_extinf_costs_its_label_not_its_entry() {
        let entries = parse("#EXTINF:nonsense\na.flac\n", &base());
        assert_eq!(entries.len(), 1, "the track must still be read");
        assert_eq!(entries[0].label, None);
    }

    #[test]
    fn backslash_separators_are_understood() {
        // Playlists written on Windows are routinely read on Linux.
        let entries = parse(r"Album\track.flac", &base());
        assert_eq!(entries[0].path, base().join("Album/track.flac"));
    }

    #[test]
    fn rendering_prefers_relative_paths_under_the_base() {
        let tracks = vec![(
            PathBuf::from("/music/Album/one.flac"),
            "Artist - One".into(),
            185_000u64,
        )];
        let rendered = render(&tracks, Some(&base()));
        assert_eq!(
            rendered,
            "#EXTM3U\n#EXTINF:185,Artist - One\nAlbum/one.flac\n"
        );
    }

    #[test]
    fn paths_outside_the_base_stay_absolute() {
        // A relative path here would be a guess about a directory the playlist
        // knows nothing about.
        let tracks = vec![(
            PathBuf::from("/elsewhere/two.flac"),
            "Artist - Two".into(),
            60_000u64,
        )];
        let rendered = render(&tracks, Some(&base()));
        assert!(rendered.contains("\n/elsewhere/two.flac\n"), "{rendered}");
    }

    #[test]
    fn what_is_written_can_be_read_back() {
        let tracks = vec![
            (PathBuf::from("/music/a.flac"), "A".into(), 1_000u64),
            (
                PathBuf::from("/music/Album/b.flac"),
                "B, and more".into(),
                2_500u64,
            ),
        ];
        let rendered = render(&tracks, Some(&base()));
        let entries = parse(&rendered, &base());
        assert_eq!(
            entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
            [
                PathBuf::from("/music/a.flac"),
                PathBuf::from("/music/Album/b.flac")
            ]
        );
        assert_eq!(entries[1].label.as_deref(), Some("B, and more"));
    }
}
