use std::{collections::BTreeMap, error::Error, fmt};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

pub(crate) const REDACTION_MARKER: &str = "[REDACTED]";
pub(crate) const MAX_REDACTION_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONFIGURED_OCCURRENCES: usize = 64;
const MAX_CONFIGURED_VALUE_BYTES: usize = 4096;
const MAX_CONFIGURED_AGGREGATE_BYTES: usize = 65_536;
const MIN_OBVIOUS_SECRET_SUFFIX_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecretRedactionError {
    TooManyConfiguredOccurrences,
    ConfiguredOccurrenceTooLarge,
    ConfiguredAggregateTooLarge,
    ConfiguredMatcherBuildFailed,
    InputTooLarge,
    OutputTooLarge,
    OutputContainsProhibitedMaterial,
}

impl fmt::Display for SecretRedactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooManyConfiguredOccurrences => {
                "eval config contains more than 64 sensitive env value occurrences"
            }
            Self::ConfiguredOccurrenceTooLarge => {
                "eval config contains a sensitive env value larger than 4096 bytes"
            }
            Self::ConfiguredAggregateTooLarge => {
                "eval config sensitive env values exceed 65536 aggregate bytes"
            }
            Self::ConfiguredMatcherBuildFailed => "redaction configured matcher could not be built",
            Self::InputTooLarge => "redaction input exceeds 2097152 bytes",
            Self::OutputTooLarge => "redaction output exceeds its bounded sink",
            Self::OutputContainsProhibitedMaterial => {
                "redaction output contains prohibited credential material"
            }
        })
    }
}

impl Error for SecretRedactionError {}

#[derive(Clone)]
pub(crate) struct SecretRedactor {
    configured_matcher: Option<AhoCorasick>,
}

impl fmt::Debug for SecretRedactor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretRedactor")
            .field(
                "configured_pattern_count",
                &self
                    .configured_matcher
                    .as_ref()
                    .map_or(0, AhoCorasick::patterns_len),
            )
            .finish()
    }
}

impl SecretRedactor {
    pub(crate) fn empty() -> Self {
        Self {
            configured_matcher: None,
        }
    }
    pub(crate) fn from_env_maps<'a>(
        env_maps: impl IntoIterator<Item = &'a BTreeMap<String, String>>,
    ) -> Result<Self, SecretRedactionError> {
        let mut occurrence_count = 0usize;
        let mut aggregate_bytes = 0usize;
        let mut configured = Vec::new();
        for env in env_maps {
            for (name, value) in env {
                if value.is_empty() || !is_sensitive_name(name) {
                    continue;
                }
                occurrence_count = occurrence_count
                    .checked_add(1)
                    .ok_or(SecretRedactionError::TooManyConfiguredOccurrences)?;
                if occurrence_count > MAX_CONFIGURED_OCCURRENCES {
                    return Err(SecretRedactionError::TooManyConfiguredOccurrences);
                }
                if value.len() > MAX_CONFIGURED_VALUE_BYTES {
                    return Err(SecretRedactionError::ConfiguredOccurrenceTooLarge);
                }
                aggregate_bytes = aggregate_bytes
                    .checked_add(value.len())
                    .ok_or(SecretRedactionError::ConfiguredAggregateTooLarge)?;
                if aggregate_bytes > MAX_CONFIGURED_AGGREGATE_BYTES {
                    return Err(SecretRedactionError::ConfiguredAggregateTooLarge);
                }
                configured.push(value.as_bytes().to_vec());
            }
        }
        configured
            .sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        configured.dedup();
        let configured_matcher = if configured.is_empty() {
            None
        } else {
            Some(
                AhoCorasickBuilder::new()
                    .match_kind(MatchKind::LeftmostLongest)
                    .build(&configured)
                    .map_err(|_| SecretRedactionError::ConfiguredMatcherBuildFailed)?,
            )
        };
        Ok(Self { configured_matcher })
    }

    pub(crate) fn from_eval_config(
        config: &seaf_core::EvalConfig,
    ) -> Result<Self, SecretRedactionError> {
        Self::from_env_maps(config.evals.required.iter().map(|check| &check.env))
    }

    pub(crate) fn redact_bytes(
        &self,
        input: &[u8],
        sink_max_bytes: usize,
    ) -> Result<Vec<u8>, SecretRedactionError> {
        if input.len() > MAX_REDACTION_BYTES {
            return Err(SecretRedactionError::InputTooLarge);
        }
        let sink_max_bytes = sink_max_bytes.min(MAX_REDACTION_BYTES);
        let configured_matches = self.configured_match_lengths(input);
        let mut output = Vec::with_capacity(input.len());
        let mut cursor = 0usize;
        while cursor < input.len() {
            let configured = usize::from(configured_matches[cursor]);
            let configured = (configured > 0).then_some(configured);
            let obvious = obvious_secret_len(input, cursor);
            let assignment = sensitive_assignment_value(input, cursor);
            let configured_or_obvious = configured.into_iter().chain(obvious).max();
            if let Some((_, value_end)) = assignment.filter(|(_, value_end)| {
                configured_or_obvious.is_none_or(|length| value_end - cursor >= length)
            }) {
                output.extend_from_slice(REDACTION_MARKER.as_bytes());
                cursor = value_end;
            } else if let Some(length) = configured_or_obvious {
                output.extend_from_slice(REDACTION_MARKER.as_bytes());
                cursor += length;
            } else {
                output.push(input[cursor]);
                cursor += 1;
            }
            if output.len() > sink_max_bytes {
                return Err(SecretRedactionError::OutputTooLarge);
            }
        }
        if self.contains_prohibited_bytes(&output)? {
            return Err(SecretRedactionError::OutputContainsProhibitedMaterial);
        }
        Ok(output)
    }

    pub(crate) fn redact_string(
        &self,
        input: &str,
        sink_max_bytes: usize,
    ) -> Result<String, SecretRedactionError> {
        let bytes = self.redact_bytes(input.as_bytes(), sink_max_bytes)?;
        String::from_utf8(bytes).map_err(|_| SecretRedactionError::OutputTooLarge)
    }

    pub(crate) fn contains_prohibited_bytes(
        &self,
        input: &[u8],
    ) -> Result<bool, SecretRedactionError> {
        if input.len() > MAX_REDACTION_BYTES {
            return Err(SecretRedactionError::InputTooLarge);
        }
        let configured_matches = self.configured_match_lengths(input);
        let mut cursor = 0usize;
        while cursor < input.len() {
            if configured_matches[cursor] > 0
                || obvious_secret_len(input, cursor).is_some()
                || sensitive_assignment_value(input, cursor).is_some()
            {
                return Ok(true);
            }
            cursor += 1;
        }
        Ok(false)
    }

    fn configured_match_lengths(&self, input: &[u8]) -> Vec<u16> {
        let mut match_lengths = vec![0; input.len()];
        let Some(matcher) = &self.configured_matcher else {
            return match_lengths;
        };
        for configured_match in matcher.find_iter(input) {
            match_lengths[configured_match.start()] = u16::try_from(configured_match.len())
                .expect("configured values are capped at 4096 bytes");
        }
        match_lengths
    }
}

pub(crate) fn is_sensitive_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD"]
        .iter()
        .any(|needle| upper.contains(needle))
}

fn obvious_secret_len(input: &[u8], start: usize) -> Option<usize> {
    const PREFIXES: [&[u8]; 7] = [
        b"sk-proj-",
        b"github_pat_",
        b"ghp_",
        b"xoxb-",
        b"xoxp-",
        b"xoxa-",
        b"sk-",
    ];
    if start > 0 && is_obvious_secret_byte(input[start - 1]) {
        return None;
    }
    let input = &input[start..];
    for prefix in PREFIXES {
        if !input.starts_with(prefix) {
            continue;
        }
        let suffix_len = input[prefix.len()..]
            .iter()
            .take_while(|byte| is_obvious_secret_byte(**byte))
            .count();
        if suffix_len >= MIN_OBVIOUS_SECRET_SUFFIX_BYTES {
            return Some(prefix.len() + suffix_len);
        }
    }
    None
}

fn is_obvious_secret_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn sensitive_assignment_value(input: &[u8], start: usize) -> Option<(usize, usize)> {
    if start > 0 && is_name_byte(input[start - 1]) {
        return None;
    }
    let mut cursor = start;
    while cursor < input.len() && is_name_byte(input[cursor]) {
        cursor += 1;
    }
    if cursor == start {
        return None;
    }
    let name = std::str::from_utf8(&input[start..cursor]).ok()?;
    if !is_sensitive_name(name) {
        return None;
    }
    if cursor < input.len() && matches!(input[cursor], b'\'' | b'"') {
        cursor += 1;
    }
    while cursor < input.len() && input[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if !matches!(input.get(cursor), Some(b'=') | Some(b':')) {
        return None;
    }
    cursor += 1;
    while cursor < input.len() && input[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    let quote = input
        .get(cursor)
        .copied()
        .filter(|byte| matches!(byte, b'\'' | b'"'));
    if quote.is_some() {
        cursor += 1;
    }
    let value_start = cursor;
    let value_end = if let Some(quote) = quote {
        unescaped_quote_position(input, value_start, quote)
            .map(|position| position + 1)
            .unwrap_or(input.len())
    } else {
        unquoted_assignment_end(input, value_start)
    };
    (value_end > value_start).then_some((value_start, value_end))
}

fn unquoted_assignment_end(input: &[u8], start: usize) -> usize {
    let marker = REDACTION_MARKER.as_bytes();
    let mut cursor = start;
    while cursor < input.len() {
        if input[cursor..].starts_with(marker) {
            cursor += marker.len();
            continue;
        }
        let byte = input[cursor];
        if byte.is_ascii_whitespace() || matches!(byte, b',' | b';' | b'}' | b']') {
            break;
        }
        cursor += 1;
    }
    cursor
}

fn unescaped_quote_position(input: &[u8], start: usize, quote: u8) -> Option<usize> {
    let mut escaped = false;
    for (offset, byte) in input[start..].iter().copied().enumerate() {
        if byte == quote && !escaped {
            return Some(start + offset);
        }
        if byte == b'\\' {
            escaped = !escaped;
        } else {
            escaped = false;
        }
    }
    None
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{SecretRedactionError, SecretRedactor, MAX_REDACTION_BYTES, REDACTION_MARKER};

    fn env(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn corpus_bounds_count_sensitive_occurrences_before_deduplication() {
        let sixty_four = (0..64)
            .map(|index| (format!("TOKEN_{index}"), "same".to_string()))
            .collect::<BTreeMap<_, _>>();
        assert!(SecretRedactor::from_env_maps([&sixty_four]).is_ok());

        let sixty_five = (0..65)
            .map(|index| (format!("TOKEN_{index}"), "same".to_string()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            SecretRedactor::from_env_maps([&sixty_five]).unwrap_err(),
            SecretRedactionError::TooManyConfiguredOccurrences
        );

        assert!(SecretRedactor::from_env_maps([&env(&[("API_KEY", &"x".repeat(4096))])]).is_ok());
        assert_eq!(
            SecretRedactor::from_env_maps([&env(&[("API_KEY", &"x".repeat(4097))])]).unwrap_err(),
            SecretRedactionError::ConfiguredOccurrenceTooLarge
        );

        let at_aggregate = (0..16)
            .map(|index| (format!("SECRET_{index}"), "x".repeat(4096)))
            .collect::<BTreeMap<_, _>>();
        assert!(SecretRedactor::from_env_maps([&at_aggregate]).is_ok());
        let over_aggregate = (0..17)
            .map(|index| {
                let size = if index == 16 { 1 } else { 4096 };
                (format!("SECRET_{index}"), "x".repeat(size))
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            SecretRedactor::from_env_maps([&over_aggregate]).unwrap_err(),
            SecretRedactionError::ConfiguredAggregateTooLarge
        );
    }

    #[test]
    fn empty_and_non_sensitive_values_do_not_consume_the_corpus() {
        let entries = (0..100)
            .flat_map(|index| {
                [
                    (format!("PUBLIC_{index}"), "visible".to_string()),
                    (format!("TOKEN_{index}"), String::new()),
                ]
            })
            .collect::<BTreeMap<_, _>>();
        assert!(SecretRedactor::from_env_maps([&entries]).is_ok());
    }

    #[test]
    fn configured_matches_are_deduplicated_longest_first() {
        let redactor = SecretRedactor::from_env_maps([&env(&[
            ("API_KEY", "abc"),
            ("ACCESS_TOKEN", "abcdef"),
            ("OTHER_SECRET", "abc"),
        ])])
        .unwrap();
        assert_eq!(
            redactor
                .redact_bytes(b"abcdef abc", 2 * 1024 * 1024)
                .unwrap(),
            b"[REDACTED] [REDACTED]"
        );
    }

    #[test]
    fn configured_secret_equal_to_the_marker_fails_closed() {
        let redactor =
            SecretRedactor::from_env_maps([&env(&[("MARKER_PASSWORD", REDACTION_MARKER)])])
                .unwrap();

        assert_eq!(
            redactor.redact_bytes(REDACTION_MARKER.as_bytes(), MAX_REDACTION_BYTES),
            Err(SecretRedactionError::OutputContainsProhibitedMaterial)
        );
    }

    #[test]
    fn same_cursor_matching_is_longest_across_configured_obvious_and_assignment_forms() {
        let redactor = SecretRedactor::from_env_maps([&env(&[
            ("SHORT_TOKEN", "sk-"),
            ("NAME_SECRET", "TOKEN"),
        ])])
        .unwrap();
        let output = redactor
            .redact_bytes(
                b"sk-abcdefghijklmnopTAIL TOKEN=plain-secret",
                MAX_REDACTION_BYTES,
            )
            .unwrap();
        assert_eq!(
            output, b"[REDACTED] [REDACTED]",
            "short configured matches must not expose longer classified material"
        );
    }

    #[test]
    fn obvious_forms_and_sensitive_assignments_cross_common_punctuation() {
        let redactor = SecretRedactor::from_env_maps(std::iter::empty()).unwrap();
        let input = br#"TOKEN=plain-secret {"password":"quoted-secret","value":"sk-proj-abcdefghijklmnop"} yaml: ghp_abcdefghijklmnop near=sk-short ask-abcdefghijklmnop"#;
        let output = redactor.redact_bytes(input, 2 * 1024 * 1024).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("plain-secret"));
        assert!(!output.contains("quoted-secret"));
        assert!(!output.contains("sk-proj-abcdefghijklmnop"));
        assert!(!output.contains("ghp_abcdefghijklmnop"));
        assert!(output.contains("sk-short"));
        assert!(output.contains("ask-abcdefghijklmnop"));
    }

    #[test]
    fn sensitive_assignments_ignore_escaped_single_and_double_quotes() {
        let redactor = SecretRedactor::from_env_maps(std::iter::empty()).unwrap();
        let input = br#"PASSWORD="abc\"secret-tail" API_TOKEN='abc\'token-tail'"#;

        let output = redactor.redact_bytes(input, MAX_REDACTION_BYTES).unwrap();

        assert_eq!(output, br#"[REDACTED] [REDACTED]"#);
    }

    #[test]
    fn sensitive_assignments_with_only_escaped_quotes_redact_to_input_end() {
        let redactor = SecretRedactor::from_env_maps(std::iter::empty()).unwrap();
        for input in [
            br#"PASSWORD="abc\"secret-tail"#.as_slice(),
            br#"API_TOKEN='abc\'token-tail"#.as_slice(),
        ] {
            assert_eq!(
                redactor.redact_bytes(input, MAX_REDACTION_BYTES).unwrap(),
                REDACTION_MARKER.as_bytes()
            );
        }
    }

    #[test]
    fn long_common_prefixes_preserve_a_maximum_clean_input_exactly() {
        let entries = (0_u8..16)
            .map(|index| {
                let mut value = "a".repeat(4095);
                value.push(char::from(b'b' + index));
                (format!("TOKEN_{index}"), value)
            })
            .collect::<BTreeMap<_, _>>();
        let redactor = SecretRedactor::from_env_maps([&entries]).unwrap();
        let input = vec![b'a'; MAX_REDACTION_BYTES];

        assert_eq!(
            redactor.redact_bytes(&input, MAX_REDACTION_BYTES),
            Ok(input.clone())
        );
        assert_eq!(redactor.contains_prohibited_bytes(&input), Ok(false));
    }

    #[test]
    fn raw_ingress_does_not_trust_markers_inside_configured_secrets() {
        let redactor = SecretRedactor::from_env_maps([&env(&[(
            "CROSS_MARKER_SECRET",
            "prefix[REDACTED]suffix",
        )])])
        .unwrap();

        assert_eq!(
            redactor
                .redact_bytes(b"prefix[REDACTED]suffix", MAX_REDACTION_BYTES)
                .unwrap(),
            b"[REDACTED]"
        );
        assert_eq!(
            redactor.contains_prohibited_bytes(b"prefix[REDACTED]suffix"),
            Ok(true)
        );
    }

    #[test]
    fn raw_ingress_redacts_the_complete_sensitive_assignment_across_a_marker() {
        let redactor = SecretRedactor::from_env_maps(std::iter::empty()).unwrap();

        assert_eq!(
            redactor
                .redact_bytes(
                    b"TOKEN=[REDACTED]unclassified-tail visible",
                    MAX_REDACTION_BYTES,
                )
                .unwrap(),
            b"[REDACTED] visible"
        );
        assert_eq!(
            redactor.contains_prohibited_bytes(b"TOKEN=[REDACTED]unclassified-tail visible"),
            Ok(true)
        );
    }

    #[test]
    fn redaction_output_is_raw_safe_and_idempotent_without_marker_trust() {
        let redactor =
            SecretRedactor::from_env_maps([&env(&[("API_TOKEN", "TOKEN=[REDACTED]")])]).unwrap();

        let output = redactor
            .redact_bytes(b"TOKEN=raw-unclassified-tail", MAX_REDACTION_BYTES)
            .unwrap();

        assert_eq!(output, REDACTION_MARKER.as_bytes());
        assert_eq!(redactor.contains_prohibited_bytes(&output), Ok(false));
        assert_eq!(
            redactor.redact_bytes(&output, MAX_REDACTION_BYTES),
            Ok(output)
        );
    }

    #[test]
    fn marker_spanning_output_collision_fails_closed() {
        let secret = "before [REDACTED] after";
        let redactor = SecretRedactor::from_env_maps([&env(&[("API_TOKEN", secret)])]).unwrap();

        assert_eq!(
            redactor.redact_bytes(b"before TOKEN=raw after", MAX_REDACTION_BYTES),
            Err(SecretRedactionError::OutputContainsProhibitedMaterial)
        );
    }

    #[test]
    fn maximum_input_with_a_boundary_secret_is_redacted() {
        let secret = "z".repeat(4096);
        let redactor = SecretRedactor::from_env_maps([&env(&[("API_TOKEN", &secret)])]).unwrap();
        let mut input = vec![b'a'; MAX_REDACTION_BYTES - secret.len()];
        input.extend_from_slice(secret.as_bytes());

        let output = redactor.redact_bytes(&input, MAX_REDACTION_BYTES).unwrap();

        assert_eq!(
            output.len(),
            input.len() - secret.len() + REDACTION_MARKER.len()
        );
        assert!(output.ends_with(REDACTION_MARKER.as_bytes()));
        assert_eq!(redactor.contains_prohibited_bytes(&input), Ok(true));
    }

    #[test]
    fn global_input_and_sink_output_bounds_fail_without_partial_output() {
        let redactor = SecretRedactor::from_env_maps([&env(&[("KEY", "x")])]).unwrap();
        assert_eq!(
            redactor.redact_bytes(&vec![b'a'; 2 * 1024 * 1024 + 1], 2 * 1024 * 1024),
            Err(SecretRedactionError::InputTooLarge)
        );
        assert_eq!(
            redactor.redact_bytes(b"clean", 4),
            Err(SecretRedactionError::OutputTooLarge)
        );
        assert_eq!(
            redactor.redact_bytes(b"xxxxxxxx", 8),
            Err(SecretRedactionError::OutputTooLarge)
        );
        assert_eq!(redactor.redact_bytes(b"clean", 5).unwrap(), b"clean");
        assert_eq!(
            redactor
                .redact_bytes(&vec![b'a'; 2 * 1024 * 1024], 2 * 1024 * 1024)
                .unwrap()
                .len(),
            2 * 1024 * 1024
        );
    }

    #[test]
    fn byte_matching_handles_multibyte_invalid_utf8_and_secret_at_retention_boundary() {
        let redactor = SecretRedactor::from_env_maps([&env(&[("API_TOKEN", "tøken")])]).unwrap();
        let mut input = vec![0xff];
        input.extend_from_slice("tøken".as_bytes());
        input.push(0xfe);
        assert_eq!(
            redactor.redact_bytes(&input, 2 * 1024 * 1024).unwrap(),
            [vec![0xff], REDACTION_MARKER.as_bytes().to_vec(), vec![0xfe]].concat()
        );

        let boundary = [vec![b'a'; 4095], "tøken".as_bytes().to_vec()].concat();
        let output = redactor.redact_bytes(&boundary, 2 * 1024 * 1024).unwrap();
        assert!(output.ends_with(REDACTION_MARKER.as_bytes()));
    }
}
