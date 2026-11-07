//! The fuzzy filter of the palette and the switcher (RF-NAV-01, RF-NAV-04):
//! the query's characters must appear in order, ignoring case and spaces.
//! Runs of consecutive characters and characters at the start of a word
//! score higher, as in the prototype.

/// How well a text matches a query, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Match {
    /// Higher is better. An empty query scores 0.
    pub(crate) score: u32,
    /// The byte offset in the text of each matched character.
    pub(crate) hits: Vec<usize>,
}

/// Points for a character that follows the previous hit directly.
const CONSECUTIVE: u32 = 3;
/// Points for any other hit.
const SCATTERED: u32 = 1;
/// Extra points for a hit at the start of the text or of a word.
const WORD_START: u32 = 2;

/// Matches `query` against `text`, or `None` when some character of the
/// query is missing. Each query character takes its first occurrence after
/// the previous hit.
pub(crate) fn fuzzy(query: &str, text: &str) -> Option<Match> {
    let mut chars = text.char_indices().peekable();
    let mut previous: Option<char> = None;
    let mut last_end: Option<usize> = None;
    let mut score = 0;
    let mut hits = Vec::new();
    for wanted in query.chars().filter(|c| !c.is_whitespace()) {
        loop {
            let (at, c) = chars.next()?;
            let before = previous.replace(c);
            if !same_letter(c, wanted) {
                continue;
            }
            score += if last_end == Some(at) {
                CONSECUTIVE
            } else {
                SCATTERED
            };
            if before.is_none_or(is_separator) {
                score += WORD_START;
            }
            hits.push(at);
            last_end = Some(at + c.len_utf8());
            break;
        }
    }
    Some(Match { score, hits })
}

/// The items that match, best first; ties keep the items' order, so a
/// caller that lists recent things first keeps them first.
pub(crate) fn rank<'a>(
    query: &str,
    texts: impl IntoIterator<Item = &'a str>,
) -> Vec<(usize, Match)> {
    let mut ranked: Vec<(usize, Match)> = texts
        .into_iter()
        .enumerate()
        .filter_map(|(at, text)| fuzzy(query, text).map(|m| (at, m)))
        .collect();
    ranked.sort_by_key(|(_, m)| std::cmp::Reverse(m.score));
    ranked
}

fn same_letter(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, ':' | '/' | '-' | '_' | '.' | '\\' | '›')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order<'a>(query: &str, texts: &[&'a str]) -> Vec<&'a str> {
        rank(query, texts.iter().copied())
            .into_iter()
            .map(|(at, _)| texts[at])
            .collect()
    }

    #[test]
    fn rf_nav_01_an_empty_query_lists_everything_in_order() {
        let texts = ["pane: split right", "tab: next tab", "theme: toggle"];
        assert_eq!(order("", &texts), texts);
        assert_eq!(order("   ", &texts), texts);
        assert_eq!(fuzzy("", "anything").map(|m| m.score), Some(0));
    }

    #[test]
    fn rf_nav_01_the_filter_ignores_case() {
        assert!(fuzzy("SPLIT", "pane: split right").is_some());
        assert!(fuzzy("split", "Pane: SPLIT Right").is_some());
        assert_eq!(
            fuzzy("Ñ", "año").map(|m| m.hits),
            Some(vec![1]),
            "non-ASCII letters fold too, and hits are byte offsets"
        );
    }

    #[test]
    fn rf_nav_01_characters_must_appear_in_order() {
        assert!(fuzzy("spr", "pane: split right").is_some());
        assert!(fuzzy("rsp", "pane: split right").is_none());
        assert!(fuzzy("splitx", "pane: split right").is_none());
    }

    #[test]
    fn rf_nav_01_spaces_in_the_query_are_ignored() {
        assert_eq!(
            fuzzy("split right", "pane: split right").map(|m| m.hits.len()),
            Some(10)
        );
    }

    #[test]
    fn rf_nav_01_a_run_at_a_word_start_ranks_above_scattered_letters() {
        assert_eq!(
            order("tab", &["workspace: toggle tab bar", "tab: next tab"]),
            ["tab: next tab", "workspace: toggle tab bar"]
        );
        assert_eq!(order("ab", &["xaxb", "ab"]), ["ab", "xaxb"]);
        assert_eq!(order("b", &["ab", "a b"]), ["a b", "ab"]);
    }

    #[test]
    fn rf_nav_01_ties_keep_the_given_order() {
        let texts = [
            "pane: split right",
            "pane: split down",
            "pane: split right with Claude Code",
        ];
        assert_eq!(order("split", &texts), texts);
    }

    #[test]
    fn the_hits_point_at_the_matched_characters() {
        let m = fuzzy("sr", "pane: split right").unwrap();
        assert_eq!(m.hits, vec![6, 12]);
        // s at a word start (1 + 2), r at a word start (1 + 2).
        assert_eq!(m.score, 6);
    }
}
