//! The order a song list or a Library grid is shown in, over the order it comes in.
//! Rows keep their numbers in the whole list, for selecting and removing.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::{LibTab, Page};
use crate::model::{Card, Track};

/// How many lists' sorts are remembered; the ones sorted longest ago are forgotten first.
const KEEP_SORTS: usize = 100;

/// The order a list is shown in. `Default` is the list's own: a playlist's, or most
/// recent first in History, Liked songs, Downloads and the Library grids.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, serde::Serialize, serde::Deserialize)]
pub enum Sort {
    #[default]
    Default,
    /// A to Z.
    Title,
    /// Z to A (the Library grids).
    TitleDesc,
    Artist,
    Album,
    /// Shortest first.
    Duration,
    /// The list's own order turned around: oldest first, where it comes newest first.
    Reversed,
}

/// Each list's chosen order, remembered, and the last orders worked out: sorting every
/// frame shows.
#[derive(Default)]
pub struct ListSorts {
    /// Each list's sort but the default, by [`list_id`]; the most recently sorted first.
    pub sorts: Vec<(String, Sort)>,
    /// A list whose sort waits until all of it is in.
    held: Option<String>,
    /// The last order worked out, the same way: its list, the tracks it came from, the rows.
    memo: RefCell<Option<(String, ListKey, Vec<usize>)>>,
    /// The Library grid's last order, the same way: its sort, its cards and their order.
    card_memo: RefCell<Option<(Sort, ListKey, Vec<usize>)>>,
}

/// The songs a list holds, by where they are and which they are.
type ListKey = (usize, usize, u64);

fn list_key(tracks: &[Track]) -> ListKey {
    let mut ids = DefaultHasher::new();
    for t in tracks {
        t.id.hash(&mut ids);
    }
    (tracks.as_ptr() as usize, tracks.len(), ids.finish())
}

impl ListSorts {
    /// How `list`'s songs are sorted.
    pub fn sort(&self, list: &str) -> Sort {
        if self.held.as_deref() == Some(list) {
            return Sort::Default;
        }
        self.sorts.iter().find(|(k, _)| k == list).map_or(Sort::Default, |(_, s)| *s)
    }

    /// Sort `list`, remembered for next time.
    pub fn set_sort(&mut self, list: &str, sort: Sort) {
        self.sorts.retain(|(k, _)| k != list);
        if sort != Sort::Default {
            self.sorts.insert(0, (list.to_owned(), sort));
            self.sorts.truncate(KEEP_SORTS);
        }
    }

    /// `held`: `list` is still coming in, so it keeps its own order for now (and plays
    /// as it arrives).
    pub fn hold(&mut self, list: &str, held: bool) {
        if held {
            self.held = Some(list.to_owned());
        } else if self.held.as_deref() == Some(list) {
            self.held = None;
        }
    }

    /// The rows of `tracks` to show, in the order `list` sorts them; `None` when it
    /// doesn't.
    pub fn order(&self, list: &str, tracks: &[Track]) -> Option<Vec<usize>> {
        let sort = self.sort(list);
        if sort == Sort::Default {
            return None;
        }
        let key = list_key(tracks);
        let mut memo = self.memo.borrow_mut();
        if let Some((_, _, rows)) = memo.as_ref().filter(|(k, l, _)| k == list && *l == key) {
            return Some(rows.clone());
        }
        let mut rows: Vec<usize> = (0..tracks.len()).collect();
        sort_rows(&mut rows, tracks, sort);
        *memo = Some((list.to_owned(), key, rows.clone()));
        Some(rows)
    }

    /// All of `tracks` in the order `list` sorts them, for its Play button; `None` when
    /// it isn't sorted.
    pub fn sorted(&self, list: &str, tracks: &[Track]) -> Option<Vec<usize>> {
        let sort = self.sort(list);
        (sort != Sort::Default).then(|| {
            let mut rows: Vec<usize> = (0..tracks.len()).collect();
            sort_rows(&mut rows, tracks, sort);
            rows
        })
    }

    /// The Library grid on `list` in its sorted order, as indices into `cards`.
    pub fn cards(&self, list: &str, cards: &[Card]) -> Vec<usize> {
        let mut rows: Vec<usize> = (0..cards.len()).collect();
        let sort = self.sort(list);
        if !matches!(sort, Sort::Title | Sort::TitleDesc) {
            return rows;
        }
        let mut titles = DefaultHasher::new();
        for c in cards {
            c.title.hash(&mut titles);
        }
        let key = (cards.as_ptr() as usize, cards.len(), titles.finish());
        let mut memo = self.card_memo.borrow_mut();
        if let Some((_, _, rows)) = memo.as_ref().filter(|(s, l, _)| *s == sort && *l == key) {
            return rows.clone();
        }
        match sort {
            Sort::Title => rows.sort_by_cached_key(|&i| fold(&cards[i].title)),
            _ => rows.sort_by_cached_key(|&i| std::cmp::Reverse(fold(&cards[i].title))),
        }
        *memo = Some((sort, key, rows.clone()));
        rows
    }
}

/// Where a list's sort is remembered: a playlist by its id (Liked Music is "LM"), a
/// Library tab by its name. Albums keep their track order and aren't sorted.
pub fn list_id(page: &Page) -> Option<String> {
    let tab = match page {
        Page::Playlist(id) => return Some(id.clone()),
        Page::Downloads => "downloads",
        Page::Library(LibTab::Songs) => super::LIKED_MUSIC,
        Page::Library(LibTab::History) => "history",
        Page::Library(LibTab::Uploads) => "uploads",
        Page::Library(LibTab::Playlists) => "playlists",
        Page::Library(LibTab::Albums) => "albums",
        Page::Library(LibTab::Artists) => "artists",
        Page::Library(LibTab::Podcasts) => "podcasts",
        _ => return None,
    };
    Some(tab.to_owned())
}

/// The orders `page`'s list can be shown in, its own order first. None for an album, which
/// keeps its track order.
pub fn options(page: &Page) -> Vec<(Sort, &'static str)> {
    // Lists that come newest first can turn around. Loading YouTube Music's own sorts just to
    // look can't be used: past the first 100 songs its pages come back unsorted. (Your own
    // playlists use its menu instead, which saves the order: see the collection page.)
    let (own, newest_first) = match page {
        Page::Playlist(id) if id == super::LIKED_MUSIC => ("Recently liked", true),
        Page::Playlist(_) => ("Custom order", false),
        Page::Library(LibTab::Songs) => ("Recently liked", true),
        Page::Library(LibTab::History) => ("Recently played", true),
        Page::Library(LibTab::Uploads) => ("Recently added", true),
        Page::Downloads => ("Newest first", true),
        Page::Library(LibTab::Playlists | LibTab::Albums | LibTab::Artists | LibTab::Podcasts) => return vec![(Sort::Default, "Recent"), (Sort::Title, "A–Z"), (Sort::TitleDesc, "Z–A")],
        _ => return Vec::new(),
    };
    let mut options = vec![(Sort::Default, own)];
    if newest_first {
        options.push((Sort::Reversed, "Oldest first"));
    }
    options.extend([(Sort::Title, "Title"), (Sort::Artist, "Artist"), (Sort::Album, "Album"), (Sort::Duration, "Duration")]);
    options
}

/// Put `rows` of `tracks` in `sort`'s order. Each song's key is made once, and songs
/// alike keep the list's own order. Songs without an artist, album or length go last.
pub fn sort_rows(rows: &mut [usize], tracks: &[Track], sort: Sort) {
    match sort {
        Sort::Default => {}
        Sort::Reversed => rows.reverse(),
        Sort::Duration => rows.sort_by_key(|&i| tracks[i].duration.unwrap_or(u32::MAX)),
        Sort::TitleDesc => rows.sort_by_cached_key(|&i| std::cmp::Reverse(fold(&tracks[i].title))),
        Sort::Title | Sort::Artist | Sort::Album => rows.sort_by_cached_key(|&i| {
            let t = &tracks[i];
            let by = match sort {
                Sort::Artist => t.artists.first().map(|a| fold(&a.name)),
                Sort::Album => t.album.as_ref().map(|a| fold(&a.name)),
                _ => None,
            };
            (sort != Sort::Title && by.is_none(), by, fold(&t.title))
        }),
    }
}

/// Lowercase without accents, so "Été" sorts among the e's.
fn fold(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    use unicode_normalization::char::is_combining_mark;
    s.nfd().filter(|c| !is_combining_mark(*c)).flat_map(char::to_lowercase).collect()
}

/// The rows a Shift-click picks: from `from` to `to` in the order on screen (`shown`, or
/// the whole list when that's `None`). Just `to` when `from` isn't on show.
pub fn run(shown: Option<&[usize]>, from: usize, to: usize) -> Vec<usize> {
    let Some(s) = shown else { return (from.min(to)..=from.max(to)).collect() };
    match (s.iter().position(|&r| r == from), s.iter().position(|&r| r == to)) {
        (Some(a), Some(b)) => s[a.min(b)..=a.max(b)].to_vec(),
        _ => vec![to],
    }
}

#[cfg(test)]
mod tests {
    use super::{KEEP_SORTS, ListSorts, Sort, run, sort_rows};
    use crate::model::{Link, Track};

    fn link(name: &str) -> Link {
        Link { name: name.into(), id: None }
    }

    fn track(title: &str, artist: &str, album: &str) -> Track {
        Track { id: title.into(), title: title.into(), artists: vec![link(artist)], album: Some(link(album)), ..Default::default() }
    }

    fn songs() -> Vec<Track> {
        vec![track("Around the World", "Daft Punk", "Homework"), track("Levitating", "Dua Lipa", "Future Nostalgia"), track("One More Time", "Daft Punk", "Discovery"), track("ÉTÉ", "Angèle", "Brol")]
    }

    fn sorted(sort: Sort, tracks: &[Track]) -> Option<Vec<usize>> {
        let mut s = ListSorts::default();
        s.set_sort("a", sort);
        s.order("a", tracks)
    }

    #[test]
    fn sorts_by_title_artist_album_and_length() {
        let mut tracks = songs();
        for (t, secs) in tracks.iter_mut().zip([300, 200, 320, 180]) {
            t.duration = Some(secs);
        }
        // Accents don't count: "ÉTÉ" is among the e's, before "Levitating".
        assert_eq!(sorted(Sort::Title, &tracks), Some(vec![0, 3, 1, 2]));
        assert_eq!(sorted(Sort::TitleDesc, &tracks), Some(vec![2, 1, 3, 0]));
        // One artist's songs by title.
        assert_eq!(sorted(Sort::Artist, &tracks), Some(vec![3, 0, 2, 1]));
        assert_eq!(sorted(Sort::Album, &tracks), Some(vec![3, 2, 1, 0]));
        assert_eq!(sorted(Sort::Duration, &tracks), Some(vec![3, 1, 0, 2]));
        assert_eq!(sorted(Sort::Default, &tracks), None);
        assert_eq!(sorted(Sort::Reversed, &tracks), Some(vec![3, 2, 1, 0]));
    }

    #[test]
    fn songs_without_an_album_or_length_go_last() {
        let mut tracks = songs();
        tracks[3].album = None;
        assert_eq!(sorted(Sort::Album, &tracks), Some(vec![2, 1, 0, 3]));
        tracks[1].duration = Some(10);
        assert_eq!(sorted(Sort::Duration, &tracks), Some(vec![1, 0, 2, 3]));
    }

    #[test]
    fn each_list_keeps_its_own_sort() {
        let mut s = ListSorts::default();
        s.set_sort("a", Sort::Title);
        s.set_sort("history", Sort::Duration);
        assert_eq!((s.sort("a"), s.sort("history")), (Sort::Title, Sort::Duration));
        // Back to the default is forgotten, and only so many are remembered.
        s.set_sort("a", Sort::Default);
        assert_eq!(s.sorts.len(), 1);
        for i in 0..KEEP_SORTS {
            s.set_sort(&i.to_string(), Sort::Album);
        }
        assert_eq!(s.sorts.len(), KEEP_SORTS);
        assert_eq!(s.sort("history"), Sort::Default);
    }

    #[test]
    fn a_list_coming_in_keeps_its_own_order() {
        let mut s = ListSorts::default();
        s.set_sort("a", Sort::Title);
        s.hold("a", true);
        assert_eq!(s.order("a", &songs()), None);
        s.hold("a", false);
        assert_eq!(s.order("a", &songs()), Some(vec![0, 3, 1, 2]));
    }

    #[test]
    fn shift_picks_the_rows_between_in_the_order_on_screen() {
        assert_eq!(run(None, 0, 2), vec![0, 1, 2]);
        assert_eq!(run(None, 2, 0), vec![0, 1, 2]);
        // Turned around, the run on screen is turned around too.
        assert_eq!(run(Some(&[2, 1, 0]), 0, 2), vec![2, 1, 0]);
        // An anchor that isn't on show picks just the row clicked.
        assert_eq!(run(Some(&[0, 2]), 1, 2), vec![2]);
    }

    #[test]
    fn sort_rows_puts_missing_keys_last() {
        let mut tracks = vec![Track { id: "a".into(), title: "B".into(), artists: vec![], album: None, duration: None, ..Default::default() }];
        tracks.push(Track { id: "b".into(), title: "A".into(), artists: vec![link("X")], album: Some(link("Y")), duration: Some(60), ..Default::default() });
        let mut rows = vec![0, 1];
        sort_rows(&mut rows, &tracks, Sort::Artist);
        assert_eq!(rows, vec![1, 0], "a song without an artist goes last");
    }
}
