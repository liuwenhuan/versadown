use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Direct,
    Magnet,
    Torrent,
    Thunder,
    QuarkShare,
    PikPakShare,
    Text,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Candidate {
    pub kind: CandidateKind,
    pub uri: String,
    pub auto_start: bool,
}

pub fn classify_input(input: &str) -> Candidate {
    let uri = input.trim().to_string();
    let lower = uri.to_lowercase();
    let kind = if lower.starts_with("magnet:") {
        CandidateKind::Magnet
    } else if lower.starts_with("thunder://") {
        CandidateKind::Thunder
    } else if lower.contains("pan.quark.cn") || lower.contains("quark.cn/s/") {
        CandidateKind::QuarkShare
    } else if lower.contains("mypikpak.com") || lower.contains("pikpak.me") {
        CandidateKind::PikPakShare
    } else if lower.ends_with(".torrent") {
        CandidateKind::Torrent
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        CandidateKind::Direct
    } else {
        CandidateKind::Text
    };
    let auto_start = matches!(kind, CandidateKind::Direct | CandidateKind::Thunder);
    Candidate {
        kind,
        uri,
        auto_start,
    }
}

pub fn extract_candidates(text: &str) -> Vec<Candidate> {
    let pattern = Regex::new(r"(?i)(magnet:\?[^\s<>]+|thunder://[^\s<>]+|https?://[^\s<>]+)")
        .expect("static candidate pattern");
    pattern
        .find_iter(text)
        .map(|match_| {
            classify_input(
                match_
                    .as_str()
                    .trim_end_matches(|character: char| ".,;，。)】".contains(character)),
            )
        })
        .collect()
}
