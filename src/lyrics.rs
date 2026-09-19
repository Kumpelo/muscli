//! Lyrics.
//!
//! Lyrics live in their own directory and are only ever put there on purpose.
//! The library scanner never looks at `.lrc` files sitting beside the audio:
//! those files belong to whoever put them there, and silently absorbing them
//! would make the library index depend on material muscli did not index.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::{features::fold, model::Track, paths::AppPaths};

/// One timed line of an LRC file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    pub at_ms: u64,
    pub text: String,
}

/// A parsed lyrics file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lyrics {
    pub lines: Vec<LyricLine>,
    /// True when no line carried a timestamp, so the text cannot follow
    /// playback and is shown as a plain block.
    pub unsynced: bool,
}

impl Lyrics {
    /// The line that should be highlighted at `position_ms`.
    ///
    /// The last line whose timestamp has passed, so a long instrumental gap
    /// keeps the previous line lit rather than going blank.
    pub fn line_at(&self, position_ms: u64) -> Option<usize> {
        if self.unsynced {
            return None;
        }
        self.lines
            .iter()
            .rposition(|line| line.at_ms <= position_ms)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Parse LRC text.
///
/// Handles the common shapes: several timestamps on one line, two- or
/// three-digit fractions, and metadata tags such as `[ar:...]`, which are
/// skipped rather than shown as lyrics.
pub fn parse_lrc(source: &str) -> Lyrics {
    let mut lines = Vec::new();
    let mut untimed = Vec::new();

    for raw in source.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut rest = trimmed;
        let mut stamps = Vec::new();
        while rest.starts_with('[') {
            let Some(close) = rest.find(']') else { break };
            let tag = &rest[1..close];
            match parse_timestamp(tag) {
                Some(at_ms) => stamps.push(at_ms),
                // A metadata tag, not a time: stop looking for stamps but do
                // not treat the rest as lyrics either.
                None => {
                    rest = "";
                    break;
                }
            }
            rest = rest[close + 1..].trim_start();
        }
        if stamps.is_empty() {
            if !rest.is_empty() {
                untimed.push(rest.to_owned());
            }
            continue;
        }
        for at_ms in stamps {
            lines.push(LyricLine {
                at_ms,
                text: rest.to_owned(),
            });
        }
    }

    if lines.is_empty() {
        return Lyrics {
            lines: untimed
                .into_iter()
                .map(|text| LyricLine { at_ms: 0, text })
                .collect(),
            unsynced: true,
        };
    }
    lines.sort_by_key(|line| line.at_ms);
    Lyrics {
        lines,
        unsynced: false,
    }
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss.xxx`.
fn parse_timestamp(tag: &str) -> Option<u64> {
    let (minutes, rest) = tag.split_once(':')?;
    let minutes: u64 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = match rest.split_once(['.', ':']) {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (rest, None),
    };
    let seconds: u64 = seconds.trim().parse().ok()?;
    let millis = match fraction {
        Some(fraction) => {
            let digits: String = fraction.chars().filter(char::is_ascii_digit).collect();
            if digits.is_empty() {
                return None;
            }
            // Two digits are hundredths, three are already milliseconds.
            let value: u64 = digits.parse().ok()?;
            match digits.len() {
                1 => value * 100,
                2 => value * 10,
                _ => value,
            }
        }
        None => 0,
    };
    Some(minutes * 60_000 + seconds * 1_000 + millis)
}

/// Where a track's lyrics would be stored.
pub fn lyrics_path(paths: &AppPaths, track: &Track) -> PathBuf {
    paths.lyrics_dir().join(format!("{}.lrc", track.id))
}

/// A filename derived from the tags, for lyrics imported without a track id.
pub fn descriptive_name(artist: &str, title: &str) -> String {
    let clean = |value: &str| {
        fold(value)
            .chars()
            .map(|character| {
                if character.is_alphanumeric() || character == ' ' {
                    character
                } else {
                    '-'
                }
            })
            .collect::<String>()
    };
    format!("{} - {}.lrc", clean(artist), clean(title))
}

/// Find a track's lyrics, by id and then by artist and title.
///
/// The second lookup is what makes lyrics survive a file being moved: the id
/// is derived from the path, so moving the audio changes it.
pub fn load(paths: &AppPaths, track: &Track) -> Option<Lyrics> {
    let by_id = lyrics_path(paths, track);
    let candidates = [
        by_id,
        paths
            .lyrics_dir()
            .join(descriptive_name(&track.artist, &track.title)),
    ];
    candidates
        .iter()
        .find_map(|path| fs::read_to_string(path).ok())
        .map(|source| parse_lrc(&source))
        .filter(|lyrics| !lyrics.is_empty())
}

/// Copy a lyrics file into the lyrics directory.
///
/// Explicit by design: nothing else ever puts a file here.
pub fn import(paths: &AppPaths, source: &Path, name: &str) -> Result<PathBuf> {
    let directory = paths.lyrics_dir();
    fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    let text = fs::read_to_string(source)
        .with_context(|| format!("could not read {}", source.display()))?;
    if parse_lrc(&text).is_empty() {
        anyhow::bail!("{} has no lyrics in it", source.display());
    }
    let relative = Path::new(name);
    if relative.is_absolute()
        || relative.components().count() != 1
        || relative.file_name().and_then(|name| name.to_str()) != Some(name)
    {
        anyhow::bail!("lyrics destination must be a single file name");
    }
    let target = directory.join(relative);
    fs::write(&target, text).with_context(|| format!("could not write {}", target.display()))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_rejects_paths_outside_the_lyrics_directory() {
        let fixture = tempfile::tempdir().expect("temporary directory");
        let paths = AppPaths::from_root(fixture.path().join("muscli"));
        let source = fixture.path().join("source.lrc");
        fs::write(&source, "[00:01.00]hello\n").expect("writing source lyrics");

        assert!(import(&paths, &source, "../escape.lrc").is_err());
        assert!(import(&paths, &source, "/tmp/escape.lrc").is_err());

        #[cfg(windows)]
        assert!(import(&paths, &source, r"C:\\escape.lrc").is_err());
    }

    #[test]
    fn timestamps_are_parsed_in_every_common_shape() {
        assert_eq!(parse_timestamp("00:00"), Some(0));
        assert_eq!(parse_timestamp("01:30"), Some(90_000));
        assert_eq!(parse_timestamp("01:30.5"), Some(90_500));
        assert_eq!(parse_timestamp("01:30.50"), Some(90_500));
        assert_eq!(parse_timestamp("01:30.500"), Some(90_500));
        assert_eq!(parse_timestamp("ar:Some Artist"), None);
        assert_eq!(parse_timestamp("nonsense"), None);
    }

    #[test]
    fn a_synced_file_is_ordered_by_time() {
        let lyrics = parse_lrc("[00:10.00]second\n[00:05.00]first\n[00:20.00]third\n");
        assert!(!lyrics.unsynced);
        let texts: Vec<&str> = lyrics.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["first", "second", "third"]);
    }

    #[test]
    fn one_line_can_carry_several_timestamps() {
        // A repeated chorus is usually written this way.
        let lyrics = parse_lrc("[00:10.00][01:10.00]chorus\n");
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].at_ms, 10_000);
        assert_eq!(lyrics.lines[1].at_ms, 70_000);
        assert!(lyrics.lines.iter().all(|line| line.text == "chorus"));
    }

    #[test]
    fn metadata_tags_are_not_shown_as_lyrics() {
        let lyrics = parse_lrc("[ar:Artist]\n[ti:Title]\n[00:01.00]real line\n");
        assert_eq!(lyrics.lines.len(), 1);
        assert_eq!(lyrics.lines[0].text, "real line");
    }

    #[test]
    fn a_file_without_timestamps_is_kept_as_plain_text() {
        let lyrics = parse_lrc("first line\nsecond line\n");
        assert!(lyrics.unsynced);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(
            lyrics.line_at(5_000),
            None,
            "plain text cannot follow along"
        );
    }

    #[test]
    fn the_current_line_is_the_last_one_reached() {
        let lyrics = parse_lrc("[00:00.00]a\n[00:10.00]b\n[00:20.00]c\n");
        assert_eq!(lyrics.line_at(0), Some(0));
        assert_eq!(lyrics.line_at(9_999), Some(0));
        assert_eq!(lyrics.line_at(10_000), Some(1));
        assert_eq!(
            lyrics.line_at(60_000),
            Some(2),
            "a long outro keeps the last line lit rather than going blank"
        );
    }

    #[test]
    fn a_line_before_the_first_timestamp_highlights_nothing() {
        let lyrics = parse_lrc("[00:05.00]only\n");
        assert_eq!(lyrics.line_at(0), None);
        assert_eq!(lyrics.line_at(5_000), Some(0));
    }

    #[test]
    fn descriptive_names_are_stable_and_filesystem_safe() {
        assert_eq!(
            descriptive_name("Sigur Rós", "Hoppípolla"),
            "sigur ros - hoppipolla.lrc"
        );
        assert_eq!(
            descriptive_name("AC/DC", "T.N.T."),
            "ac-dc - t-n-t-.lrc",
            "path separators and dots must not survive into a filename"
        );
    }

    #[test]
    fn an_empty_file_is_not_lyrics() {
        assert!(parse_lrc("").is_empty());
        assert!(parse_lrc("[ar:Only metadata]\n").is_empty());
    }
}
