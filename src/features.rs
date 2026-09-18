use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap, HashMap},
    sync::Arc,
};

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::model::{SmartMatch, SmartPlaylist, SmartRule, Track, TrackStats};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Genre {
    pub name: String,
    pub track_ids: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchIndex {
    fields: Arc<Vec<[String; 4]>>,
}

struct RuleContext<'a> {
    track_index: usize,
    track: &'a Track,
    search_index: &'a SearchIndex,
    stats: Option<&'a TrackStats>,
    added_at: &'a HashMap<String, i64>,
    now: i64,
}

impl SearchIndex {
    pub fn build(tracks: &[Track]) -> Self {
        let _profile = crate::profiling::span("search_index_build");
        Self {
            fields: Arc::new(
                tracks
                    .iter()
                    .map(|track| {
                        [
                            fold(&track.title),
                            fold(&track.artist),
                            fold(&track.album),
                            fold(&track.genre),
                        ]
                    })
                    .collect(),
            ),
        }
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        let _profile = crate::profiling::span("fuzzy_search");
        let query = fold(query.trim());
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }

        let max_distance = (query.chars().count() / 3).max(1);
        let mut best = BinaryHeap::<Reverse<(usize, Reverse<usize>)>>::with_capacity(limit + 1);
        for (index, fields) in self.fields.iter().enumerate() {
            let Some(score) = fields
                .iter()
                .filter_map(|field| fuzzy_score(field, &query, max_distance))
                .max()
            else {
                continue;
            };
            let rank = (score, Reverse(index));
            if best.len() < limit {
                best.push(Reverse(rank));
            } else if best.peek().is_some_and(|Reverse(worst)| rank > *worst) {
                best.pop();
                best.push(Reverse(rank));
            }
        }

        let mut scored = best
            .into_iter()
            .map(|Reverse((score, Reverse(index)))| (index, score))
            .collect::<Vec<_>>();
        scored.sort_by_key(|(index, score)| (Reverse(*score), *index));
        scored.into_iter().map(|(index, _)| index).collect()
    }
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
    SearchIndex::build(tracks).search(query, limit)
}

fn fuzzy_score(value: &str, query: &str, max_distance: usize) -> Option<usize> {
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
        .filter_map(|word| levenshtein_bounded(word, query, max_distance))
        .min()
        .map(|distance| 5_000usize.saturating_sub(distance * 100))
}

pub fn evaluate_smart_playlist(
    playlist: &SmartPlaylist,
    tracks: &[Track],
    search_index: &SearchIndex,
    stats: &HashMap<String, TrackStats>,
    added_at: &HashMap<String, i64>,
    now: i64,
) -> Vec<String> {
    let folded_rule_values = playlist
        .rules
        .iter()
        .map(|rule| rule.value.as_str().map(fold))
        .collect::<Vec<_>>();
    let mut matching = tracks
        .iter()
        .enumerate()
        .filter(|(track_index, track)| {
            let context = RuleContext {
                track_index: *track_index,
                track,
                search_index,
                stats: stats.get(&track.id),
                added_at,
                now,
            };
            let mut values = playlist.rules.iter().enumerate().map(|(rule_index, rule)| {
                rule_matches(rule, folded_rule_values[rule_index].as_deref(), &context)
            });
            match playlist.match_mode {
                SmartMatch::All => values.all(std::convert::identity),
                SmartMatch::Any => values.any(std::convert::identity),
            }
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    matching.sort_by(|&left_index, &right_index| {
        let left = &tracks[left_index];
        let right = &tracks[right_index];
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
            _ => search_index.fields[left_index][0].cmp(&search_index.fields[right_index][0]),
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
    matching
        .into_iter()
        .map(|index| tracks[index].id.clone())
        .collect()
}

fn rule_matches(rule: &SmartRule, folded_string: Option<&str>, context: &RuleContext<'_>) -> bool {
    let number = rule.value.as_i64().unwrap_or_default();
    let string = folded_string.unwrap_or_default();
    match (rule.field.as_str(), rule.operator.as_str()) {
        ("title", "contains") => {
            context.search_index.fields[context.track_index][0].contains(string)
        }
        ("artist", "contains") => {
            context.search_index.fields[context.track_index][1].contains(string)
        }
        ("album", "contains") => {
            context.search_index.fields[context.track_index][2].contains(string)
        }
        ("genre", "contains") => {
            context.search_index.fields[context.track_index][3].contains(string)
        }
        ("favorite", "is") => context.track.favorite == rule.value.as_bool().unwrap_or(false),
        ("available", "is") => context.track.available == rule.value.as_bool().unwrap_or(false),
        ("played", "is") => {
            (context.stats.map_or(0, |value| value.play_count) > 0)
                == rule.value.as_bool().unwrap_or(false)
        }
        ("play_count", "gte") => {
            context.stats.map_or(0, |value| value.play_count) >= number.max(0) as u64
        }
        ("duration_ms", "gte") => context.track.duration_ms >= number.max(0) as u64,
        ("added_days", "lte") => context
            .added_at
            .get(&context.track.id)
            .is_some_and(|timestamp| context.now.saturating_sub(*timestamp) <= number * 86_400),
        ("last_played_days", "lte") => context
            .stats
            .and_then(|value| value.last_played_at)
            .is_some_and(|timestamp| context.now.saturating_sub(timestamp) <= number * 86_400),
        _ => false,
    }
}

pub fn fold(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase)
    {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(character);
    }
    output
}

fn levenshtein_bounded(left: &str, right: &str, max_distance: usize) -> Option<usize> {
    let left_len = left.chars().count();
    let right = right.chars().collect::<Vec<_>>();
    if left_len.abs_diff(right.len()) > max_distance {
        return None;
    }

    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (row, left_char) in left.chars().enumerate() {
        current[0] = row + 1;
        let mut row_min = current[0];
        for (column, right_char) in right.iter().enumerate() {
            current[column + 1] = (previous[column + 1] + 1)
                .min(current[column] + 1)
                .min(previous[column] + usize::from(left_char != *right_char));
            row_min = row_min.min(current[column + 1]);
        }
        if row_min > max_distance {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }

    (previous[right.len()] <= max_distance).then_some(previous[right.len()])
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
    fn search_index_reuses_normalized_track_fields() {
        let tracks = vec![
            track("Música", "House", false),
            track("Other", "Jazz", false),
        ];
        let index = SearchIndex::build(&tracks);
        assert_eq!(index.search("musica", 10), [0]);
        assert_eq!(index.search("jazz", 10), [1]);
    }

    #[test]
    fn bounded_levenshtein_stops_impossible_matches() {
        assert_eq!(levenshtein_bounded("radiohead", "radiohed", 2), Some(1));
        assert_eq!(levenshtein_bounded("radiohead", "x", 2), None);
        assert_eq!(levenshtein_bounded("abcdef", "uvwxyz", 1), None);
    }

    #[test]
    fn search_limit_keeps_the_best_ranked_results() {
        let tracks = vec![
            track("alpha", "", false),
            track("alphabet", "", false),
            track("x alpha", "", false),
        ];
        let index = SearchIndex::build(&tracks);
        assert_eq!(index.search("alpha", 2), [0, 1]);
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
            evaluate_smart_playlist(
                &playlist,
                &tracks,
                &SearchIndex::build(&tracks),
                &HashMap::new(),
                &HashMap::new(),
                0,
            ),
            ["one"]
        );
        let any = SmartPlaylist {
            match_mode: SmartMatch::Any,
            limit: Some(2),
            ..playlist
        };
        assert_eq!(
            evaluate_smart_playlist(
                &any,
                &tracks,
                &SearchIndex::build(&tracks),
                &HashMap::new(),
                &HashMap::new(),
                0,
            ),
            ["one", "three"]
        );
    }
}
