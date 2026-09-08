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
