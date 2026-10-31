use std::collections::{BTreeMap, BTreeSet};

use crate::cli::SecretScan;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecretKind {
    AwsKey,
    GithubToken,
    GoogleApiKey,
    Jwt,
    OpenAiKey,
    PrivateKey,
    SlackToken,
    StripeKey,
    AnthropicKey,
    GenericAssignment,
}

impl SecretKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::AwsKey => "aws-key",
            Self::GithubToken => "github-token",
            Self::GoogleApiKey => "google-api-key",
            Self::Jwt => "jwt",
            Self::OpenAiKey => "openai-key",
            Self::PrivateKey => "private-key",
            Self::SlackToken => "slack-token",
            Self::StripeKey => "stripe-key",
            Self::AnthropicKey => "anthropic-key",
            Self::GenericAssignment => "generic-assignment",
        }
    }
}

#[derive(Debug, Default)]
pub struct SecretSummary {
    pub kinds: BTreeMap<SecretKind, usize>,
    pub files: BTreeSet<String>,
}

impl SecretSummary {
    pub fn record(&mut self, path: &str, kinds: &[SecretKind]) {
        if kinds.is_empty() {
            return;
        }
        self.files.insert(path.to_string());
        for kind in kinds {
            *self.kinds.entry(*kind).or_default() += 1;
        }
    }

    pub fn total(&self) -> usize {
        self.kinds.values().sum()
    }
}

pub struct ScanResult {
    pub content: Option<String>,
    pub kinds: Vec<SecretKind>,
}

#[derive(Clone, Copy)]
struct Match {
    start: usize,
    end: usize,
    kind: SecretKind,
}

pub fn scan_content(input: &str, mode: SecretScan) -> ScanResult {
    if mode == SecretScan::Off {
        return ScanResult {
            content: Some(input.to_string()),
            kinds: Vec::new(),
        };
    }

    let mut matches = find_matches(input);
    if matches.is_empty() {
        return ScanResult {
            content: Some(input.to_string()),
            kinds: Vec::new(),
        };
    }

    matches.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| (b.end - b.start).cmp(&(a.end - a.start)))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    let mut selected: Vec<Match> = Vec::new();
    for candidate in matches {
        if selected
            .last()
            .is_some_and(|previous| candidate.start < previous.end)
        {
            continue;
        }
        selected.push(candidate);
    }

    let kinds = selected.iter().map(|found| found.kind).collect();
    if mode == SecretScan::Skip {
        return ScanResult {
            content: None,
            kinds,
        };
    }

    let mut redacted = String::with_capacity(input.len());
    let mut cursor = 0;
    for found in selected {
        redacted.push_str(&input[cursor..found.start]);
        redacted.push_str("[REDACTED:");
        redacted.push_str(found.kind.label());
        redacted.push(']');
        cursor = found.end;
    }
    redacted.push_str(&input[cursor..]);
    ScanResult {
        content: Some(redacted),
        kinds,
    }
}

fn find_matches(input: &str) -> Vec<Match> {
    let bytes = input.as_bytes();
    let mut found = Vec::new();
    find_private_keys(bytes, &mut found);
    find_prefixed_tokens(bytes, &mut found);
    find_jwts(bytes, &mut found);
    find_generic_assignments(input, &mut found);
    found
}

fn find_private_keys(bytes: &[u8], found: &mut Vec<Match>) {
    let mut cursor = 0;
    while let Some(relative) = find_bytes(&bytes[cursor..], b"-----BEGIN ") {
        let start = cursor + relative;
        let Some(header_end) = find_bytes(&bytes[start..], b"PRIVATE KEY-----") else {
            cursor = start + 1;
            continue;
        };
        let begin_end = start + header_end + b"PRIVATE KEY-----".len();
        let Some(end_relative) = find_bytes(&bytes[begin_end..], b"-----END ") else {
            cursor = begin_end;
            continue;
        };
        let end_start = begin_end + end_relative;
        let Some(end_header) = find_bytes(&bytes[end_start..], b"PRIVATE KEY-----") else {
            cursor = end_start + 1;
            continue;
        };
        let end = end_start + end_header + b"PRIVATE KEY-----".len();
        found.push(Match {
            start,
            end,
            kind: SecretKind::PrivateKey,
        });
        cursor = end;
    }
}

fn find_prefixed_tokens(bytes: &[u8], found: &mut Vec<Match>) {
    let prefixes: &[(&[u8], SecretKind, usize, Option<usize>)] = &[
        (b"github_pat_", SecretKind::GithubToken, 20, None),
        (b"ghp_", SecretKind::GithubToken, 20, None),
        (b"gho_", SecretKind::GithubToken, 20, None),
        (b"ghu_", SecretKind::GithubToken, 20, None),
        (b"ghs_", SecretKind::GithubToken, 20, None),
        (b"ghr_", SecretKind::GithubToken, 20, None),
        (b"AKIA", SecretKind::AwsKey, 16, Some(16)),
        (b"ASIA", SecretKind::AwsKey, 16, Some(16)),
        (b"AIza", SecretKind::GoogleApiKey, 35, Some(35)),
        (b"sk_live_", SecretKind::StripeKey, 16, None),
        (b"rk_live_", SecretKind::StripeKey, 16, None),
        (b"sk-ant-", SecretKind::AnthropicKey, 16, None),
    ];
    for (prefix, kind, minimum, exact) in prefixes {
        let mut cursor = 0;
        while cursor + prefix.len() <= bytes.len() {
            let Some(relative) = find_bytes(&bytes[cursor..], prefix) else {
                break;
            };
            let start = cursor + relative;
            let payload_start = start + prefix.len();
            let mut end = payload_start;
            while end < bytes.len() && token_byte(bytes[end]) {
                end += 1;
            }
            let payload_len = end - payload_start;
            if payload_len >= *minimum
                && exact.is_none_or(|length| payload_len == length)
                && (start == 0 || !token_byte(bytes[start - 1]))
            {
                found.push(Match {
                    start,
                    end,
                    kind: *kind,
                });
            }
            cursor = start + prefix.len();
        }
    }

    let mut cursor = 0;
    while cursor + 3 <= bytes.len() {
        let Some(relative) = find_bytes(&bytes[cursor..], b"sk-") else {
            break;
        };
        let start = cursor + relative;
        let payload_start = start + 3;
        let mut end = payload_start;
        while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
            end += 1;
        }
        if end - payload_start >= 32
            && (start == 0 || !token_byte(bytes[start - 1]))
            && !bytes[payload_start..].starts_with(b"ant-")
        {
            found.push(Match {
                start,
                end,
                kind: SecretKind::OpenAiKey,
            });
        }
        cursor = start + 3;
    }

    let mut cursor = 0;
    while cursor + 5 <= bytes.len() {
        let Some(relative) = find_bytes(&bytes[cursor..], b"xox") else {
            break;
        };
        let start = cursor + relative;
        if start + 5 <= bytes.len()
            && matches!(bytes[start + 3], b'b' | b'a' | b'p' | b'r' | b's')
            && bytes[start + 4] == b'-'
        {
            let mut end = start + 5;
            while end < bytes.len() && token_byte(bytes[end]) {
                end += 1;
            }
            if end > start + 5 && (start == 0 || !token_byte(bytes[start - 1])) {
                found.push(Match {
                    start,
                    end,
                    kind: SecretKind::SlackToken,
                });
            }
        }
        cursor = start + 3;
    }
}

fn find_jwts(bytes: &[u8], found: &mut Vec<Match>) {
    let mut cursor = 0;
    while cursor + 3 <= bytes.len() {
        let Some(relative) = find_bytes(&bytes[cursor..], b"eyJ") else {
            break;
        };
        let start = cursor + relative;
        if start > 0 && token_byte(bytes[start - 1]) {
            cursor = start + 3;
            continue;
        }
        let mut end = start;
        let mut dots = 0;
        while end < bytes.len() && (token_byte(bytes[end]) || bytes[end] == b'.') {
            if bytes[end] == b'.' {
                dots += 1;
            }
            end += 1;
        }
        if dots == 2 && end > start + 5 && (end == bytes.len() || !token_byte(bytes[end])) {
            found.push(Match {
                start,
                end,
                kind: SecretKind::Jwt,
            });
        }
        cursor = start + 3;
    }
}

fn find_generic_assignments(input: &str, found: &mut Vec<Match>) {
    let mut offset = 0;
    for line in input.split_inclusive('\n') {
        if let Some(equal) = line.find('=') {
            let key = line[..equal]
                .trim_end()
                .rsplit(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if is_secret_assignment(&key) {
                let value = line[equal + 1..].trim_start();
                let leading = line[equal + 1..].len() - value.len();
                if let Some(quote) = value.as_bytes().first().copied() {
                    if quote == b'"' || quote == b'\'' {
                        if let Some(end_quote) =
                            value.as_bytes()[1..].iter().position(|b| *b == quote)
                        {
                            let start = offset + equal + 1 + leading + 1;
                            let end = start + end_quote;
                            let secret = &value[1..1 + end_quote];
                            if secret.len() >= 20
                                && secret.is_ascii()
                                && secret.bytes().all(generic_value_byte)
                                && shannon_entropy(secret) > 4.2
                            {
                                found.push(Match {
                                    start,
                                    end,
                                    kind: SecretKind::GenericAssignment,
                                });
                            }
                        }
                    }
                }
            }
        }
        offset += line.len();
    }
}

fn is_secret_assignment(key: &str) -> bool {
    matches!(
        key,
        "api_key"
            | "api-key"
            | "apikey"
            | "secret"
            | "client_secret"
            | "client-secret"
            | "token"
            | "access_token"
            | "access-token"
            | "password"
    )
}

fn generic_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn shannon_entropy(value: &str) -> f64 {
    let mut counts = [0usize; 256];
    for byte in value.bytes() {
        counts[byte as usize] += 1;
    }
    counts
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| {
            let probability = count as f64 / value.len() as f64;
            -probability * probability.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic(prefix: &str) -> String {
        format!("{prefix}{}", "aB3dE5fG7hJ9kL2mN4pQ6rS8tU0vW1xY")
    }

    fn redacts(value: String, kind: SecretKind) {
        let source = format!("before {value} after\n");
        let result = scan_content(&source, SecretScan::Redact);
        assert!(result
            .content
            .as_deref()
            .is_some_and(|text| { text == format!("before [REDACTED:{}] after\n", kind.label()) }));
        assert_eq!(result.kinds, vec![kind]);
        assert!(result
            .content
            .as_deref()
            .is_some_and(|text| !text.contains(&value)));
    }

    #[test]
    fn redacts_supported_provider_tokens() {
        redacts(format!("AKIA{}", "A1B2C3D4E5F6G7H8"), SecretKind::AwsKey);
        redacts(format!("ASIA{}", "H8G7F6E5D4C3B2A1"), SecretKind::AwsKey);
        for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"] {
            redacts(synthetic(prefix), SecretKind::GithubToken);
        }
        redacts(
            format!("AIza{}", "aB3dE5fG7hJ9kL2mN4pQ6rS8tU0vW1xYz9Q"),
            SecretKind::GoogleApiKey,
        );
        redacts(
            format!("xoxb-{}", "aB3dE5fG7hJ9kL2mN4pQ6rS8tU0vW1xY"),
            SecretKind::SlackToken,
        );
        redacts(
            format!("sk_live_{}", "aB3dE5fG7hJ9kL2mN4pQ6rS8tU0vW1xY"),
            SecretKind::StripeKey,
        );
        redacts(
            format!("rk_live_{}", "aB3dE5fG7hJ9kL2mN4pQ6rS8tU0vW1xY"),
            SecretKind::StripeKey,
        );
        redacts(
            format!("sk-ant-{}", "aB3dE5fG7hJ9kL2mN4pQ6rS8tU0vW1xY"),
            SecretKind::AnthropicKey,
        );
        redacts(
            format!("sk-{}", "aB3dE5fG7hJ9kL2mN4pQ6rS8tU0vW1xY"),
            SecretKind::OpenAiKey,
        );
        redacts(
            format!(
                "eyJ{}.eyJ{}.{}",
                "aB3dE5fG7hJ9kL2mN4pQ6rS8tU0vW1xY", "1aB2cD3eF4gH", "sig9aB2"
            ),
            SecretKind::Jwt,
        );
    }

    #[test]
    fn redacts_private_key_block_and_preserves_surrounding_text() {
        let private_key = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----",
            "aB3dE5fG7hJ9kL2mN4pQ6rS8tU0vW1xY"
        );
        redacts(private_key, SecretKind::PrivateKey);
    }

    #[test]
    fn generic_assignments_require_high_entropy_and_leave_hashes_alone() {
        let value = "aB3dE5fG7hJ9kL2mN4pQ6rS8tU0vW1xY";
        let result = scan_content(&format!("api_key = \"{value}\"\n"), SecretScan::Redact);
        assert!(result
            .content
            .is_some_and(|text| { text == "api_key = \"[REDACTED:generic-assignment]\"\n" }));

        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert!(
            shannon_entropy(sha) <= 4.2,
            "hex SHA entropy was {}",
            shannon_entropy(sha)
        );
        let result = scan_content(&format!("api_key = \"{sha}\"\n"), SecretScan::Redact);
        assert!(
            result.content.is_some_and(|text| text.contains(sha)),
            "hex SHA was unexpectedly matched as {:?}",
            result.kinds
        );
        let base64 = "VGhpcy1pcy1hLXRlc3QtZGF0YS1zdHJpbmc=";
        let result = scan_content(&format!("blob = \"{base64}\"\n"), SecretScan::Redact);
        assert!(result.content.is_some_and(|text| text.contains(base64)));
    }

    #[test]
    fn skip_and_off_modes_have_distinct_behavior() {
        let value = synthetic("ghp_");
        let source = format!("before {value} after\n");
        let skipped = scan_content(&source, SecretScan::Skip);
        assert!(skipped.content.is_none());
        assert_eq!(skipped.kinds, vec![SecretKind::GithubToken]);

        let off = scan_content(&source, SecretScan::Off);
        assert!(off
            .content
            .as_deref()
            .is_some_and(|content| content == source));
        assert!(off.kinds.is_empty());
    }
}
