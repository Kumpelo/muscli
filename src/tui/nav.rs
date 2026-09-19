//! The navigation stack.
//!
//! Detail views open on top of whatever you were looking at, and more than one
//! can be open at a time: an album reached from an artist reached from a genre
//! is three levels deep, and closing each one has to land back where it came
//! from. A stack says that directly.
//!
//! Each level used to be three parallel fields (`opened_*`, `*_return_selection`,
//! `*_parent_view`), one set per kind of detail view. That only ever remembered
//! one level per kind, so opening an album from an artist reached from an album
//! overwrote the first album's return path.

use super::*;

/// What a detail view was opened to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NavTarget {
    Album(String),
    Artist(String),
    Genre(String),
    SmartPlaylist(i64),
}

/// One step into a detail view.
#[derive(Debug, Clone)]
pub(super) struct NavFrame {
    /// The view to return to when this level closes.
    pub(super) parent: View,
    /// Where the cursor was in the parent. Only a fallback: closing prefers to
    /// re-find the item that was opened, because the list may have changed
    /// underneath while a rescan ran.
    pub(super) selection: usize,
    pub(super) target: NavTarget,
}

/// The innermost open frame of a given kind.
///
/// Searching from the top down rather than only at the top is what lets an
/// album opened from an artist still know which artist it belongs to.
fn innermost<'a, T>(nav: &'a [NavFrame], pick: impl Fn(&'a NavTarget) -> Option<T>) -> Option<T> {
    nav.iter().rev().find_map(|frame| pick(&frame.target))
}

impl App {
    /// Open `view` showing `target`, remembering how to get back.
    pub(super) fn push_nav(&mut self, target: NavTarget, view: View) {
        self.nav.push(NavFrame {
            parent: self.view,
            selection: self.selected,
            target,
        });
        self.view = view;
        self.selected = 0;
        self.focus = Focus::Content;
        self.dirty = true;
    }

    /// Close the current detail view, returning the frame that was open.
    pub(super) fn pop_nav(&mut self) -> Option<NavFrame> {
        let frame = self.nav.pop()?;
        self.view = frame.parent;
        self.dirty = true;
        Some(frame)
    }

    /// Leave every detail view, as picking a destination in the sidebar does.
    pub(super) fn clear_nav(&mut self) {
        self.nav.clear();
        self.artist_release_keys.clear();
    }

    pub(super) fn opened_album_key(&self) -> Option<&str> {
        innermost(&self.nav, |target| match target {
            NavTarget::Album(key) => Some(key.as_str()),
            _ => None,
        })
    }

    pub(super) fn opened_artist_name(&self) -> Option<&str> {
        innermost(&self.nav, |target| match target {
            NavTarget::Artist(name) => Some(name.as_str()),
            _ => None,
        })
    }

    pub(super) fn opened_genre_name(&self) -> Option<&str> {
        innermost(&self.nav, |target| match target {
            NavTarget::Genre(name) => Some(name.as_str()),
            _ => None,
        })
    }

    pub(super) fn opened_smart_playlist(&self) -> Option<i64> {
        innermost(&self.nav, |target| match target {
            NavTarget::SmartPlaylist(id) => Some(*id),
            _ => None,
        })
    }

    /// The view the current detail level will return to.
    pub(super) fn nav_parent(&self) -> Option<View> {
        self.nav.last().map(|frame| frame.parent)
    }

    /// Close the innermost detail view, whatever kind it is.
    pub(super) fn close_detail(&mut self) {
        match self.view {
            View::AlbumDetail => self.close_album_detail(),
            View::ArtistDetail => self.close_artist_detail(),
            View::GenreDetail | View::SmartPlaylistDetail => {
                if let Some(frame) = self.pop_nav() {
                    self.selected = frame.selection.min(self.item_count().saturating_sub(1));
                }
            }
            _ => {}
        }
    }

    /// Drop levels whose target no longer exists.
    ///
    /// A rescan can delete the album that is currently open. Unwinding from the
    /// top means a deep path collapses only as far as it has to: an album that
    /// vanished closes back to its artist, not all the way home.
    pub(super) fn prune_nav(&mut self) {
        while let Some(frame) = self.nav.last() {
            let alive = match &frame.target {
                NavTarget::Album(key) => self.album_index.contains_key(key),
                NavTarget::Artist(name) => self.artist_index.contains_key(name),
                NavTarget::Genre(name) => self.genres.iter().any(|genre| &genre.name == name),
                NavTarget::SmartPlaylist(id) => {
                    self.smart_playlists.iter().any(|list| list.id == *id)
                }
            };
            if alive {
                break;
            }
            let Some(frame) = self.pop_nav() else { break };
            self.selected = frame.selection;
        }
        self.refresh_artist_releases();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(parent: View, selection: usize, target: NavTarget) -> NavFrame {
        NavFrame {
            parent,
            selection,
            target,
        }
    }

    fn album(key: &str) -> NavTarget {
        NavTarget::Album(key.into())
    }

    fn artist(name: &str) -> NavTarget {
        NavTarget::Artist(name.into())
    }

    fn album_key(nav: &[NavFrame]) -> Option<&str> {
        innermost(nav, |target| match target {
            NavTarget::Album(key) => Some(key.as_str()),
            _ => None,
        })
    }

    fn artist_name(nav: &[NavFrame]) -> Option<&str> {
        innermost(nav, |target| match target {
            NavTarget::Artist(name) => Some(name.as_str()),
            _ => None,
        })
    }

    #[test]
    fn nothing_is_open_at_the_top_level() {
        assert_eq!(album_key(&[]), None);
        assert_eq!(artist_name(&[]), None);
    }

    #[test]
    fn an_album_opened_from_an_artist_still_knows_its_artist() {
        // The album sits on top, but closing it has to find the album inside
        // that artist's releases, so the artist below must stay reachable.
        let nav = [
            frame(View::Artists, 3, artist("Bowie")),
            frame(View::ArtistDetail, 1, album("bowie-low")),
        ];
        assert_eq!(album_key(&nav), Some("bowie-low"));
        assert_eq!(artist_name(&nav), Some("Bowie"));
    }

    #[test]
    fn nesting_keeps_every_level_distinct() {
        // The case the old fields could not represent: three levels alternating
        // kinds meant the second album overwrote the first one's return path,
        // because there was exactly one album_parent_view for the whole app.
        let nav = [
            frame(View::Albums, 7, album("first")),
            frame(View::AlbumDetail, 2, artist("Guest")),
            frame(View::ArtistDetail, 0, album("second")),
        ];

        assert_eq!(album_key(&nav), Some("second"), "the innermost album wins");
        assert_eq!(artist_name(&nav), Some("Guest"));

        // Unwinding one level at a time lands back on each parent in turn.
        assert_eq!(nav[2].parent, View::ArtistDetail);
        assert_eq!(album_key(&nav[..2]), Some("first"));
        assert_eq!(nav[1].parent, View::AlbumDetail);
        assert_eq!(nav[1].selection, 2);
        assert_eq!(nav[0].parent, View::Albums);
        assert_eq!(nav[0].selection, 7, "the outermost return index survives");
    }

    #[test]
    fn a_genre_stays_reachable_under_an_artist_and_an_album() {
        let nav = [
            frame(View::Genres, 4, NavTarget::Genre("Jazz".into())),
            frame(View::GenreDetail, 2, artist("Mingus")),
            frame(View::ArtistDetail, 0, album("ah-um")),
        ];
        let genre = innermost(&nav, |target| match target {
            NavTarget::Genre(name) => Some(name.as_str()),
            _ => None,
        });
        assert_eq!(genre, Some("Jazz"));
        assert_eq!(artist_name(&nav), Some("Mingus"));
        assert_eq!(album_key(&nav), Some("ah-um"));
    }
}
