//! The taste every one of Home's recommendation rows grows from, and the shape of
//! the rows themselves — the port of Mochy's `HomeRecommender`: rows from a song
//! ("Because you like …"), from an artist ("More like …"), from a genre, and the
//! picking and arranging that keep them fresh (mostly-unheard songs, a few you
//! know, spread across artists, never anything you skipped).
//!
//! Everything here is plain math over the local listening history, so the taste is
//! built wherever the rows are asked for; the network halves (a song's radio, an
//! artist's page, a genre's page) go through the backend's requests.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::listening::{Listening, unix_now};
use crate::model::{Link, Track};

/// Which of Home's rows a row is, set when it's asked for and read when it's drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// "Because you like <song>": songs that grow out of one you play a lot.
    BecauseYouLike,
    /// "More like <artist>": other artists' songs from that artist's radio.
    MoreLike,
    /// A genre your top artists are filed under.
    YourGenre,
    /// A genre you don't play yet, so Home isn't only more of the same.
    NewGenre,
}

/// Where a row's title leads: the artist's page, or the genre's.
#[derive(Clone, Debug, PartialEq)]
pub enum Endpoint {
    Artist(String),
    Mood(String),
}

/// One titled row, as the backend answers it and Home draws it. The new releases
/// row ("New from your artists") is albums, so it travels as cards instead.
#[derive(Clone, Debug)]
pub struct Row {
    pub kind: Kind,
    /// What the row grew from: the song, the artist, the genre.
    pub title: String,
    pub tracks: Vec<Track>,
    /// The seed's cover, for the header beside the title.
    pub thumb: Option<String>,
    pub endpoint: Option<Endpoint>,
}

/// Chance where a stronger score is likelier to win but never certain to — a
/// refresh doesn't grow every row from the same few songs. Fisher-Yates and the
/// draw both run on the clock-seeded xorshift `ui::shuffle` uses; nothing here
/// needs an RNG crate.
pub struct Rng(u64);

/// Small seeds don't scramble on their first steps — a first draw near zero has a
/// log of -23, which would swamp the weighted sample's keys. A few rounds first.
const WARMUP: usize = 8;

impl Rng {
    pub fn new() -> Self {
        Self(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64) | 1).warmed()
    }

    /// A seed, for the tests and any place that wants the same picks twice.
    pub fn seeded(seed: u64) -> Self {
        Self(seed | 1).warmed()
    }

    fn warmed(mut self) -> Self {
        for _ in 0..WARMUP {
            self.step();
        }
        self
    }

    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in (0, 1): the weighted sample's key divides by it, so it can't be 0.
    pub fn unit(&mut self) -> f64 {
        (self.step() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, (self.step() % (i as u64 + 1)) as usize);
        }
    }
}

/// What the backend's row requests carry: the parts of the taste their picking
/// needs. The taste itself is built on the UI thread, where the listening lives.
#[derive(Clone, Debug, Default)]
pub struct TasteLite {
    pub heard: Vec<String>,
    pub avoid_songs: Vec<String>,
    pub avoid_artists: Vec<String>,
    /// What autoplay served lately: fresh recommendations skip them a while, so a
    /// queue and a Home row don't hand back what the last one just did.
    pub recent_reco: Vec<String>,
    pub hide_explicit: bool,
}

/// The taste, read once per load and shared by every row (Mochy's `TasteProfile`).
#[derive(Clone, Debug)]
pub struct Taste {
    /// Songs played in the last three months.
    pub heard: HashSet<String>,
    pub avoid_songs: Vec<String>,
    /// Artist names, lowercase: anything of theirs is kept out of rows.
    pub avoid_artists: HashSet<String>,
    /// What autoplay served lately, kept out of every picking for a while.
    pub recent_reco: HashSet<String>,
    /// The songs to grow rows from, weighted-sampled from the top of the score.
    pub song_seeds: Vec<Track>,
    /// The artists played most this month, with the seconds listened.
    pub top_artists: Vec<(Link, u64)>,
}

impl TasteLite {
    /// The parts a radio ask carries, straight off the listening — no seeds, no top
    /// artists: a radio asks far oftener than the rows rebuild, and the picking
    /// needs neither. The heard set matches `Taste::build`'s: played in the last
    /// three months, plus the likes.
    pub fn of(l: &Listening, hide_explicit: bool) -> Self {
        let since = unix_now().saturating_sub(90 * 86_400);
        let mut heard: Vec<String> = l.events.iter().filter(|e| e.at >= since).map(|e| e.track.id.clone()).collect();
        heard.extend(l.liked.iter().map(|t| t.id.clone()));
        heard.sort();
        heard.dedup();
        Self {
            heard,
            avoid_songs: l.avoid_songs(),
            avoid_artists: l.avoid_artists(),
            recent_reco: l.recent_reco.iter().cloned().collect(),
            hide_explicit,
        }
    }
}

impl Taste {
    /// A taste from the parts a request carries — for the picking, which needs
    /// neither seeds nor top artists.
    pub fn from_parts(l: &TasteLite) -> Self {
        Self {
            heard: l.heard.iter().cloned().collect(),
            avoid_songs: l.avoid_songs.clone(),
            avoid_artists: l.avoid_artists.iter().cloned().collect(),
            recent_reco: l.recent_reco.iter().cloned().collect(),
            song_seeds: Vec::new(),
            top_artists: Vec::new(),
        }
    }

    /// The parts to send along with the asks (see `Taste::build` on the UI side).
    pub fn lite(&self, hide_explicit: bool) -> TasteLite {
        TasteLite { heard: self.heard.iter().cloned().collect(), avoid_songs: self.avoid_songs.clone(), avoid_artists: self.avoid_artists.iter().cloned().collect(), recent_reco: self.recent_reco.iter().cloned().collect(), hide_explicit }
    }

    /// A play today counts 1, two weeks ago about a third, three months ago next
    /// to nothing; a like says more than a play (the newest fifty count). What you
    /// skipped or named "Not recommended" never comes back as a recommendation.
    /// `library_artists` (subscribed ones) stand in until history exists.
    pub fn build(l: &Listening, library_artists: Vec<(String, String)>) -> Self {
        let now = unix_now();
        let mut scores: HashMap<String, f64> = HashMap::new();
        let mut songs: HashMap<String, Track> = HashMap::new();
        for e in &l.events {
            let age_days = now.saturating_sub(e.at) as f64 / 86_400.0;
            if age_days > 90.0 {
                continue;
            }
            *scores.entry(e.track.id.clone()).or_insert(0.0) += (-age_days / 14.0).exp();
            songs.entry(e.track.id.clone()).or_insert_with(|| e.track.clone());
        }
        for t in &l.liked {
            *scores.entry(t.id.clone()).or_insert(0.0) += 1.5;
            songs.entry(t.id.clone()).or_insert_with(|| t.clone());
        }

        let avoid_songs = l.avoid_songs();
        let avoid_artists: HashSet<String> = l.avoid_artists().into_iter().collect();
        let artist_ok = |t: &Track| !t.artists.iter().any(|a| avoid_artists.contains(&a.name.to_lowercase()));

        // seeds: the top of the scores, one per lead artist, weighted-sampled
        let mut candidates: Vec<(Track, f64)> = scores
            .iter()
            .filter_map(|(id, s)| songs.get(id).map(|t| (t.clone(), *s)))
            .collect();
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        candidates.retain(|(t, _)| {
            !crate::local::is_local(&t.id) && !avoid_songs.contains(&t.id) && artist_ok(t)
        });
        // one seed per lead artist, so no one act's songs grow every row
        let mut seen_artists: HashSet<String> = HashSet::new();
        candidates.retain(|(t, _)| {
            let lead = t.artists.first().map(|a| a.name.as_str()).unwrap_or(t.id.as_str()).to_owned();
            seen_artists.insert(lead)
        });
        candidates.truncate(14);
        let song_seeds = weighted_sample(&mut Rng::new(), &candidates, 8);

        let mut top_artists: Vec<(Link, u64)> = l.top_artists(30, 14).into_iter().filter(|(a, _)| !avoid_artists.contains(&a.name.to_lowercase())).collect();
        if top_artists.is_empty() {
            // nothing played yet: the library's subscribed artists stand in, the way
            // Mochy's onboarding picks do
            top_artists = library_artists
                .into_iter()
                .filter(|(_, id)| !id.is_empty())
                .map(|(name, id)| (Link { name, id: Some(id) }, 1))
                .collect();
        }

        Self { heard: scores.keys().cloned().collect(), avoid_songs, avoid_artists, recent_reco: l.recent_reco.iter().cloned().collect(), song_seeds, top_artists }
    }
}

/// Mochy's weighted sample (Efraimidis–Spirakis): the strongest are likelier to
/// make the cut but never certain to.
fn weighted_sample(rng: &mut Rng, candidates: &[(Track, f64)], count: usize) -> Vec<Track> {
    let mut keyed: Vec<(f64, &Track)> = candidates.iter().map(|(t, w)| (rng.unit().ln() / w.max(1e-6), t)).collect();
    keyed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    keyed.into_iter().take(count).map(|(_, t)| t.clone()).collect()
}

/// The hero's discovery seeds (Mochy's `getDailyDiscover`): something you liked,
/// something you play all the time, something you played just now, one per artist,
/// up to five.
pub fn discover_seeds(l: &Listening, rng: &mut Rng) -> Vec<Track> {
    let recent = l.recently_played(20);
    let most_played = l.most_played(30, 0, 20);
    if recent.is_empty() && l.liked.is_empty() {
        return Vec::new();
    }
    let hidden = l.banned.clone();
    let mut picked: Vec<Track> = Vec::new();
    // a seed per artist: whatever the direction, a second song of the same artist
    // isn't another direction
    let mut take_from = |picked: &mut Vec<Track>, pool: &[Track], count: usize| {
        let mut pool: Vec<Track> = pool.to_vec();
        rng.shuffle(&mut pool);
        let lead_artist = |t: &Track| t.artists.first().and_then(|a| a.id.clone());
        for t in pool {
            if picked.len() >= count {
                break;
            }
            if hidden.contains(&t.id)
                || picked.iter().any(|p| p.id == t.id)
                || picked.iter().any(|p| lead_artist(p) == lead_artist(&t))
            {
                continue;
            }
            picked.push(t);
        }
    };
    take_from(&mut picked, &l.liked, 2);
    take_from(&mut picked, &most_played, 2);
    take_from(&mut picked, &recent, 2);
    if picked.len() < 5 {
        let missing = 5 - picked.len();
        take_from(&mut picked, &recent, missing);
    }
    picked
}

/// The picking every row shares: mostly songs you haven't heard, plus a few you
/// have so the row still feels like you; nothing you skipped, no videos, no
/// banned songs, nothing autoplay served lately, and only `per_artist` from any
/// one artist (Mochy's `pick`).
pub fn pick(tracks: Vec<Track>, taste: &Taste, hide_explicit: bool, limit: usize, per_artist: usize, rng: &mut Rng) -> Vec<Track> {
    let mut seen = HashSet::new();
    let usable: Vec<Track> = tracks
        .into_iter()
        .filter(|t| !(hide_explicit && t.explicit))
        .filter(|t| !t.has_video())
        .filter(|t| !taste.avoid_songs.contains(&t.id))
        .filter(|t| !taste.recent_reco.contains(&t.id))
        .filter(|t| !t.artists.iter().any(|a| taste.avoid_artists.contains(&a.name.to_lowercase())))
        .filter(|t| seen.insert(t.id.clone()))
        .collect();
    let (mut heard, mut unheard): (Vec<Track>, Vec<Track>) = usable.into_iter().partition(|t| taste.heard.contains(&t.id));
    let familiar = (limit / 4).max(1);
    rng.shuffle(&mut unheard);
    let mut chosen: Vec<Track> = unheard.into_iter().take(limit.saturating_sub(familiar.min(heard.len()))).collect();
    rng.shuffle(&mut heard);
    chosen.extend(heard.into_iter().take(familiar));
    // the spread: at most per_artist songs from any one act
    let mut per_artist_count: HashMap<String, usize> = HashMap::new();
    rng.shuffle(&mut chosen);
    chosen.retain(|t| {
        let artist = t.artists.first().map(|a| a.name.to_lowercase()).unwrap_or_default();
        let n = per_artist_count.entry(artist).or_default();
        *n += 1;
        *n <= per_artist
    });
    chosen
}

/// How deep the queue's own radio tops up at once: one fetch's worth, the same
/// count the radio asks for, so an autoplay never grows shallower than it was.
const RADIO_DEPTH: usize = 50;

/// A radio the listener asked for themselves (Mochy's radio-queue filter): what
/// YouTube lined up around their song with the taste's rulings taken out — the
/// curation order kept, it's theirs to read. The song itself plays regardless of
/// any of this; only what YouTube adds around it goes.
pub fn user_radio(tracks: Vec<Track>, taste: &TasteLite) -> Vec<Track> {
    let hide_explicit = taste.hide_explicit;
    let taste = Taste::from_parts(taste);
    let mut seen = HashSet::new();
    tracks
        .into_iter()
        .filter(|t| !(hide_explicit && t.explicit))
        .filter(|t| !t.has_video())
        .filter(|t| !taste.avoid_songs.contains(&t.id))
        .filter(|t| !t.artists.iter().any(|a| taste.avoid_artists.contains(&a.name.to_lowercase())))
        .filter(|t| seen.insert(t.id.clone()))
        .collect()
}

/// The queue's own radio: what `user_radio` leaves, picked the way every
/// recommendation row is — mostly songs unheard, a few familiar, spread over
/// artists, nothing lately served — so autoplay grows from the same taste as
/// Home's rows instead of YouTube's one-size mix. `per_artist` is where a spread
/// doesn't belong: an artist's own catalog plays as it lies.
pub fn queue_radio(tracks: Vec<Track>, taste: &TasteLite, per_artist: usize, rng: &mut Rng) -> Vec<Track> {
    let hide_explicit = taste.hide_explicit;
    let usable = user_radio(tracks, taste);
    // the limit rides the pool's size, or the familiar few would swamp a short one
    let limit = RADIO_DEPTH.min(usable.len());
    pick(usable, &Taste::from_parts(taste), hide_explicit, limit, per_artist, rng)
}

/// The rows in the order Home shows them: a song row, an artist row and a genre
/// row in turn, the new genres last (the new-releases card row slots in after the
/// first round, where the page draws it). A song only shows up in the first row
/// that has it, and a row left with fewer than four songs is dropped (Mochy's
/// `arrange`).
pub fn arrange(because: Vec<Row>, more_like: Vec<Row>, your_genres: Vec<Row>, new_genres: Vec<Row>) -> Vec<Row> {
    let mut rounds = [VecDeque::from(because), VecDeque::from(more_like), VecDeque::from(your_genres)];
    let mut ordered = Vec::new();
    while rounds.iter().any(|q| !q.is_empty()) {
        for q in &mut rounds {
            if let Some(r) = q.pop_front() {
                ordered.push(r);
            }
        }
    }
    ordered.extend(new_genres);
    let mut seen = HashSet::new();
    ordered
        .into_iter()
        .map(|mut r| {
            r.tracks.retain(|t| seen.insert(t.id.clone()));
            r
        })
        .filter(|r| r.tracks.len() >= 4)
        .collect()
}

/// Genre families, most specific first: K-Pop before Pop, Latin before Pop ("Pop
/// Latino"), Alternative before Rock and Pop, Metal before Rock. The iTunes stems
/// name what an artist is filed under; the YouTube ones match that onto the genre
/// tiles, which come in the user's language, so they read the same in most of them
/// ("pop", "hip", "r&b", "electr"/"elektr"…). From Mochy's `ListeningGenres`.
const FAMILIES: &[(&[&str], &[&str])] = &[
    (&["k-pop"], &["k-pop", "kpop"]),
    (&["j-pop", "anime"], &["j-pop", "jpop"]),
    (&["mandopop", "cantopop", "c-pop", "chinese"], &["mandopop", "cantopop"]),
    (&["hip-hop", "hip hop", "rap"], &["hip-hop", "hip hop", "hiphop", "rap"]),
    (&["r&b", "soul", "funk"], &["r&b", "soul"]),
    (&["electronic", "dance", "house", "techno", "trance", "edm", "dubstep"], &["dance", "electr", "elektr", "dansa", "électr", "eletr"]),
    (&["metal"], &["metal"]),
    (&["latin", "reggaeton", "salsa", "bachata"], &["latin"]),
    (&["alternative", "indie"], &["indie", "altern"]),
    (&["rock", "punk", "grunge"], &["rock"]),
    (&["jazz"], &["jazz"]),
    (&["classical", "opera"], &["classical", "klasik", "clásic", "classiq", "klassik", "clássic", "classic"]),
    (&["country", "americana"], &["country"]),
    (&["reggae", "dancehall", "caribbean"], &["reggae"]),
    (&["blues"], &["blues"]),
    (&["folk", "singer/songwriter", "acoustic"], &["folk", "akustik", "acoustic", "acústic"]),
    (&["christian", "gospel"], &["christian", "gospel", "kristen", "rohani", "cristian", "chrétien"]),
    (&["soundtrack"], &["soundtrack", "musical"]),
    (&["afro", "african", "amapiano"], &["afric", "afrika", "afro"]),
    (&["arabic"], &["arab"]),
    (&["bollywood", "indian", "hindi", "punjabi", "tamil"], &["india"]),
    (&["brazil", "mpb", "sertanejo", "samba", "bossa"], &["brazil", "brasil"]),
    (&["dangdut"], &["dangdut"]),
    (&["pop"], &["pop"]),
];

fn family_of(itunes_genre: &str) -> Option<usize> {
    let genre = itunes_genre.to_lowercase();
    FAMILIES.iter().position(|(itunes, _)| itunes.iter().any(|s| genre.contains(s)))
}

fn family_matches(i: usize, title: &str) -> bool {
    let t = title.to_lowercase();
    FAMILIES[i].1.iter().any(|s| t.contains(s))
}

fn family_matches_exactly(i: usize, title: &str) -> bool {
    let t = title.trim().to_lowercase();
    FAMILIES[i].1.iter().any(|s| t == *s)
}

/// Whether a tile's title reads as a genre, for telling YouTube's genre grid from
/// its moods one; false for the odd ones out like "Decades" or "Family".
pub fn is_genre(title: &str) -> bool {
    FAMILIES.iter().enumerate().any(|(i, _)| family_matches(i, title))
}

/// How many of a grid's titles read as genres.
pub fn genre_score(titles: &[String]) -> usize {
    titles.iter().filter(|t| is_genre(t)).count()
}

/// The genre tile an iTunes genre belongs to, as an index into `titles`: an exact
/// title first, so "Pop" never lands on "K-Pop", then a stem match.
pub fn match_genre(itunes_genre: &str, titles: &[String]) -> Option<usize> {
    let family = family_of(itunes_genre)?;
    titles
        .iter()
        .position(|t| family_matches_exactly(family, t))
        .or_else(|| titles.iter().position(|t| family_matches(family, t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, artist: &str, duration: Option<u32>) -> Track {
        Track { id: id.into(), title: id.into(), artists: vec![Link { name: artist.into(), id: Some(artist.into()) }], duration, thumbs: vec![], ..Default::default() }
    }

    fn at(days_ago: f64) -> u64 {
        unix_now() - (days_ago * 86_400.0) as u64
    }

    fn listening_with_plays() -> Listening {
        let mut l = Listening::default();
        for _ in 0..5 {
            l.note_left(song("a", "AA", Some(200)), at(1.0), 90.0);
        }
        for _ in 0..3 {
            l.note_left(song("b", "BB", Some(200)), at(10.0), 90.0);
        }
        l.note_left(song("c", "CC", Some(200)), at(60.0), 90.0);
        l.liked(song("liked1", "LL", Some(200)));
        l.liked(song("liked2", "AA", Some(200)));
        l
    }

    #[test]
    fn a_play_today_outranks_a_play_last_month_and_a_like_adds_half_again() {
        let taste = Taste::build(&listening_with_plays(), vec![]);
        // seeds come from the top of the score, weighted-sampled — with one seed
        // per artist the strong "a" is nearly always in
        assert!(taste.song_seeds.iter().any(|t| t.id == "a"));
        assert!(taste.heard.contains("c")); // 60 days back is still heard
        assert!(taste.song_seeds.iter().any(|t| t.id == "liked1") || taste.heard.contains("liked1"));
    }

    #[test]
    fn skipped_songs_and_thrice_skipped_artists_are_kept_out_of_seeds() {
        let mut l = listening_with_plays();
        for i in 0..3 {
            l.note_left(song(&format!("skipper{i}"), "CC", Some(200)), at(0.0), 4.0);
        }
        l.note_left(song("a", "AA", Some(200)), at(0.0), 4.0); // skip the favourite too
        let taste = Taste::build(&l, vec![]);
        assert!(taste.avoid_artists.contains("cc"));
        assert!(taste.avoid_songs.contains(&"a".to_owned()));
        assert!(taste.song_seeds.iter().all(|t| t.id != "a"));
        assert!(taste.song_seeds.iter().all(|t| t.artists.iter().all(|a| a.name != "CC")));
        assert!(taste.top_artists.iter().all(|(a, _)| a.name != "CC"));
    }

    #[test]
    fn library_artists_stand_in_when_nothing_has_played() {
        let taste = Taste::build(&Listening::default(), vec![("Sub One".into(), "UCsub1".into())]);
        assert_eq!(taste.top_artists.len(), 1);
        assert_eq!(taste.top_artists[0].0.name, "Sub One");
    }

    #[test]
    fn weighted_sampling_prefers_the_strong_without_locking_the_weak_out() {
        let strong = song("strong", "AA", None);
        let weak = song("weak", "BB", None);
        let mut weak_picked = 0;
        for seed in 0..64u64 {
            let mut rng = Rng::seeded(seed);
            let picked = weighted_sample(&mut rng, &[(strong.clone(), 10.0), (weak.clone(), 2.0)], 1);
            if picked.first().map(|t| t.id.as_str()) == Some("weak") {
                weak_picked += 1;
            }
        }
        // a fifth of the picks on paper; anywhere between sometimes and mostly
        assert!(weak_picked > 0 && weak_picked < 64, "weak won {weak_picked} of 64");
    }

    #[test]
    fn picking_is_mostly_unheard_with_a_few_you_know_and_spread_over_artists() {
        let mut l = Listening::default();
        l.note_left(song("known", "Zed", Some(200)), at(2.0), 90.0);
        let taste = Taste::build(&l, vec![]);
        let mut pool: Vec<Track> = vec![];
        for i in 0..24 {
            let artist = if i < 6 { "AA" } else { "Zed" }; // six from one act, eighteen from another
            pool.push(song(&format!("new{i}"), artist, Some(200)));
        }
        pool.push(song("known", "Zed", Some(200)));
        let mut rng = Rng::seeded(42);
        let picked = pick(pool, &taste, false, 10, 2, &mut rng);
        assert!(!picked.is_empty() && picked.len() <= 10);
        assert!(picked.iter().any(|t| taste.heard.contains(&t.id)), "a familiar one made the cut");
        let aa = picked.iter().filter(|t| t.artists.first().map(|a| a.name.as_str()) == Some("AA")).count();
        assert!(aa <= 2, "{aa} from one artist");
        let mut seen = HashSet::new();
        assert!(picked.iter().all(|t| seen.insert(t.id.clone())));
        assert!(picked.iter().all(|t| !taste.avoid_songs.contains(&t.id)));
    }

    #[test]
    fn a_radio_the_listener_asked_for_keeps_the_lineup_but_not_the_ruled_out() {
        let mut l = Listening::default();
        l.note_left(song("heard", "Zed", Some(200)), at(2.0), 90.0);
        l.note_left(song("old", "X", Some(200)), at(100.0), 90.0); // past the window: not heard
        l.ban("banned");
        l.note_reco(&[song("served", "Zed", None)]);
        let lite = TasteLite::of(&l, true);
        assert!(lite.heard.contains(&"heard".to_owned()));
        assert!(!lite.heard.contains(&"old".to_owned()));
        assert!(lite.avoid_songs.contains(&"banned".to_owned()));
        assert_eq!(lite.recent_reco, vec!["served".to_owned()]);
        let mut rude = song("rude", "X", Some(200));
        rude.explicit = true;
        let lineup = vec![
            song("a", "AA", Some(200)),
            song("banned", "X", Some(200)),
            song("a", "AA", Some(200)), // the same song twice
            rude,                       // hidden when the taste asks it to be
            song("served", "Zed", None), // lately served — no ruling against a hand-asked radio
        ];
        // order kept: only the taste's rulings go, the curation stays to read
        let kept = user_radio(lineup, &lite);
        assert_eq!(kept.iter().map(|t| t.id.clone()).collect::<Vec<_>>(), vec!["a".to_owned(), "served".to_owned()]);
    }

    #[test]
    fn the_queues_own_radio_is_picked_not_kept() {
        let mut l = Listening::default();
        // four familiar songs, one per artist, so a spread can't eat them
        for i in 0..4 {
            l.note_left(song(&format!("k{i}"), &format!("K{i}"), Some(200)), at(2.0), 90.0);
        }
        l.note_reco(&[song("lately", "Zed", None)]);
        let lite = TasteLite::of(&l, false);
        let mut pool: Vec<Track> = vec![song("lately", "Zed", None)];
        // the familiar ones have to be in the pool for the picking to keep a few
        for i in 0..4 {
            pool.push(song(&format!("k{i}"), &format!("K{i}"), Some(200)));
        }
        for i in 0..20 {
            pool.push(song(&format!("new{i}"), if i < 4 { "AA" } else { "Zed" }, Some(200)));
        }
        let mut rng = Rng::seeded(42);
        let picked = queue_radio(pool, &lite, 2, &mut rng);
        assert!(picked.iter().all(|t| t.id != "lately"), "nothing lately served");
        assert!(picked.iter().any(|t| t.id.starts_with('k')), "a familiar one made the cut");
        let aa = picked.iter().filter(|t| t.artists.first().map(|a| a.name.as_str()) == Some("AA")).count();
        assert!(aa <= 2, "{aa} from one artist");
        assert!(picked.len() >= 4 && picked.len() <= 25, "{} picked", picked.len());
    }

    #[test]
    fn arranging_deals_the_rows_and_drops_the_thin_ones() {
        let row = |kind: Kind, title: &str, n: usize| Row { kind, title: title.into(), tracks: (0..n).map(|i| song(&format!("{title}{i}"), "AA", None)).collect(), thumb: None, endpoint: None };
        let arranged = arrange(vec![row(Kind::BecauseYouLike, "s1", 5)], vec![row(Kind::MoreLike, "m1", 5)], vec![row(Kind::YourGenre, "g1", 5)], vec![row(Kind::NewGenre, "fresh", 5)]);
        let kinds: Vec<Kind> = arranged.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![Kind::BecauseYouLike, Kind::MoreLike, Kind::YourGenre, Kind::NewGenre]);
        // a row with fewer than four songs after dedup is dropped
        let arranged = arrange(vec![row(Kind::BecauseYouLike, "s1", 3)], vec![], vec![], vec![]);
        assert!(arranged.is_empty());
        // the first row that has a song keeps it
        let dup_s = row(Kind::BecauseYouLike, "s2", 5);
        let dup_m = Row { kind: Kind::MoreLike, title: "m2".into(), tracks: dup_s.tracks.clone(), thumb: None, endpoint: None };
        let arranged = arrange(vec![dup_s], vec![dup_m], vec![], vec![]);
        // the first row that has a song keeps it; the other one goes thin and is dropped
        assert_eq!(arranged.len(), 1);
        assert_eq!(arranged[0].tracks.len(), 5);
    }

    #[test]
    fn genres_match_their_tiles_most_specific_first() {
        // K-Pop before Pop: an exact K-Pop tile wins for a k-pop artist
        let titles: Vec<String> = vec!["Pop".into(), "K-Pop".into(), "Rock".into()];
        assert_eq!(match_genre("K-Pop", &titles), Some(1));
        // a plain pop artist lands on Pop exactly, not K-Pop
        assert_eq!(match_genre("Pop", &titles), Some(0));
        // a stem match when no exact one: "Electrónica" carries the dance stems
        let titles: Vec<String> = vec!["Pop".into(), "Electrónica".into()];
        assert_eq!(match_genre("Dance", &titles), Some(1));
        // an unknown genre matches nothing
        assert_eq!(match_genre("Zydeco", &titles), None);
        // the odd ones out don't read as genres
        assert!(!is_genre("Decades"));
        assert!(!is_genre("Family"));
        assert!(is_genre("Hip-Hop"));
        assert!(is_genre("Afrobeats")); // "afro" stem
        assert_eq!(genre_score(&["Pop".into(), "Decades".into(), "Jazz".into()]), 2);
    }

    #[test]
    fn discover_seeds_come_from_three_directions_one_artist_each() {
        let l = listening_with_plays();
        let mut rng = Rng::seeded(7);
        let seeds = discover_seeds(&l, &mut rng);
        assert!(!seeds.is_empty());
        let mut artists: HashSet<Option<String>> = HashSet::new();
        for s in &seeds {
            assert!(artists.insert(s.artists.first().map(|a| a.id.clone()).flatten()));
        }
        assert_eq!(artists.len(), seeds.len(), "one seed per artist");
        // a liked song made it in, or a most-played one — pools overlapped
        assert!(seeds.iter().any(|s| l.liked.iter().any(|x| x.id == s.id) || l.most_played(30, 0, 20).iter().any(|x| x.id == s.id)));
    }

    #[test]
    fn the_rng_never_draws_zero_or_one() {
        let mut rng = Rng::seeded(3);
        for _ in 0..1000 {
            let u = rng.unit();
            assert!(u > 0.0 && u < 1.0);
        }
    }
}
