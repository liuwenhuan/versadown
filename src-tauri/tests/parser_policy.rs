use alldownloader_lib::{
    classify_input, extract_candidates, validate_processing, CandidateKind, ProcessStep,
};

#[test]
fn recognizes_direct_http_magnet_and_thunder() {
    assert_eq!(
        classify_input("magnet:?xt=urn:btih:abc").kind,
        CandidateKind::Magnet
    );
    assert!(classify_input("https://example.com/a.zip").auto_start);
    assert_eq!(
        classify_input("thunder://QUFodHRwOi8vYS9iWlpa").kind,
        CandidateKind::Thunder
    );
}

#[test]
fn recognizes_ed2k_ftp_and_native_media_protocols() {
    let ed2k = "ed2k://|file|Example%20Archive.zip|1048576|0123456789ABCDEF0123456789ABCDEF|/";
    assert_eq!(classify_input(ed2k).kind, CandidateKind::Ed2k);
    assert!(classify_input(ed2k).auto_start);

    for source in ["ftp://demo:secret@example.test/releases/file.iso"] {
        let candidate = classify_input(source);
        assert_eq!(candidate.kind, CandidateKind::Ftp, "{source}");
        assert!(candidate.auto_start, "{source}");
    }

    let hls = classify_input("https://media.example.test/master.m3u8?token=temporary");
    assert_eq!(hls.kind, CandidateKind::Hls);
    assert!(hls.auto_start);

    let dash = classify_input("https://media.example.test/manifest.MPD#playback");
    assert_eq!(dash.kind, CandidateKind::Dash);
    assert!(dash.auto_start);
}

#[test]
fn rejects_malformed_ed2k_file_links_and_extracts_complete_valid_links() {
    for source in [
        "ed2k://|file|missing-fields|/",
        "ed2k://|file|name.bin|not-a-size|0123456789ABCDEF0123456789ABCDEF|/",
        "ed2k://|file|name.bin|42|short-hash|/",
    ] {
        assert_eq!(classify_input(source).kind, CandidateKind::Text, "{source}");
    }

    let ed2k = "ed2k://|file|Example%20Archive.zip|1048576|0123456789ABCDEF0123456789ABCDEF|/";
    let candidates = extract_candidates(&format!(
        "ED2K {ed2k}；FTP ftp://example.test/pub/file.iso；HLS https://media.test/a.m3u8"
    ));
    assert_eq!(candidates.len(), 3);
    assert_eq!(candidates[0].uri, ed2k);
    assert_eq!(candidates[0].kind, CandidateKind::Ed2k);
    assert_eq!(candidates[1].kind, CandidateKind::Ftp);
    assert_eq!(candidates[2].kind, CandidateKind::Hls);
}

#[test]
fn recognizes_torrent_urls_with_query_and_fragment() {
    for source in [
        "https://example.com/releases/game.torrent?token=temporary",
        "https://example.com/releases/game.TORRENT#download",
        "https://example.com/releases/game.torrent?token=temporary#download",
    ] {
        let candidate = classify_input(source);
        assert_eq!(candidate.kind, CandidateKind::Torrent, "{source}");
        assert!(!candidate.auto_start, "torrent selection must be confirmed");
    }
}

#[test]
fn extracts_multiple_links_from_descriptive_text() {
    let candidates = extract_candidates("地址 https://a.test/a.zip 以及 magnet:?xt=urn:btih:abc");
    assert_eq!(candidates.len(), 2);
}

#[test]
fn rejects_program_execution_processing() {
    assert!(validate_processing(&ProcessStep::execute("setup.exe")).is_err());
}
