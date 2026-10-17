mod common;

use common::{Fixture, TrackSpec, write_track};
use muscli::{
    db::Database,
    library::{ScanOptions, scan_to_database},
    lyrics,
};
use std::fs;

#[test]
fn portable_lyrics_follow_the_audio_and_preserve_existing_files() {
    let fixture = Fixture::new();
    let audio = fixture.source().join("song.flac");
    write_track(&audio, &TrackSpec::new("Song"));
    let original_audio = fs::read(&audio).unwrap();
    let mut db = Database::open(&fixture.paths().database_file()).unwrap();
    scan_to_database(
        &mut db,
        fixture.paths(),
        &[fixture.source()],
        &ScanOptions::default(),
    )
    .unwrap();
    let mut track = db.load_tracks().unwrap().remove(0);
    let source = fixture.source().join("import.lrc");
    fs::write(&source, "[00:01.00]first\n[00:02.00]second\n").unwrap();
    lyrics::import(fixture.paths(), &source, &format!("{}.lrc", track.id)).unwrap();
    // An explicit second fetch uses the cached synchronized file offline.
    assert_eq!(
        lyrics::fetch(fixture.paths(), &track).unwrap(),
        lyrics::lyrics_path(fixture.paths(), &track)
    );
    let exported = lyrics::export(fixture.paths(), &track, None).unwrap();
    assert_eq!(exported, audio.with_extension("lrc"));
    assert!(lyrics::export(fixture.paths(), &track, None).is_err());
    assert!(lyrics::export(fixture.paths(), &track, Some(&audio)).is_err());
    assert_eq!(fs::read(&audio).unwrap(), original_audio);

    let portable = fixture.source().join("usb");
    fs::create_dir(&portable).unwrap();
    fs::rename(&audio, portable.join("song.flac")).unwrap();
    fs::rename(&exported, portable.join("song.lrc")).unwrap();
    track.id = "different-pc-and-path".into();
    track.path = portable.join("song.flac");
    let loaded = lyrics::load(fixture.paths(), &track).unwrap();
    assert_eq!(loaded.line_at(1500), Some(0));
    assert_eq!(loaded.line_at(2500), Some(1));
    assert_eq!(loaded.lines[1].text, "second");
}
