use std::collections::{BTreeMap, HashMap};

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::model::{SmartMatch, SmartPlaylist, SmartRule, Track, TrackStats};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Genre {
    pub name: String,
    pub track_ids: Vec<String>,
}

pub fn group_genres(tracks: &[Track]) -> Vec<Genre> {
    let mut grouped: BTreeMap<String, Genre> = BTreeMap::new();
    for track in tracks {
        let name = if track.genre.trim().is_empty() {
            "Sin género"
        } else {
            track.genre.trim()
        };
        let key = fold(name);
        grouped
            .entry(key)
            .or_insert_with(|| Genre {
                name: name.to_owned(),
                track_ids: Vec::new(),
            })
            .track_ids
            .push(track.id.clone());
    }
    grouped.into_values().collect()
}

pub fn fuzzy_search(tracks: &[Track], query: &str, limit: usize) -> Vec<usize> {
    let query = fold(query.trim());
    if query.is_empty() {
        return Vec::new();
    }
    let mut scored = tracks
        .iter()
        .enumerate()
        .filter_map(|(index, track)| {
            let fields = [&track.title, &track.artist, &track.album, &track.genre];
            fields
                .into_iter()
                .filter_map(|field| fuzzy_score(&fold(field), &query))
                .max()
                .map(|score| (index, score))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|(index, score)| (std::cmp::Reverse(*score), *index));
    scored.truncate(limit);
    scored.into_iter().map(|(index, _)| index).collect()
}

fn fuzzy_score(value: &str, query: &str) -> Option<usize> {
    if value == query {
        return Some(10_000);
    }
    if value.starts_with(query) {
        return Some(9_000usize.saturating_sub(value.len() - query.len()));
    }
    if let Some(position) = value.find(query) {
        return Some(8_000usize.saturating_sub(position));
    }
    value
        .split_whitespace()
        .map(|word| levenshtein(word, query))
        .min()
        .filter(|distance| *distance <= (query.chars().count() / 3).max(1))
        .map(|distance| 5_000usize.saturating_sub(distance * 100))
}

pub fn evaluate_smart_playlist(
    playlist: &SmartPlaylist,
    tracks: &[Track],
    stats: &HashMap<String, TrackStats>,
    added_at: &HashMap<String, i64>,
    now: i64,
) -> Vec<String> {
    let mut matching = tracks
        .iter()
        .filter(|track| {
            let mut values = playlist
                .rules
                .iter()
                .map(|rule| rule_matches(rule, track, stats.get(&track.id), added_at, now));
            match playlist.match_mode {
                SmartMatch::All => values.all(std::convert::identity),
                SmartMatch::Any => values.any(std::convert::identity),
            }
        })
        .collect::<Vec<_>>();
    matching.sort_by(|left, right| {
        let order = match playlist.sort_field.as_str() {
            "added_at" => added_at.get(&left.id).cmp(&added_at.get(&right.id)),
            "last_played" => stats
                .get(&left.id)
                .and_then(|value| value.last_played_at)
                .cmp(&stats.get(&right.id).and_then(|value| value.last_played_at)),
            "play_count" => stats
                .get(&left.id)
                .map_or(0, |value| value.play_count)
                .cmp(&stats.get(&right.id).map_or(0, |value| value.play_count)),
            "duration" => left.duration_ms.cmp(&right.duration_ms),
            _ => fold(&left.title).cmp(&fold(&right.title)),
        };
        if playlist.descending {
            order.reverse()
        } else {
            order
        }
    });
    if let Some(limit) = playlist.limit {
        matching.truncate(limit);
    }
    matching.into_iter().map(|track| track.id.clone()).collect()
}

fn rule_matches(
    rule: &SmartRule,
    track: &Track,
    stats: Option<&TrackStats>,
    added_at: &HashMap<String, i64>,
    now: i64,
) -> bool {
    let string = rule.value.as_str().unwrap_or_default();
    let number = rule.value.as_i64().unwrap_or_default();
    match (rule.field.as_str(), rule.operator.as_str()) {
        ("title", "contains") => fold(&track.title).contains(&fold(string)),
        ("artist", "contains") => fold(&track.artist).contains(&fold(string)),
        ("album", "contains") => fold(&track.album).contains(&fold(string)),
        ("genre", "contains") => fold(&track.genre).contains(&fold(string)),
        ("favorite", "is") => track.favorite == rule.value.as_bool().unwrap_or(false),
        ("available", "is") => track.available == rule.value.as_bool().unwrap_or(false),
        ("played", "is") => {
            (stats.map_or(0, |value| value.play_count) > 0) == rule.value.as_bool().unwrap_or(false)
        }
        ("play_count", "gte") => stats.map_or(0, |value| value.play_count) >= number.max(0) as u64,
        ("duration_ms", "gte") => track.duration_ms >= number.max(0) as u64,
        ("added_days", "lte") => added_at
            .get(&track.id)
            .is_some_and(|timestamp| now.saturating_sub(*timestamp) <= number * 86_400),
        ("last_played_days", "lte") => stats
            .and_then(|value| value.last_played_at)
            .is_some_and(|timestamp| now.saturating_sub(timestamp) <= number * 86_400),
        _ => false,
    }
}

pub fn fold(value: &str) -> String {
    value
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn levenshtein(left: &str, right: &str) -> usize {
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (row, left_char) in left.chars().enumerate() {
        let mut current = vec![row + 1];
        for (column, right_char) in right.iter().enumerate() {
            current.push(
                (previous[column + 1] + 1)
                    .min(current[column] + 1)
                    .min(previous[column] + usize::from(left_char != *right_char)),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, genre: &str, favorite: bool) -> Track {
        Track {
            id: id.into(),
            source_id: "source".into(),
            relative_path: format!("{id}.flac"),
            path: format!("/{id}.flac").into(),
            title: id.into(),
            artist: "Artist".into(),
            album_artist: "Artist".into(),
            album: "Album".into(),
            genre: genre.into(),
            year: None,
            disc_number: 1,
            track_number: 1,
            duration_ms: 60_000,
            cover_path: None,
            available: true,
            favorite,
        }
    }

    #[test]
    fn folding_removes_accents_and_extra_space() {
        assert_eq!(fold("  Música ÉLÉCTRÓNICA "), "musica electronica");
    }

    #[test]
    fn fuzzy_score_tolerates_a_typo() {
        assert!(fuzzy_score("skrillex", "skrilex").is_some());
    }

    #[test]
    fn smart_playlists_apply_all_any_sort_and_limit() {
        let tracks = vec![
            track("one", "Metal", true),
            track("two", "Metal", false),
            track("three", "Jazz", true),
        ];
        let rules = vec![
            SmartRule {
                field: "genre".into(),
                operator: "contains".into(),
                value: serde_json::json!("metal"),
            },
            SmartRule {
                field: "favorite".into(),
                operator: "is".into(),
                value: serde_json::json!(true),
            },
        ];
        let playlist = SmartPlaylist {
            id: 1,
            name: "test".into(),
            match_mode: SmartMatch::All,
            rules: rules.clone(),
            sort_field: "title".into(),
            descending: false,
            limit: None,
        };
        assert_eq!(
            evaluate_smart_playlist(&playlist, &tracks, &HashMap::new(), &HashMap::new(), 0),
            ["one"]
        );
        let any = SmartPlaylist {
            match_mode: SmartMatch::Any,
            limit: Some(2),
            ..playlist
        };
        assert_eq!(
            evaluate_smart_playlist(&any, &tracks, &HashMap::new(), &HashMap::new(), 0),
            ["one", "three"]
        );
    }
}
