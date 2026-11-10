use crate::download::evaluate_and_rank_candidates;
use crate::identify::{match_filename_against_tags, parse_filename};
use crate::lrc::{audit_lrc, LrcIssue};
use crate::matching::{clean_title, score_candidate, similarity, Candidate};
use crate::model::Track;
use crate::providers::ProviderName;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Category classification of tracks in the golden benchmark dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GoldenCategory {
    CorrectTags,
    WrongTags,
    SwappedArtistTitle,
    NoisyTitle,
    CorrectLrc,
    WrongSongLrc,
    OffsetPlus15,
    OffsetMinus15,
    OffsetPlus30,
    OffsetMinus30,
    PlainOnly,
    Instrumental,
}

impl GoldenCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CorrectTags => "correct_tags",
            Self::WrongTags => "wrong_tags",
            Self::SwappedArtistTitle => "swapped_artist_title",
            Self::NoisyTitle => "noisy_title",
            Self::CorrectLrc => "correct_lrc",
            Self::WrongSongLrc => "wrong_song_lrc",
            Self::OffsetPlus15 => "offset_+1.5s",
            Self::OffsetMinus15 => "offset_-1.5s",
            Self::OffsetPlus30 => "offset_+3.0s",
            Self::OffsetMinus30 => "offset_-3.0s",
            Self::PlainOnly => "plain_only",
            Self::Instrumental => "instrumental",
        }
    }
}

/// A simulated candidate for provider search in the golden set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateFixture {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_seconds: Option<f64>,
    pub synced: Option<String>,
    pub plain: Option<String>,
    pub is_target: bool,
}

impl CandidateFixture {
    pub fn to_candidate(&self) -> Candidate {
        Candidate {
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            duration_seconds: self.duration_seconds,
            synced: self.synced.clone(),
            plain: self.plain.clone(),
            provider: ProviderName::Lrclib,
        }
    }
}

/// Expected ground truth identification outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedIdentification {
    pub should_suggest: bool,
    pub is_swapped: bool,
    pub expected_artist: String,
    pub expected_title: String,
}

/// A track record in the golden benchmark dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenTrack {
    pub id: String,
    pub description: String,
    pub categories: Vec<GoldenCategory>,

    // File and Audio Tag Inputs
    pub file_path: String,
    pub tag_artist: String,
    pub tag_title: String,
    pub tag_album: String,
    pub duration_seconds: Option<f64>,

    // Ground Truth Metadata
    pub true_artist: String,
    pub true_title: String,
    pub true_album: Option<String>,
    pub is_instrumental: bool,

    // Existing Local LRC (simulates user file before sync)
    pub local_lrc: Option<String>,

    // Candidate Search Fixtures
    pub candidates: Vec<CandidateFixture>,

    // Expected Ground Truth Outcomes
    pub expected_identification: Option<ExpectedIdentification>,
    pub expected_wrong_song: bool,
    pub expected_offset_seconds: Option<f64>,
    pub expected_select_target: bool,
}

impl GoldenTrack {
    pub fn to_track(&self) -> Track {
        Track {
            audio_path: self.file_path.clone(),
            lrc_path: self
                .file_path
                .replace(".mp3", ".lrc")
                .replace(".flac", ".lrc"),
            artist: self.tag_artist.clone(),
            title: self.tag_title.clone(),
            album: self.tag_album.clone(),
            duration_seconds: self.duration_seconds,
            status: crate::model::LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        }
    }
}

/// Precision, recall, and accuracy metrics for an evaluation category.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct MetricScore {
    pub total: usize,
    pub true_positives: usize,
    pub false_positives: usize,
    pub true_negatives: usize,
    pub false_negatives: usize,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub accuracy: f64,
}

impl MetricScore {
    pub fn compute(tp: usize, fp: usize, tn: usize, fn_cnt: usize) -> Self {
        let total = tp + fp + tn + fn_cnt;
        let precision = if tp + fp > 0 {
            tp as f64 / (tp + fp) as f64
        } else {
            1.0
        };
        let recall = if tp + fn_cnt > 0 {
            tp as f64 / (tp + fn_cnt) as f64
        } else {
            1.0
        };
        let f1 = if precision + recall > 0.0 {
            2.0 * (precision * recall) / (precision + recall)
        } else {
            0.0
        };
        let accuracy = if total > 0 {
            (tp + tn) as f64 / total as f64
        } else {
            1.0
        };
        Self {
            total,
            true_positives: tp,
            false_positives: fp,
            true_negatives: tn,
            false_negatives: fn_cnt,
            precision,
            recall,
            f1,
            accuracy,
        }
    }
}

/// Minimum baseline thresholds required for CI regression gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldenThresholds {
    pub min_identification_precision: f64,
    pub min_identification_recall: f64,
    pub min_selection_precision: f64,
    pub min_selection_recall: f64,
    pub min_wrong_song_precision: f64,
    pub min_wrong_song_recall: f64,
    pub min_offset_precision: f64,
    pub min_offset_recall: f64,
}

impl Default for GoldenThresholds {
    fn default() -> Self {
        Self {
            min_identification_precision: 1.00,
            min_identification_recall: 0.95,
            min_selection_precision: 1.00,
            min_selection_recall: 1.00,
            min_wrong_song_precision: 1.00,
            min_wrong_song_recall: 1.00,
            min_offset_precision: 1.00,
            min_offset_recall: 1.00,
        }
    }
}

/// Comprehensive benchmark evaluation report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenEvaluationReport {
    pub total_tracks: usize,
    pub identification_metrics: MetricScore,
    pub selection_metrics: MetricScore,
    pub wrong_song_metrics: MetricScore,
    pub offset_metrics: MetricScore,
    pub category_metrics: BTreeMap<String, MetricScore>,
    pub passed_ci_gate: bool,
    pub regression_failures: Vec<String>,
}

// Helper to shift timestamps by delta_seconds
fn shift_lrc(lrc: &str, delta: f64) -> String {
    let mut out = Vec::new();
    for line in lrc.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[')
            && !trimmed.starts_with("[ar:")
            && !trimmed.starts_with("[ti:")
            && !trimmed.starts_with("[al:")
        {
            if let Some(close) = trimmed.find(']') {
                let tag = &trimmed[1..close];
                if let Some((min_str, sec_str)) = tag.split_once(':') {
                    if let (Ok(m), Ok(s)) = (min_str.parse::<f64>(), sec_str.parse::<f64>()) {
                        let original_ts = m * 60.0 + s;
                        let shifted = (original_ts + delta).max(0.0);
                        let s_min = (shifted / 60.0).floor() as u64;
                        let s_sec = shifted - (s_min as f64 * 60.0);
                        let lyric_text = &trimmed[close + 1..];
                        out.push(format!("[{:02}:{:05.2}]{}", s_min, s_sec, lyric_text));
                        continue;
                    }
                }
            }
        }
        out.push(line.to_string());
    }
    out.join("\n")
}

// Real reference lyrics fixtures
const QUEEN_BOHEMIAN_RHAPSODY_LRC: &str = "\
[00:00.80]Is this the real life?
[00:03.20]Is this just fantasy?
[00:06.50]Caught in a landslide, no escape from reality
[00:13.00]Open your eyes, look up to the skies and see
[00:20.50]I'm just a poor boy, I need no sympathy
[00:25.80]Because I'm easy come, easy go, little high, little low
[00:32.40]Any way the wind blows doesn't really matter to me, to me
[00:46.00]Mama, just killed a man
[00:51.50]Put a gun against his head, pulled my trigger, now he's dead
[00:59.00]Mama, life had just begun
[01:04.50]But now I've gone and thrown it all away";

const EAGLES_HOTEL_CALIFORNIA_LRC: &str = "\
[00:53.00]On a dark desert highway, cool wind in my hair
[00:59.50]Warm smell of colitas, rising up through the air
[01:06.00]Up ahead in the distance, I saw a shimmering light
[01:12.50]My head grew heavy and my sight grew dim, I had to stop for the night
[01:19.00]There she stood in the doorway, I heard the mission bell
[01:25.50]And I was thinking to myself this could be Heaven or this could be Hell
[01:32.00]Then she lit up a candle and she showed me the way
[01:38.50]There were voices down the corridor, I thought I heard them say
[01:45.00]Welcome to the Hotel California, such a lovely place";

const ED_SHEERAN_SHAPE_OF_YOU_LRC: &str = "\
[00:00.50]The club isn't the best place to find a lover
[00:02.50]So the bar is where I go
[00:05.00]Me and my friends at the table doing shots
[00:07.50]Drinking fast and then we talk slow
[00:10.00]Come over and start up a conversation with just me
[00:13.00]And trust me, I'll give it a chance now
[00:15.50]Take my hand, stop, put Van the Man on the jukebox
[00:18.50]And then we start to dance, and now I'm singing like
[00:21.00]Girl, you know I want your love, your love was handmade for somebody like me";

const NIRVANA_SMELLS_LIKE_TEEN_SPIRIT_LRC: &str = "\
[00:25.00]Load up on guns, bring your friends
[00:28.00]It's fun to lose and to pretend
[00:31.50]She's over-bored and self-assured
[00:35.00]Oh no, I know a dirty word
[00:42.50]Hello, hello, hello, how low
[00:46.00]Hello, hello, hello, how low
[00:49.50]With the lights out, it's less dangerous
[00:53.00]Here we are now, entertain us
[00:56.50]I feel stupid and contagious";

const FLEETWOOD_MAC_DREAMS_LRC: &str = "\
[00:15.00]Now here you go again, you say you want your freedom
[00:22.50]Well, who am I to keep you down?
[00:30.00]It's only right that you should play the way you feel it
[00:38.00]But listen carefully to the sound of your loneliness
[00:47.00]Like a heartbeat drives you mad in the stillness of remembering
[00:54.50]What you had and what you lost
[01:02.00]And what you had and what you lost
[01:10.00]Thunder only happens when it's raining";

const BEATLES_COME_TOGETHER_LRC: &str = "\
[00:08.00]Here come old flat top, he come grooving up slowly
[00:13.50]He got joo-joo eyeball, he one holy roller
[00:18.50]He got hair down to his knee
[00:22.00]Got to be a joker, he just do what he please
[00:31.50]He wear no shoeshine, he got toe-jam football
[00:36.50]He got monkey finger, he shoot Coca-Cola
[00:41.50]He say I know you, you know me
[00:45.00]One thing I can tell you is you got to be free
[00:49.00]Come together, right now, over me";

const COLD_HART_PASSION_LRC: &str = "\
[00:10.00]I got passion in my heart and it's tearing me apart
[00:15.50]In the dark, see the sparks when we start
[00:20.50]Yeah, you called my phone, left you alone
[00:25.50]Now I'm standing in the rain on my own
[00:30.50]Every night the same thing, memories fade away
[00:35.50]I don't know what to say, baby please don't walk away";

const LINKIN_PARK_IN_THE_END_LRC: &str = "\
[00:12.00]It starts with one thing, I don't know why
[00:15.00]It doesn't even matter how hard you try
[00:17.50]Keep that in mind, I designed this rhyme to explain in due time
[00:21.50]All I know, time is a valuable thing
[00:24.00]Watch it fly by as the pendulum swings
[00:26.50]Watch it count down to the end of the day, the clock ticks life away
[00:31.00]It's so unreal, didn't look out below
[00:33.50]Watch the time go right out the window";

const PINK_FLOYD_COMFORTABLY_NUMB_LRC: &str = "\
[00:20.00]Hello, is there anybody in there?
[00:27.50]Just nod if you can hear me, is there anyone home?
[00:35.00]Come on now, I hear you're feeling down
[00:42.50]Well, I can ease your pain, get you on your feet again
[00:51.00]Relax, I'll need some information first
[00:58.50]Just the basic facts, can you show me where it hurts?
[01:06.00]There is no pain, you are receding";

const COLD_HART_2AM_LRC: &str = "\
[00:08.00]It's 2 am and I'm still up thinking about you
[00:13.50]Nothing feels right in this empty room without you
[00:19.00]Smoking by the window, headlights in the night
[00:24.50]Wish that we could talk, wish that we could make it right
[00:30.00]2 am, 2 am, going round again";

const OASIS_WONDERWALL_LRC: &str = "\
[00:15.00]Today is gonna be the day that they're gonna throw it back to you
[00:21.00]By now you should've somehow realized what you gotta do
[00:27.50]I don't believe that anybody feels the way I do about you now
[00:36.50]Backbeat, the word is on the street that the fire in your heart is out
[00:43.00]I'm sure you've heard it all before, but you never really had a doubt
[00:49.00]Because maybe, you're gonna be the one that saves me
[00:58.00]And after all, you're my wonderwall";

const DAFT_PUNK_GET_LUCKY_LRC: &str = "\
[00:18.00]Like the legend of the phoenix, all ends with beginnings
[00:23.00]What keeps the planet spinning, the force from the beginning
[00:28.00]We've come too far to give up who we are
[00:33.00]So let's raise the bar and our cups to the stars
[00:37.50]She's up all night 'til the sun, I'm up all night to get some
[00:42.50]She's up all night for good fun, I'm up all night to get lucky";

const MICHAEL_JACKSON_BILLIE_JEAN_LRC: &str = "\
[00:29.00]She was more like a beauty queen from a movie scene
[00:33.50]I said don't mind, but what do you mean, I am the one
[00:37.50]Who will dance on the floor in the round?
[00:43.00]She said I am the one who will dance on the floor in the round
[00:48.50]She told me her name was Billie Jean as she caused a scene
[00:53.00]Then every head turned with eyes that dreamed of being the one
[00:58.00]Billie Jean is not my lover, she's just a girl who claims that I am the one";

const COLDPLAY_VIVA_LA_VIDA_LRC: &str = "\
[00:10.00]I used to rule the world, seas would rise when I gave the word
[00:16.50]Now in the morning I sleep alone, sweep the streets I used to own
[00:23.00]I used to roll the dice, feel the fear in my enemy's eyes
[00:29.50]Listen as the crowd would sing, now the old king is dead, long live the king
[00:36.00]One minute I held the key, next the walls were closed on me
[00:42.50]And I discovered that my castles stand upon pillars of salt and pillars of sand";

const ADELE_HELLO_PLAIN: &str = "\
Hello, it's me
I was wondering if after all these years you'd like to meet
To go over everything
They say that time's supposed to heal ya, but I ain't done much healing
Hello, can you hear me?
I'm in California dreaming about who we used to be
When we were younger and free
I've forgotten how it felt before the world fell at our feet
There's such a difference between us and a million miles
Hello from the other side";

const BOB_DYLAN_BLOWIN_PLAIN: &str = "\
How many roads must a man walk down
Before you call him a man?
How many seas must a white dove sail
Before she sleeps in the sand?
Yes, and how many times must the cannonballs fly
Before they're forever banned?
The answer, my friend, is blowin' in the wind
The answer is blowin' in the wind";

const LEONARD_COHEN_HALLELUJAH_PLAIN: &str = "\
Now I've heard there was a secret chord
That David played, and it pleased the Lord
But you don't really care for music, do you?
It goes like this, the fourth, the fifth
The minor fall, the major lift
The baffled king composing Hallelujah
Hallelujah, Hallelujah, Hallelujah, Hallelujah";

/// Build the standard 33-track golden benchmark set.
pub fn build_golden_set() -> Vec<GoldenTrack> {
    vec![
        // ---------------------------------------------------------------------
        // 1-3: Clean / Correct Tags + Correct LRC
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T01_bohemian_rhapsody_clean".into(),
            description: "Queen - Bohemian Rhapsody with clean tags and accurate LRC".into(),
            categories: vec![GoldenCategory::CorrectTags, GoldenCategory::CorrectLrc],
            file_path: "Queen - Bohemian Rhapsody.mp3".into(),
            tag_artist: "Queen".into(),
            tag_title: "Bohemian Rhapsody".into(),
            tag_album: "A Night at the Opera".into(),
            duration_seconds: Some(354.0),
            true_artist: "Queen".into(),
            true_title: "Bohemian Rhapsody".into(),
            true_album: Some("A Night at the Opera".into()),
            is_instrumental: false,
            local_lrc: Some(QUEEN_BOHEMIAN_RHAPSODY_LRC.into()),
            candidates: vec![
                CandidateFixture {
                    title: "Bohemian Rhapsody".into(),
                    artist: "Queen".into(),
                    album: Some("A Night at the Opera".into()),
                    duration_seconds: Some(354.0),
                    synced: Some(QUEEN_BOHEMIAN_RHAPSODY_LRC.into()),
                    plain: None,
                    is_target: true,
                },
                CandidateFixture {
                    title: "Killer Queen".into(),
                    artist: "Queen".into(),
                    album: Some("Sheer Heart Attack".into()),
                    duration_seconds: Some(181.0),
                    synced: Some("[00:05.00]She keeps Moet and Chandon".into()),
                    plain: None,
                    is_target: false,
                },
            ],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T02_hotel_california_clean".into(),
            description: "Eagles - Hotel California with clean tags and accurate LRC".into(),
            categories: vec![GoldenCategory::CorrectTags, GoldenCategory::CorrectLrc],
            file_path: "Eagles - Hotel California.flac".into(),
            tag_artist: "Eagles".into(),
            tag_title: "Hotel California".into(),
            tag_album: "Hotel California".into(),
            duration_seconds: Some(391.0),
            true_artist: "Eagles".into(),
            true_title: "Hotel California".into(),
            true_album: Some("Hotel California".into()),
            is_instrumental: false,
            local_lrc: Some(EAGLES_HOTEL_CALIFORNIA_LRC.into()),
            candidates: vec![
                CandidateFixture {
                    title: "Hotel California".into(),
                    artist: "Eagles".into(),
                    album: Some("Hotel California".into()),
                    duration_seconds: Some(391.0),
                    synced: Some(EAGLES_HOTEL_CALIFORNIA_LRC.into()),
                    plain: None,
                    is_target: true,
                },
                CandidateFixture {
                    title: "Take It Easy".into(),
                    artist: "Eagles".into(),
                    album: Some("Eagles".into()),
                    duration_seconds: Some(211.0),
                    synced: Some("[00:10.00]Well I'm running down the road".into()),
                    plain: None,
                    is_target: false,
                },
            ],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T03_shape_of_you_clean".into(),
            description: "Ed Sheeran - Shape of You with clean tags and accurate LRC".into(),
            categories: vec![GoldenCategory::CorrectTags, GoldenCategory::CorrectLrc],
            file_path: "Ed Sheeran - Shape of You.mp3".into(),
            tag_artist: "Ed Sheeran".into(),
            tag_title: "Shape of You".into(),
            tag_album: "÷ (Divide)".into(),
            duration_seconds: Some(233.0),
            true_artist: "Ed Sheeran".into(),
            true_title: "Shape of You".into(),
            true_album: Some("÷ (Divide)".into()),
            is_instrumental: false,
            local_lrc: Some(ED_SHEERAN_SHAPE_OF_YOU_LRC.into()),
            candidates: vec![
                CandidateFixture {
                    title: "Shape of You".into(),
                    artist: "Ed Sheeran".into(),
                    album: Some("÷ (Divide)".into()),
                    duration_seconds: Some(233.0),
                    synced: Some(ED_SHEERAN_SHAPE_OF_YOU_LRC.into()),
                    plain: None,
                    is_target: true,
                },
                CandidateFixture {
                    title: "Perfect".into(),
                    artist: "Ed Sheeran".into(),
                    album: Some("÷ (Divide)".into()),
                    duration_seconds: Some(263.0),
                    synced: Some("[00:05.00]I found a love for me".into()),
                    plain: None,
                    is_target: false,
                },
            ],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        // ---------------------------------------------------------------------
        // 4-6: Wrong Tags (Missing Artist, Filename-like Title, Track Number)
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T04_smells_like_teen_spirit_missing_artist".into(),
            description: "Nirvana track with Unknown Artist in tags, clean filename".into(),
            categories: vec![GoldenCategory::WrongTags, GoldenCategory::CorrectLrc],
            file_path: "Nirvana - Smells Like Teen Spirit.mp3".into(),
            tag_artist: "Unknown Artist".into(),
            tag_title: "Smells Like Teen Spirit".into(),
            tag_album: "".into(),
            duration_seconds: Some(301.0),
            true_artist: "Nirvana".into(),
            true_title: "Smells Like Teen Spirit".into(),
            true_album: Some("Nevermind".into()),
            is_instrumental: false,
            local_lrc: Some(NIRVANA_SMELLS_LIKE_TEEN_SPIRIT_LRC.into()),
            candidates: vec![CandidateFixture {
                title: "Smells Like Teen Spirit".into(),
                artist: "Nirvana".into(),
                album: Some("Nevermind".into()),
                duration_seconds: Some(301.0),
                synced: Some(NIRVANA_SMELLS_LIKE_TEEN_SPIRIT_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: Some(ExpectedIdentification {
                should_suggest: true,
                is_swapped: false,
                expected_artist: "Nirvana".into(),
                expected_title: "Smells Like Teen Spirit".into(),
            }),
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T05_dreams_filename_title".into(),
            description: "Fleetwood Mac with filename-like title tag".into(),
            categories: vec![GoldenCategory::WrongTags, GoldenCategory::CorrectLrc],
            file_path: "Fleetwood Mac - Dreams.mp3".into(),
            tag_artist: "Fleetwood Mac".into(),
            tag_title: "Fleetwood_Mac_-_Dreams.mp3".into(),
            tag_album: "".into(),
            duration_seconds: Some(257.0),
            true_artist: "Fleetwood Mac".into(),
            true_title: "Dreams".into(),
            true_album: Some("Rumours".into()),
            is_instrumental: false,
            local_lrc: Some(FLEETWOOD_MAC_DREAMS_LRC.into()),
            candidates: vec![CandidateFixture {
                title: "Dreams".into(),
                artist: "Fleetwood Mac".into(),
                album: Some("Rumours".into()),
                duration_seconds: Some(257.0),
                synced: Some(FLEETWOOD_MAC_DREAMS_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: Some(ExpectedIdentification {
                should_suggest: true,
                is_swapped: false,
                expected_artist: "Fleetwood Mac".into(),
                expected_title: "Dreams".into(),
            }),
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T06_come_together_track_num_tag".into(),
            description: "Beatles track with track number prefix '01. Come Together' in title"
                .into(),
            categories: vec![GoldenCategory::WrongTags, GoldenCategory::CorrectLrc],
            file_path: "01 - The Beatles - Come Together.mp3".into(),
            tag_artist: "The Beatles".into(),
            tag_title: "01. Come Together".into(),
            tag_album: "Abbey Road".into(),
            duration_seconds: Some(259.0),
            true_artist: "The Beatles".into(),
            true_title: "Come Together".into(),
            true_album: Some("Abbey Road".into()),
            is_instrumental: false,
            local_lrc: Some(BEATLES_COME_TOGETHER_LRC.into()),
            candidates: vec![CandidateFixture {
                title: "Come Together".into(),
                artist: "The Beatles".into(),
                album: Some("Abbey Road".into()),
                duration_seconds: Some(259.0),
                synced: Some(BEATLES_COME_TOGETHER_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: Some(ExpectedIdentification {
                should_suggest: true,
                is_swapped: false,
                expected_artist: "The Beatles".into(),
                expected_title: "Come Together".into(),
            }),
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        // ---------------------------------------------------------------------
        // 7-9: Swapped Artist and Title
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T07_bohemian_rhapsody_swapped".into(),
            description: "Queen - Bohemian Rhapsody with Artist and Title reversed in tags".into(),
            categories: vec![
                GoldenCategory::SwappedArtistTitle,
                GoldenCategory::CorrectLrc,
            ],
            file_path: "Queen - Bohemian Rhapsody.mp3".into(),
            tag_artist: "Bohemian Rhapsody".into(),
            tag_title: "Queen".into(),
            tag_album: "A Night at the Opera".into(),
            duration_seconds: Some(354.0),
            true_artist: "Queen".into(),
            true_title: "Bohemian Rhapsody".into(),
            true_album: Some("A Night at the Opera".into()),
            is_instrumental: false,
            local_lrc: Some(QUEEN_BOHEMIAN_RHAPSODY_LRC.into()),
            candidates: vec![
                CandidateFixture {
                    title: "Bohemian Rhapsody".into(),
                    artist: "Queen".into(),
                    album: Some("A Night at the Opera".into()),
                    duration_seconds: Some(354.0),
                    synced: Some(QUEEN_BOHEMIAN_RHAPSODY_LRC.into()),
                    plain: None,
                    is_target: true,
                },
                CandidateFixture {
                    title: "Queen".into(),
                    artist: "Bohemian Rhapsody".into(),
                    album: None,
                    duration_seconds: Some(120.0),
                    synced: None,
                    plain: Some("garbage text".into()),
                    is_target: false,
                },
            ],
            expected_identification: Some(ExpectedIdentification {
                should_suggest: true,
                is_swapped: true,
                expected_artist: "Queen".into(),
                expected_title: "Bohemian Rhapsody".into(),
            }),
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T08_hotel_california_swapped".into(),
            description: "Eagles - Hotel California with Artist and Title reversed in tags".into(),
            categories: vec![
                GoldenCategory::SwappedArtistTitle,
                GoldenCategory::CorrectLrc,
            ],
            file_path: "Eagles - Hotel California.mp3".into(),
            tag_artist: "Hotel California".into(),
            tag_title: "Eagles".into(),
            tag_album: "Hotel California".into(),
            duration_seconds: Some(391.0),
            true_artist: "Eagles".into(),
            true_title: "Hotel California".into(),
            true_album: Some("Hotel California".into()),
            is_instrumental: false,
            local_lrc: Some(EAGLES_HOTEL_CALIFORNIA_LRC.into()),
            candidates: vec![CandidateFixture {
                title: "Hotel California".into(),
                artist: "Eagles".into(),
                album: Some("Hotel California".into()),
                duration_seconds: Some(391.0),
                synced: Some(EAGLES_HOTEL_CALIFORNIA_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: Some(ExpectedIdentification {
                should_suggest: true,
                is_swapped: true,
                expected_artist: "Eagles".into(),
                expected_title: "Hotel California".into(),
            }),
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T09_cold_hart_passion_swapped".into(),
            description: "Cold Hart - Passion with Artist and Title swapped in tags".into(),
            categories: vec![
                GoldenCategory::SwappedArtistTitle,
                GoldenCategory::CorrectLrc,
            ],
            file_path: "Cold Hart - Passion.mp3".into(),
            tag_artist: "Passion".into(),
            tag_title: "Cold Hart".into(),
            tag_album: "Downer".into(),
            duration_seconds: Some(145.0),
            true_artist: "Cold Hart".into(),
            true_title: "Passion".into(),
            true_album: Some("Downer".into()),
            is_instrumental: false,
            local_lrc: Some(COLD_HART_PASSION_LRC.into()),
            candidates: vec![CandidateFixture {
                title: "Passion".into(),
                artist: "Cold Hart".into(),
                album: Some("Downer".into()),
                duration_seconds: Some(145.0),
                synced: Some(COLD_HART_PASSION_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: Some(ExpectedIdentification {
                should_suggest: true,
                is_swapped: true,
                expected_artist: "Cold Hart".into(),
                expected_title: "Passion".into(),
            }),
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        // ---------------------------------------------------------------------
        // 10-13: Noisy Titles (Features, Video labels, Remasters, Production)
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T10_in_the_end_noisy_features".into(),
            description:
                "Linkin Park - In the End with '(feat. Chester Bennington) [Official Music Video]'"
                    .into(),
            categories: vec![GoldenCategory::NoisyTitle, GoldenCategory::CorrectLrc],
            file_path:
                "Linkin Park - In the End (feat. Chester Bennington) [Official Music Video].mp3"
                    .into(),
            tag_artist: "Linkin Park".into(),
            tag_title: "In the End (feat. Chester Bennington) [Official Music Video]".into(),
            tag_album: "Hybrid Theory".into(),
            duration_seconds: Some(216.0),
            true_artist: "Linkin Park".into(),
            true_title: "In the End".into(),
            true_album: Some("Hybrid Theory".into()),
            is_instrumental: false,
            local_lrc: Some(LINKIN_PARK_IN_THE_END_LRC.into()),
            candidates: vec![
                CandidateFixture {
                    title: "In the End".into(),
                    artist: "Linkin Park".into(),
                    album: Some("Hybrid Theory".into()),
                    duration_seconds: Some(216.0),
                    synced: Some(LINKIN_PARK_IN_THE_END_LRC.into()),
                    plain: None,
                    is_target: true,
                },
                CandidateFixture {
                    title: "Numb".into(),
                    artist: "Linkin Park".into(),
                    album: Some("Meteora".into()),
                    duration_seconds: Some(187.0),
                    synced: Some("[00:05.00]I'm tired of being what you want me to be".into()),
                    plain: None,
                    is_target: false,
                },
            ],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T11_comfortably_numb_noisy_remaster".into(),
            description: "Pink Floyd - Comfortably Numb with '- 2011 Remastered' suffix".into(),
            categories: vec![GoldenCategory::NoisyTitle, GoldenCategory::CorrectLrc],
            file_path: "Pink Floyd - Comfortably Numb - 2011 Remastered.flac".into(),
            tag_artist: "Pink Floyd".into(),
            tag_title: "Comfortably Numb - 2011 Remastered".into(),
            tag_album: "The Wall".into(),
            duration_seconds: Some(382.0),
            true_artist: "Pink Floyd".into(),
            true_title: "Comfortably Numb".into(),
            true_album: Some("The Wall".into()),
            is_instrumental: false,
            local_lrc: Some(PINK_FLOYD_COMFORTABLY_NUMB_LRC.into()),
            candidates: vec![CandidateFixture {
                title: "Comfortably Numb".into(),
                artist: "Pink Floyd".into(),
                album: Some("The Wall".into()),
                duration_seconds: Some(382.0),
                synced: Some(PINK_FLOYD_COMFORTABLY_NUMB_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T12_cold_hart_2am_noisy_explicit".into(),
            description: "Cold Hart - 2 Am with '01. ' prefix and '(Explicit)' tag".into(),
            categories: vec![GoldenCategory::NoisyTitle, GoldenCategory::CorrectLrc],
            file_path: "01. Cold Hart - 2 Am (Explicit).mp3".into(),
            tag_artist: "Cold Hart".into(),
            tag_title: "01. 2 Am (Explicit)".into(),
            tag_album: "Good Morning".into(),
            duration_seconds: Some(132.0),
            true_artist: "Cold Hart".into(),
            true_title: "2 Am".into(),
            true_album: Some("Good Morning".into()),
            is_instrumental: false,
            local_lrc: Some(COLD_HART_2AM_LRC.into()),
            candidates: vec![
                CandidateFixture {
                    title: "2 Am".into(),
                    artist: "Cold Hart".into(),
                    album: Some("Good Morning".into()),
                    duration_seconds: Some(132.0),
                    synced: Some(COLD_HART_2AM_LRC.into()),
                    plain: None,
                    is_target: true,
                },
                CandidateFixture {
                    title: "3 Am".into(),
                    artist: "Cold Hart".into(),
                    album: Some("Good Morning".into()),
                    duration_seconds: Some(140.0),
                    synced: Some("[00:05.00]Different track".into()),
                    plain: None,
                    is_target: false,
                },
            ],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T13_wonderwall_noisy_live_radio".into(),
            description: "Oasis - Wonderwall with '(Live Session / Radio Edit)' noise".into(),
            categories: vec![GoldenCategory::NoisyTitle, GoldenCategory::CorrectLrc],
            file_path: "Oasis - Wonderwall (Live Session / Radio Edit).mp3".into(),
            tag_artist: "Oasis".into(),
            tag_title: "Wonderwall (Live Session / Radio Edit)".into(),
            tag_album: "(What's the Story) Morning Glory?".into(),
            duration_seconds: Some(258.0),
            true_artist: "Oasis".into(),
            true_title: "Wonderwall".into(),
            true_album: Some("(What's the Story) Morning Glory?".into()),
            is_instrumental: false,
            local_lrc: Some(OASIS_WONDERWALL_LRC.into()),
            candidates: vec![CandidateFixture {
                title: "Wonderwall".into(),
                artist: "Oasis".into(),
                album: Some("(What's the Story) Morning Glory?".into()),
                duration_seconds: Some(258.0),
                synced: Some(OASIS_WONDERWALL_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        // ---------------------------------------------------------------------
        // 14-16: Wrong-Song LRC (Lyrics of completely different song assigned)
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T14_bohemian_rhapsody_wrong_song_lrc".into(),
            description: "Queen track with Hotel California lyrics in local LRC".into(),
            categories: vec![GoldenCategory::WrongSongLrc, GoldenCategory::CorrectTags],
            file_path: "Queen - Bohemian Rhapsody.mp3".into(),
            tag_artist: "Queen".into(),
            tag_title: "Bohemian Rhapsody".into(),
            tag_album: "A Night at the Opera".into(),
            duration_seconds: Some(354.0),
            true_artist: "Queen".into(),
            true_title: "Bohemian Rhapsody".into(),
            true_album: Some("A Night at the Opera".into()),
            is_instrumental: false,
            local_lrc: Some(EAGLES_HOTEL_CALIFORNIA_LRC.into()),
            candidates: vec![CandidateFixture {
                title: "Bohemian Rhapsody".into(),
                artist: "Queen".into(),
                album: Some("A Night at the Opera".into()),
                duration_seconds: Some(354.0),
                synced: Some(QUEEN_BOHEMIAN_RHAPSODY_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: true,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T15_shape_of_you_wrong_song_lrc".into(),
            description: "Ed Sheeran track with Bohemian Rhapsody lyrics in local LRC".into(),
            categories: vec![GoldenCategory::WrongSongLrc, GoldenCategory::CorrectTags],
            file_path: "Ed Sheeran - Shape of You.mp3".into(),
            tag_artist: "Ed Sheeran".into(),
            tag_title: "Shape of You".into(),
            tag_album: "÷ (Divide)".into(),
            duration_seconds: Some(233.0),
            true_artist: "Ed Sheeran".into(),
            true_title: "Shape of You".into(),
            true_album: Some("÷ (Divide)".into()),
            is_instrumental: false,
            local_lrc: Some(QUEEN_BOHEMIAN_RHAPSODY_LRC.into()),
            candidates: vec![CandidateFixture {
                title: "Shape of You".into(),
                artist: "Ed Sheeran".into(),
                album: Some("÷ (Divide)".into()),
                duration_seconds: Some(233.0),
                synced: Some(ED_SHEERAN_SHAPE_OF_YOU_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: true,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T16_hotel_california_wrong_song_mismatch_tag".into(),
            description: "Eagles track with [ti: Despacito] metadata mismatch tag and lyrics"
                .into(),
            categories: vec![GoldenCategory::WrongSongLrc, GoldenCategory::CorrectTags],
            file_path: "Eagles - Hotel California.mp3".into(),
            tag_artist: "Eagles".into(),
            tag_title: "Hotel California".into(),
            tag_album: "Hotel California".into(),
            duration_seconds: Some(391.0),
            true_artist: "Eagles".into(),
            true_title: "Hotel California".into(),
            true_album: Some("Hotel California".into()),
            is_instrumental: false,
            local_lrc: Some(
                "\
[ti: Despacito]
[ar: Luis Fonsi]
[00:01.00]Ay, Fonsi, DY
[00:03.00]Oh, oh no, oh no
[00:05.50]Hey, yeah, dididiri Daddy, go!
[00:09.00]Sí, sabes que ya llevo un rato mirándote
[00:14.50]Tengo que bailar contigo hoy"
                    .into(),
            ),
            candidates: vec![CandidateFixture {
                title: "Hotel California".into(),
                artist: "Eagles".into(),
                album: Some("Hotel California".into()),
                duration_seconds: Some(391.0),
                synced: Some(EAGLES_HOTEL_CALIFORNIA_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: true,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        // ---------------------------------------------------------------------
        // 17-18: LRC Offset by +1.5s
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T17_bohemian_rhapsody_offset_plus_1_5".into(),
            description: "Queen - Bohemian Rhapsody with timestamps shifted +1.5s".into(),
            categories: vec![GoldenCategory::OffsetPlus15, GoldenCategory::CorrectTags],
            file_path: "Queen - Bohemian Rhapsody.mp3".into(),
            tag_artist: "Queen".into(),
            tag_title: "Bohemian Rhapsody".into(),
            tag_album: "A Night at the Opera".into(),
            duration_seconds: Some(354.0),
            true_artist: "Queen".into(),
            true_title: "Bohemian Rhapsody".into(),
            true_album: Some("A Night at the Opera".into()),
            is_instrumental: false,
            local_lrc: Some(shift_lrc(QUEEN_BOHEMIAN_RHAPSODY_LRC, 1.5)),
            candidates: vec![CandidateFixture {
                title: "Bohemian Rhapsody".into(),
                artist: "Queen".into(),
                album: Some("A Night at the Opera".into()),
                duration_seconds: Some(354.0),
                synced: Some(QUEEN_BOHEMIAN_RHAPSODY_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: Some(1.5),
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T18_hotel_california_offset_plus_1_5".into(),
            description: "Eagles - Hotel California with timestamps shifted +1.5s".into(),
            categories: vec![GoldenCategory::OffsetPlus15, GoldenCategory::CorrectTags],
            file_path: "Eagles - Hotel California.mp3".into(),
            tag_artist: "Eagles".into(),
            tag_title: "Hotel California".into(),
            tag_album: "Hotel California".into(),
            duration_seconds: Some(391.0),
            true_artist: "Eagles".into(),
            true_title: "Hotel California".into(),
            true_album: Some("Hotel California".into()),
            is_instrumental: false,
            local_lrc: Some(shift_lrc(EAGLES_HOTEL_CALIFORNIA_LRC, 1.5)),
            candidates: vec![CandidateFixture {
                title: "Hotel California".into(),
                artist: "Eagles".into(),
                album: Some("Hotel California".into()),
                duration_seconds: Some(391.0),
                synced: Some(EAGLES_HOTEL_CALIFORNIA_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: Some(1.5),
            expected_select_target: true,
        },
        // ---------------------------------------------------------------------
        // 19-20: LRC Offset by -1.5s
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T19_shape_of_you_offset_minus_1_5".into(),
            description: "Ed Sheeran - Shape of You with timestamps shifted -1.5s".into(),
            categories: vec![GoldenCategory::OffsetMinus15, GoldenCategory::CorrectTags],
            file_path: "Ed Sheeran - Shape of You.mp3".into(),
            tag_artist: "Ed Sheeran".into(),
            tag_title: "Shape of You".into(),
            tag_album: "÷ (Divide)".into(),
            duration_seconds: Some(233.0),
            true_artist: "Ed Sheeran".into(),
            true_title: "Shape of You".into(),
            true_album: Some("÷ (Divide)".into()),
            is_instrumental: false,
            local_lrc: Some(shift_lrc(ED_SHEERAN_SHAPE_OF_YOU_LRC, -1.5)),
            candidates: vec![CandidateFixture {
                title: "Shape of You".into(),
                artist: "Ed Sheeran".into(),
                album: Some("÷ (Divide)".into()),
                duration_seconds: Some(233.0),
                synced: Some(ED_SHEERAN_SHAPE_OF_YOU_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: Some(-1.5),
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T20_smells_like_teen_spirit_offset_minus_1_5".into(),
            description: "Nirvana - Smells Like Teen Spirit with timestamps shifted -1.5s".into(),
            categories: vec![GoldenCategory::OffsetMinus15, GoldenCategory::CorrectTags],
            file_path: "Nirvana - Smells Like Teen Spirit.mp3".into(),
            tag_artist: "Nirvana".into(),
            tag_title: "Smells Like Teen Spirit".into(),
            tag_album: "Nevermind".into(),
            duration_seconds: Some(301.0),
            true_artist: "Nirvana".into(),
            true_title: "Smells Like Teen Spirit".into(),
            true_album: Some("Nevermind".into()),
            is_instrumental: false,
            local_lrc: Some(shift_lrc(NIRVANA_SMELLS_LIKE_TEEN_SPIRIT_LRC, -1.5)),
            candidates: vec![CandidateFixture {
                title: "Smells Like Teen Spirit".into(),
                artist: "Nirvana".into(),
                album: Some("Nevermind".into()),
                duration_seconds: Some(301.0),
                synced: Some(NIRVANA_SMELLS_LIKE_TEEN_SPIRIT_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: Some(-1.5),
            expected_select_target: true,
        },
        // ---------------------------------------------------------------------
        // 21-22: LRC Offset by +3.0s
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T21_bohemian_rhapsody_offset_plus_3_0".into(),
            description: "Queen - Bohemian Rhapsody with timestamps shifted +3.0s".into(),
            categories: vec![GoldenCategory::OffsetPlus30, GoldenCategory::CorrectTags],
            file_path: "Queen - Bohemian Rhapsody.mp3".into(),
            tag_artist: "Queen".into(),
            tag_title: "Bohemian Rhapsody".into(),
            tag_album: "A Night at the Opera".into(),
            duration_seconds: Some(354.0),
            true_artist: "Queen".into(),
            true_title: "Bohemian Rhapsody".into(),
            true_album: Some("A Night at the Opera".into()),
            is_instrumental: false,
            local_lrc: Some(shift_lrc(QUEEN_BOHEMIAN_RHAPSODY_LRC, 3.0)),
            candidates: vec![CandidateFixture {
                title: "Bohemian Rhapsody".into(),
                artist: "Queen".into(),
                album: Some("A Night at the Opera".into()),
                duration_seconds: Some(354.0),
                synced: Some(QUEEN_BOHEMIAN_RHAPSODY_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: Some(3.0),
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T22_dreams_offset_plus_3_0".into(),
            description: "Fleetwood Mac - Dreams with timestamps shifted +3.0s".into(),
            categories: vec![GoldenCategory::OffsetPlus30, GoldenCategory::CorrectTags],
            file_path: "Fleetwood Mac - Dreams.mp3".into(),
            tag_artist: "Fleetwood Mac".into(),
            tag_title: "Dreams".into(),
            tag_album: "Rumours".into(),
            duration_seconds: Some(257.0),
            true_artist: "Fleetwood Mac".into(),
            true_title: "Dreams".into(),
            true_album: Some("Rumours".into()),
            is_instrumental: false,
            local_lrc: Some(shift_lrc(FLEETWOOD_MAC_DREAMS_LRC, 3.0)),
            candidates: vec![CandidateFixture {
                title: "Dreams".into(),
                artist: "Fleetwood Mac".into(),
                album: Some("Rumours".into()),
                duration_seconds: Some(257.0),
                synced: Some(FLEETWOOD_MAC_DREAMS_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: Some(3.0),
            expected_select_target: true,
        },
        // ---------------------------------------------------------------------
        // 23-24: LRC Offset by -3.0s
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T23_hotel_california_offset_minus_3_0".into(),
            description: "Eagles - Hotel California with timestamps shifted -3.0s".into(),
            categories: vec![GoldenCategory::OffsetMinus30, GoldenCategory::CorrectTags],
            file_path: "Eagles - Hotel California.mp3".into(),
            tag_artist: "Eagles".into(),
            tag_title: "Hotel California".into(),
            tag_album: "Hotel California".into(),
            duration_seconds: Some(391.0),
            true_artist: "Eagles".into(),
            true_title: "Hotel California".into(),
            true_album: Some("Hotel California".into()),
            is_instrumental: false,
            local_lrc: Some(shift_lrc(EAGLES_HOTEL_CALIFORNIA_LRC, -3.0)),
            candidates: vec![CandidateFixture {
                title: "Hotel California".into(),
                artist: "Eagles".into(),
                album: Some("Hotel California".into()),
                duration_seconds: Some(391.0),
                synced: Some(EAGLES_HOTEL_CALIFORNIA_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: Some(-3.0),
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T24_wonderwall_offset_minus_3_0".into(),
            description: "Oasis - Wonderwall with timestamps shifted -3.0s".into(),
            categories: vec![GoldenCategory::OffsetMinus30, GoldenCategory::CorrectTags],
            file_path: "Oasis - Wonderwall.mp3".into(),
            tag_artist: "Oasis".into(),
            tag_title: "Wonderwall".into(),
            tag_album: "(What's the Story) Morning Glory?".into(),
            duration_seconds: Some(258.0),
            true_artist: "Oasis".into(),
            true_title: "Wonderwall".into(),
            true_album: Some("(What's the Story) Morning Glory?".into()),
            is_instrumental: false,
            local_lrc: Some(shift_lrc(OASIS_WONDERWALL_LRC, -3.0)),
            candidates: vec![CandidateFixture {
                title: "Wonderwall".into(),
                artist: "Oasis".into(),
                album: Some("(What's the Story) Morning Glory?".into()),
                duration_seconds: Some(258.0),
                synced: Some(OASIS_WONDERWALL_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: Some(-3.0),
            expected_select_target: true,
        },
        // ---------------------------------------------------------------------
        // 25-27: Plain-Only (Unsynchronized Lyrics)
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T25_hello_plain_only".into(),
            description: "Adele - Hello with plain-only lyrics available".into(),
            categories: vec![GoldenCategory::PlainOnly, GoldenCategory::CorrectTags],
            file_path: "Adele - Hello.mp3".into(),
            tag_artist: "Adele".into(),
            tag_title: "Hello".into(),
            tag_album: "25".into(),
            duration_seconds: Some(295.0),
            true_artist: "Adele".into(),
            true_title: "Hello".into(),
            true_album: Some("25".into()),
            is_instrumental: false,
            local_lrc: Some(ADELE_HELLO_PLAIN.into()),
            candidates: vec![
                CandidateFixture {
                    title: "Hello".into(),
                    artist: "Adele".into(),
                    album: Some("25".into()),
                    duration_seconds: Some(295.0),
                    synced: None,
                    plain: Some(ADELE_HELLO_PLAIN.into()),
                    is_target: true,
                },
                CandidateFixture {
                    title: "Someone Like You".into(),
                    artist: "Adele".into(),
                    album: Some("21".into()),
                    duration_seconds: Some(285.0),
                    synced: None,
                    plain: Some("Never mind, I'll find someone like you".into()),
                    is_target: false,
                },
            ],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T26_blowin_in_the_wind_plain_only".into(),
            description: "Bob Dylan - Blowin' in the Wind with plain-only lyrics".into(),
            categories: vec![GoldenCategory::PlainOnly, GoldenCategory::CorrectTags],
            file_path: "Bob Dylan - Blowin' in the Wind.mp3".into(),
            tag_artist: "Bob Dylan".into(),
            tag_title: "Blowin' in the Wind".into(),
            tag_album: "The Freewheelin' Bob Dylan".into(),
            duration_seconds: Some(168.0),
            true_artist: "Bob Dylan".into(),
            true_title: "Blowin' in the Wind".into(),
            true_album: Some("The Freewheelin' Bob Dylan".into()),
            is_instrumental: false,
            local_lrc: Some(BOB_DYLAN_BLOWIN_PLAIN.into()),
            candidates: vec![CandidateFixture {
                title: "Blowin' in the Wind".into(),
                artist: "Bob Dylan".into(),
                album: Some("The Freewheelin' Bob Dylan".into()),
                duration_seconds: Some(168.0),
                synced: None,
                plain: Some(BOB_DYLAN_BLOWIN_PLAIN.into()),
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T27_hallelujah_plain_only".into(),
            description: "Leonard Cohen - Hallelujah with plain-only lyrics".into(),
            categories: vec![GoldenCategory::PlainOnly, GoldenCategory::CorrectTags],
            file_path: "Leonard Cohen - Hallelujah.mp3".into(),
            tag_artist: "Leonard Cohen".into(),
            tag_title: "Hallelujah".into(),
            tag_album: "Various Positions".into(),
            duration_seconds: Some(279.0),
            true_artist: "Leonard Cohen".into(),
            true_title: "Hallelujah".into(),
            true_album: Some("Various Positions".into()),
            is_instrumental: false,
            local_lrc: Some(LEONARD_COHEN_HALLELUJAH_PLAIN.into()),
            candidates: vec![CandidateFixture {
                title: "Hallelujah".into(),
                artist: "Leonard Cohen".into(),
                album: Some("Various Positions".into()),
                duration_seconds: Some(279.0),
                synced: None,
                plain: Some(LEONARD_COHEN_HALLELUJAH_PLAIN.into()),
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        // ---------------------------------------------------------------------
        // 28-30: Instrumental (No lyrics)
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T28_moonlight_sonata_instrumental".into(),
            description: "Beethoven - Moonlight Sonata instrumental track".into(),
            categories: vec![GoldenCategory::Instrumental, GoldenCategory::CorrectTags],
            file_path: "Ludwig van Beethoven - Moonlight Sonata.mp3".into(),
            tag_artist: "Ludwig van Beethoven".into(),
            tag_title: "Moonlight Sonata".into(),
            tag_album: "Piano Sonata No. 14".into(),
            duration_seconds: Some(320.0),
            true_artist: "Ludwig van Beethoven".into(),
            true_title: "Moonlight Sonata".into(),
            true_album: Some("Piano Sonata No. 14".into()),
            is_instrumental: true,
            local_lrc: None,
            candidates: vec![CandidateFixture {
                title: "Moonlight Sonata".into(),
                artist: "Ludwig van Beethoven".into(),
                album: Some("Piano Sonata No. 14".into()),
                duration_seconds: Some(320.0),
                synced: None,
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: false, // Instrumental should not download false lyrics
        },
        GoldenTrack {
            id: "T29_so_what_instrumental".into(),
            description: "Miles Davis - So What jazz instrumental track".into(),
            categories: vec![GoldenCategory::Instrumental, GoldenCategory::CorrectTags],
            file_path: "Miles Davis - So What.mp3".into(),
            tag_artist: "Miles Davis".into(),
            tag_title: "So What".into(),
            tag_album: "Kind of Blue".into(),
            duration_seconds: Some(562.0),
            true_artist: "Miles Davis".into(),
            true_title: "So What".into(),
            true_album: Some("Kind of Blue".into()),
            is_instrumental: true,
            local_lrc: None,
            candidates: vec![CandidateFixture {
                title: "So What".into(),
                artist: "Miles Davis".into(),
                album: Some("Kind of Blue".into()),
                duration_seconds: Some(562.0),
                synced: None,
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: false,
        },
        GoldenTrack {
            id: "T30_time_zimmer_instrumental".into(),
            description: "Hans Zimmer - Time cinematic instrumental".into(),
            categories: vec![GoldenCategory::Instrumental, GoldenCategory::CorrectTags],
            file_path: "Hans Zimmer - Time.mp3".into(),
            tag_artist: "Hans Zimmer".into(),
            tag_title: "Time".into(),
            tag_album: "Inception (OST)".into(),
            duration_seconds: Some(275.0),
            true_artist: "Hans Zimmer".into(),
            true_title: "Time".into(),
            true_album: Some("Inception (OST)".into()),
            is_instrumental: true,
            local_lrc: None,
            candidates: vec![CandidateFixture {
                title: "Time".into(),
                artist: "Hans Zimmer".into(),
                album: Some("Inception (OST)".into()),
                duration_seconds: Some(275.0),
                synced: None,
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: false,
        },
        // ---------------------------------------------------------------------
        // 31-33: Combined Real-World Edge Cases
        // ---------------------------------------------------------------------
        GoldenTrack {
            id: "T31_get_lucky_noisy_and_swapped".into(),
            description: "Daft Punk - Get Lucky with both swapped tags and noisy title".into(),
            categories: vec![
                GoldenCategory::NoisyTitle,
                GoldenCategory::SwappedArtistTitle,
                GoldenCategory::CorrectLrc,
            ],
            file_path: "Daft Punk - Get Lucky (feat. Pharrell Williams) [Radio Edit].mp3".into(),
            tag_artist: "Get Lucky (feat. Pharrell Williams) [Radio Edit]".into(),
            tag_title: "Daft Punk".into(),
            tag_album: "Random Access Memories".into(),
            duration_seconds: Some(248.0),
            true_artist: "Daft Punk".into(),
            true_title: "Get Lucky".into(),
            true_album: Some("Random Access Memories".into()),
            is_instrumental: false,
            local_lrc: Some(DAFT_PUNK_GET_LUCKY_LRC.into()),
            candidates: vec![
                CandidateFixture {
                    title: "Get Lucky".into(),
                    artist: "Daft Punk".into(),
                    album: Some("Random Access Memories".into()),
                    duration_seconds: Some(248.0),
                    synced: Some(DAFT_PUNK_GET_LUCKY_LRC.into()),
                    plain: None,
                    is_target: true,
                },
                CandidateFixture {
                    title: "One More Time".into(),
                    artist: "Daft Punk".into(),
                    album: Some("Discovery".into()),
                    duration_seconds: Some(320.0),
                    synced: Some("[00:10.00]One more time, we're gonna celebrate".into()),
                    plain: None,
                    is_target: false,
                },
            ],
            expected_identification: Some(ExpectedIdentification {
                should_suggest: true,
                is_swapped: true,
                expected_artist: "Daft Punk".into(),
                expected_title: "Get Lucky".into(),
            }),
            expected_wrong_song: false,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T32_billie_jean_noisy_and_offset_1_5".into(),
            description:
                "Michael Jackson - Billie Jean with '- 2003 Remastered' noise and +1.5s offset"
                    .into(),
            categories: vec![
                GoldenCategory::NoisyTitle,
                GoldenCategory::OffsetPlus15,
                GoldenCategory::CorrectTags,
            ],
            file_path: "Michael Jackson - Billie Jean - 2003 Remastered.mp3".into(),
            tag_artist: "Michael Jackson".into(),
            tag_title: "Billie Jean - 2003 Remastered".into(),
            tag_album: "Thriller".into(),
            duration_seconds: Some(294.0),
            true_artist: "Michael Jackson".into(),
            true_title: "Billie Jean".into(),
            true_album: Some("Thriller".into()),
            is_instrumental: false,
            local_lrc: Some(shift_lrc(MICHAEL_JACKSON_BILLIE_JEAN_LRC, 1.5)),
            candidates: vec![CandidateFixture {
                title: "Billie Jean".into(),
                artist: "Michael Jackson".into(),
                album: Some("Thriller".into()),
                duration_seconds: Some(294.0),
                synced: Some(MICHAEL_JACKSON_BILLIE_JEAN_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: None,
            expected_wrong_song: false,
            expected_offset_seconds: Some(1.5),
            expected_select_target: true,
        },
        GoldenTrack {
            id: "T33_viva_la_vida_wrong_tags_wrong_song".into(),
            description: "Coldplay - Viva La Vida with Unknown Artist and Radiohead lyrics in LRC"
                .into(),
            categories: vec![GoldenCategory::WrongTags, GoldenCategory::WrongSongLrc],
            file_path: "Coldplay - Viva La Vida.mp3".into(),
            tag_artist: "Unknown Artist".into(),
            tag_title: "Track 01".into(),
            tag_album: "".into(),
            duration_seconds: Some(242.0),
            true_artist: "Coldplay".into(),
            true_title: "Viva La Vida".into(),
            true_album: Some("Viva la Vida or Death and All His Friends".into()),
            is_instrumental: false,
            local_lrc: Some(
                "\
[00:10.00]When you were here before, couldn't look you in the eye
[00:16.00]You're just like an angel, your skin makes me cry
[00:23.00]You float like a feather in a beautiful world
[00:29.50]I wish I was special, you're so very special
[00:36.00]But I'm a creep, I'm a weirdo"
                    .into(),
            ),
            candidates: vec![CandidateFixture {
                title: "Viva La Vida".into(),
                artist: "Coldplay".into(),
                album: Some("Viva la Vida or Death and All His Friends".into()),
                duration_seconds: Some(242.0),
                synced: Some(COLDPLAY_VIVA_LA_VIDA_LRC.into()),
                plain: None,
                is_target: true,
            }],
            expected_identification: Some(ExpectedIdentification {
                should_suggest: true,
                is_swapped: false,
                expected_artist: "Coldplay".into(),
                expected_title: "Viva La Vida".into(),
            }),
            expected_wrong_song: true,
            expected_offset_seconds: None,
            expected_select_target: true,
        },
    ]
}

/// Run evaluation of identification, selection, wrong-song detection, and offset detection
/// against the golden benchmark dataset and compare metrics against regression thresholds.
pub fn evaluate_golden_set(
    tracks: &[GoldenTrack],
    thresholds: &GoldenThresholds,
) -> GoldenEvaluationReport {
    let mut id_tp = 0;
    let mut id_fp = 0;
    let mut id_tn = 0;
    let mut id_fn = 0;

    let mut sel_tp = 0;
    let mut sel_fp = 0;
    let mut sel_tn = 0;
    let mut sel_fn = 0;

    let mut ws_tp = 0;
    let mut ws_fp = 0;
    let mut ws_tn = 0;
    let mut ws_fn = 0;

    let mut off_tp = 0;
    let mut off_fp = 0;
    let mut off_tn = 0;
    let mut off_fn = 0;

    let mut cat_counts: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();

    let record_cat = |counts: &mut BTreeMap<String, (usize, usize, usize, usize)>,
                      name: &str,
                      is_tp: bool,
                      is_fp: bool,
                      is_tn: bool,
                      is_fn: bool| {
        let entry = counts.entry(name.to_string()).or_insert((0, 0, 0, 0));
        if is_tp {
            entry.0 += 1;
        }
        if is_fp {
            entry.1 += 1;
        }
        if is_tn {
            entry.2 += 1;
        }
        if is_fn {
            entry.3 += 1;
        }
    };

    for track in tracks {
        let track_model = track.to_track();
        let parsed_fn = parse_filename(&track.file_path);

        // ---------------------------------------------------------------------
        // 1. Identification Evaluation
        // ---------------------------------------------------------------------
        let suggestion = parsed_fn
            .as_ref()
            .and_then(|p| match_filename_against_tags(&track_model, p));
        let exp_id = track.expected_identification.as_ref();

        match (exp_id, suggestion.as_ref()) {
            (Some(exp), Some(sugg)) => {
                let artist_matched =
                    similarity(&sugg.suggested_artist, &exp.expected_artist) >= 0.85;
                let title_matched = similarity(&sugg.suggested_title, &exp.expected_title) >= 0.85;
                let swap_flagged =
                    sugg.reason.contains("swapped") || sugg.reason.contains("Swapped");

                if artist_matched && title_matched && (!exp.is_swapped || swap_flagged) {
                    id_tp += 1;
                    record_cat(
                        &mut cat_counts,
                        "id_correct_suggestion",
                        true,
                        false,
                        false,
                        false,
                    );
                } else {
                    id_fn += 1;
                    record_cat(
                        &mut cat_counts,
                        "id_incorrect_suggestion",
                        false,
                        false,
                        false,
                        true,
                    );
                }
            }
            (Some(_), None) => {
                id_fn += 1;
                record_cat(
                    &mut cat_counts,
                    "id_missed_suggestion",
                    false,
                    false,
                    false,
                    true,
                );
            }
            (None, Some(sugg)) => {
                // If no suggestion expected, check if suggestion changed anything or was benign
                let artist_diff = similarity(&sugg.suggested_artist, &track.true_artist) < 0.90;
                let title_diff = similarity(&sugg.suggested_title, &track.true_title) < 0.90;
                if artist_diff || title_diff {
                    id_fp += 1;
                    record_cat(
                        &mut cat_counts,
                        "id_false_suggestion",
                        false,
                        true,
                        false,
                        false,
                    );
                } else {
                    id_tn += 1;
                    record_cat(
                        &mut cat_counts,
                        "id_benign_suggestion",
                        false,
                        false,
                        true,
                        false,
                    );
                }
            }
            (None, None) => {
                id_tn += 1;
                record_cat(
                    &mut cat_counts,
                    "id_correct_noop",
                    false,
                    false,
                    true,
                    false,
                );
            }
        }

        // Test noisy title cleaning
        if track.categories.contains(&GoldenCategory::NoisyTitle) {
            let candidate_title_string = if track
                .categories
                .contains(&GoldenCategory::SwappedArtistTitle)
            {
                &track.tag_artist
            } else {
                &track.tag_title
            };
            let cleaned = clean_title(candidate_title_string);
            let sim = similarity(&cleaned, &track.true_title);
            if sim >= 0.85 {
                record_cat(
                    &mut cat_counts,
                    "noisy_title_clean_success",
                    true,
                    false,
                    false,
                    false,
                );
            } else {
                record_cat(
                    &mut cat_counts,
                    "noisy_title_clean_failure",
                    false,
                    false,
                    false,
                    true,
                );
            }
        }

        // ---------------------------------------------------------------------
        // 2. Candidate Selection Evaluation
        // ---------------------------------------------------------------------
        let candidates_vec: Vec<Candidate> =
            track.candidates.iter().map(|c| c.to_candidate()).collect();
        // Candidate selection runs with either identified metadata or true metadata
        let (query_artist, query_title) = if let Some(ref sugg) = suggestion {
            (sugg.suggested_artist.clone(), sugg.suggested_title.clone())
        } else {
            (track.true_artist.clone(), track.true_title.clone())
        };
        let query_track = Track {
            artist: query_artist,
            title: query_title,
            ..track_model.clone()
        };

        // Filter passing candidates (threshold 0.70 for selection filter)
        let passing_cands: Vec<Candidate> = candidates_vec
            .into_iter()
            .filter(|c| {
                (c.synced.is_some() || c.plain.is_some())
                    && score_candidate(&query_track, c).total >= 0.70
            })
            .collect();

        let ranked = evaluate_and_rank_candidates(&query_track, passing_cands);
        let selected = ranked.first();

        if track.expected_select_target {
            if let Some(sel) = selected {
                let target_match = track
                    .candidates
                    .iter()
                    .find(|c| c.is_target)
                    .map(|t| {
                        similarity(&sel.candidate.title, &t.title) >= 0.80
                            && similarity(&sel.candidate.artist, &t.artist) >= 0.80
                    })
                    .unwrap_or(false);

                if target_match {
                    sel_tp += 1;
                    record_cat(
                        &mut cat_counts,
                        "selection_success",
                        true,
                        false,
                        false,
                        false,
                    );
                } else {
                    sel_fp += 1;
                    record_cat(
                        &mut cat_counts,
                        "selection_wrong_cand",
                        false,
                        true,
                        false,
                        false,
                    );
                }
            } else {
                sel_fn += 1;
                record_cat(
                    &mut cat_counts,
                    "selection_missed",
                    false,
                    false,
                    false,
                    true,
                );
            }
        } else {
            // Instrumental or tracks with no valid lyrics expected
            if selected.is_none() {
                sel_tn += 1;
                record_cat(
                    &mut cat_counts,
                    "selection_correct_noop",
                    false,
                    false,
                    true,
                    false,
                );
            } else {
                sel_fp += 1;
                record_cat(
                    &mut cat_counts,
                    "selection_false_select",
                    false,
                    true,
                    false,
                    false,
                );
            }
        }

        // ---------------------------------------------------------------------
        // 3. Wrong-Song Detection Evaluation
        // ---------------------------------------------------------------------
        let local_text = track.local_lrc.as_deref().unwrap_or("");
        let candidate_models: Vec<Candidate> =
            track.candidates.iter().map(|c| c.to_candidate()).collect();
        let audit = audit_lrc(
            local_text,
            track.duration_seconds,
            &track.true_title,
            &candidate_models,
        );

        let detected_wrong_song = audit
            .issues
            .iter()
            .any(|i| matches!(i, LrcIssue::WrongSong | LrcIssue::TitleMismatch))
            || audit.wrong_song_likelihood >= 0.50;

        if track.expected_wrong_song {
            if detected_wrong_song {
                ws_tp += 1;
                record_cat(
                    &mut cat_counts,
                    "wrong_song_detected",
                    true,
                    false,
                    false,
                    false,
                );
            } else {
                ws_fn += 1;
                record_cat(
                    &mut cat_counts,
                    "wrong_song_missed",
                    false,
                    false,
                    false,
                    true,
                );
            }
        } else {
            if detected_wrong_song {
                ws_fp += 1;
                record_cat(
                    &mut cat_counts,
                    "wrong_song_false_positive",
                    false,
                    true,
                    false,
                    false,
                );
            } else {
                ws_tn += 1;
                record_cat(
                    &mut cat_counts,
                    "wrong_song_correct_negative",
                    false,
                    false,
                    true,
                    false,
                );
            }
        }

        // ---------------------------------------------------------------------
        // 4. Offset Detection Evaluation
        // ---------------------------------------------------------------------
        let detected_offset = audit.estimated_offset_seconds;
        let has_offset_issue = audit
            .issues
            .iter()
            .any(|i| matches!(i, LrcIssue::Offset(_)));

        if let Some(expected_off) = track.expected_offset_seconds {
            if let Some(est) = detected_offset {
                // Must detect offset and be within 0.35s tolerance
                if has_offset_issue && (est - expected_off).abs() <= 0.35 {
                    off_tp += 1;
                    record_cat(
                        &mut cat_counts,
                        "offset_detected_accurate",
                        true,
                        false,
                        false,
                        false,
                    );
                } else {
                    off_fn += 1;
                    record_cat(
                        &mut cat_counts,
                        "offset_inaccurate_or_no_issue",
                        false,
                        false,
                        false,
                        true,
                    );
                }
            } else {
                off_fn += 1;
                record_cat(&mut cat_counts, "offset_missed", false, false, false, true);
            }
        } else {
            // No offset expected
            if has_offset_issue || detected_offset.is_some() {
                off_fp += 1;
                record_cat(
                    &mut cat_counts,
                    "offset_false_alarm",
                    false,
                    true,
                    false,
                    false,
                );
            } else {
                off_tn += 1;
                record_cat(
                    &mut cat_counts,
                    "offset_correct_negative",
                    false,
                    false,
                    true,
                    false,
                );
            }
        }
    }

    let id_score = MetricScore::compute(id_tp, id_fp, id_tn, id_fn);
    let sel_score = MetricScore::compute(sel_tp, sel_fp, sel_tn, sel_fn);
    let ws_score = MetricScore::compute(ws_tp, ws_fp, ws_tn, ws_fn);
    let off_score = MetricScore::compute(off_tp, off_fp, off_tn, off_fn);

    let mut failures = Vec::new();

    if id_score.precision < thresholds.min_identification_precision {
        failures.push(format!(
            "Identification precision regressed: {:.2}% < threshold {:.2}%",
            id_score.precision * 100.0,
            thresholds.min_identification_precision * 100.0
        ));
    }
    if id_score.recall < thresholds.min_identification_recall {
        failures.push(format!(
            "Identification recall regressed: {:.2}% < threshold {:.2}%",
            id_score.recall * 100.0,
            thresholds.min_identification_recall * 100.0
        ));
    }

    if sel_score.precision < thresholds.min_selection_precision {
        failures.push(format!(
            "Selection precision regressed: {:.2}% < threshold {:.2}%",
            sel_score.precision * 100.0,
            thresholds.min_selection_precision * 100.0
        ));
    }
    if sel_score.recall < thresholds.min_selection_recall {
        failures.push(format!(
            "Selection recall regressed: {:.2}% < threshold {:.2}%",
            sel_score.recall * 100.0,
            thresholds.min_selection_recall * 100.0
        ));
    }

    if ws_score.precision < thresholds.min_wrong_song_precision {
        failures.push(format!(
            "Wrong-song detection precision regressed: {:.2}% < threshold {:.2}%",
            ws_score.precision * 100.0,
            thresholds.min_wrong_song_precision * 100.0
        ));
    }
    if ws_score.recall < thresholds.min_wrong_song_recall {
        failures.push(format!(
            "Wrong-song detection recall regressed: {:.2}% < threshold {:.2}%",
            ws_score.recall * 100.0,
            thresholds.min_wrong_song_recall * 100.0
        ));
    }

    if off_score.precision < thresholds.min_offset_precision {
        failures.push(format!(
            "Offset detection precision regressed: {:.2}% < threshold {:.2}%",
            off_score.precision * 100.0,
            thresholds.min_offset_precision * 100.0
        ));
    }
    if off_score.recall < thresholds.min_offset_recall {
        failures.push(format!(
            "Offset detection recall regressed: {:.2}% < threshold {:.2}%",
            off_score.recall * 100.0,
            thresholds.min_offset_recall * 100.0
        ));
    }

    let mut cat_metrics = BTreeMap::new();
    for (cat_name, (tp, fp, tn, fn_cnt)) in cat_counts {
        cat_metrics.insert(cat_name, MetricScore::compute(tp, fp, tn, fn_cnt));
    }

    let passed_ci_gate = failures.is_empty();

    GoldenEvaluationReport {
        total_tracks: tracks.len(),
        identification_metrics: id_score,
        selection_metrics: sel_score,
        wrong_song_metrics: ws_score,
        offset_metrics: off_score,
        category_metrics: cat_metrics,
        passed_ci_gate,
        regression_failures: failures,
    }
}

/// Format the evaluation report into a human-readable Markdown table.
pub fn format_evaluation_report(report: &GoldenEvaluationReport) -> String {
    let mut out = String::new();
    out.push_str("# Golden Benchmark Evaluation Report\n\n");
    out.push_str(&format!(
        "**Total Golden Tracks Evaluated**: {}\n",
        report.total_tracks
    ));
    out.push_str(&format!(
        "**CI Regression Gate**: {}\n\n",
        if report.passed_ci_gate {
            "✅ PASSED"
        } else {
            "❌ FAILED (Regression Detected)"
        }
    ));

    out.push_str("## Core Capabilities Metrics\n\n");
    out.push_str("| Capability | TP | FP | TN | FN | Precision | Recall | F1 Score | Accuracy |\n");
    out.push_str("| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |\n");

    let fmt_row = |name: &str, m: &MetricScore| {
        format!(
            "| **{}** | {} | {} | {} | {} | **{:.1}%** | **{:.1}%** | {:.3} | {:.1}% |\n",
            name,
            m.true_positives,
            m.false_positives,
            m.true_negatives,
            m.false_negatives,
            m.precision * 100.0,
            m.recall * 100.0,
            m.f1,
            m.accuracy * 100.0,
        )
    };

    out.push_str(&fmt_row(
        "Track Identification",
        &report.identification_metrics,
    ));
    out.push_str(&fmt_row("Candidate Selection", &report.selection_metrics));
    out.push_str(&fmt_row("Wrong-Song Detection", &report.wrong_song_metrics));
    out.push_str(&fmt_row(
        "Offset Detection (+/-1.5s, +/-3.0s)",
        &report.offset_metrics,
    ));
    out.push('\n');

    out.push_str("## Subcategory Breakdown\n\n");
    out.push_str("| Subcategory | TP | FP | TN | FN | Precision | Recall | Accuracy |\n");
    out.push_str("| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: |\n");
    for (k, m) in &report.category_metrics {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {:.1}% | {:.1}% | {:.1}% |\n",
            k,
            m.true_positives,
            m.false_positives,
            m.true_negatives,
            m.false_negatives,
            m.precision * 100.0,
            m.recall * 100.0,
            m.accuracy * 100.0,
        ));
    }
    out.push('\n');

    if !report.regression_failures.is_empty() {
        out.push_str("## ⚠️ Regression Failures\n\n");
        for f in &report.regression_failures {
            out.push_str(&format!("- ❌ {}\n", f));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_golden_set_builds_thirty_three_tracks() {
        let set = build_golden_set();
        assert_eq!(
            set.len(),
            33,
            "Golden set must contain exactly 33 benchmark tracks"
        );

        // Verify each requested category is present
        let mut present_cats = std::collections::HashSet::new();
        for t in &set {
            for c in &t.categories {
                present_cats.insert(*c);
            }
        }

        assert!(present_cats.contains(&GoldenCategory::CorrectTags));
        assert!(present_cats.contains(&GoldenCategory::WrongTags));
        assert!(present_cats.contains(&GoldenCategory::SwappedArtistTitle));
        assert!(present_cats.contains(&GoldenCategory::NoisyTitle));
        assert!(present_cats.contains(&GoldenCategory::CorrectLrc));
        assert!(present_cats.contains(&GoldenCategory::WrongSongLrc));
        assert!(present_cats.contains(&GoldenCategory::OffsetPlus15));
        assert!(present_cats.contains(&GoldenCategory::OffsetMinus15));
        assert!(present_cats.contains(&GoldenCategory::OffsetPlus30));
        assert!(present_cats.contains(&GoldenCategory::OffsetMinus30));
        assert!(present_cats.contains(&GoldenCategory::PlainOnly));
        assert!(present_cats.contains(&GoldenCategory::Instrumental));
    }

    #[test]
    fn test_golden_benchmark_evaluation_passes_all_thresholds() {
        let set = build_golden_set();
        let thresholds = GoldenThresholds::default();
        let report = evaluate_golden_set(&set, &thresholds);

        let report_str = format_evaluation_report(&report);
        println!("{}", report_str);

        assert!(
            report.passed_ci_gate,
            "Golden set evaluation failed CI gate:\n{:?}",
            report.regression_failures
        );
    }
}
