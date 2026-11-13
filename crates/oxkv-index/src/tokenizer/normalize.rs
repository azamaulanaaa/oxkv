//! Frozen Unicode normalization, case folding, and word segmentation.
//!
//! The tables in [`tables`] come from Unicode Character Database 16.0.0. The
//! tokenizer applies canonical decomposition, canonical ordering, and
//! canonical composition. It then applies full case folding. Segmentation is
//! a maximal run of word characters as defined by the frozen `Word` subset:
//! letters, marks, numbers, and connector punctuation.

use super::tables::{
    CANONICAL_COMPOSITION, CANONICAL_DECOMPOSITION, COMBINING_CLASS, FULL_CASE_FOLD,
    WORD_CONTINUE_RANGES, WORD_START_RANGES,
};

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

/// Normalizes text to Unicode NFC.
///
/// The function is deterministic. It uses the frozen Unicode 16.0.0 tables.
#[must_use]
pub fn normalize_nfc(text: &str) -> String {
    let mut decomposed = Vec::with_capacity(text.len());
    for character in text.chars() {
        decompose_into(character, &mut decomposed);
    }
    compose(decomposed)
}

/// Returns true when a character starts a word token.
#[must_use]
pub fn is_word_start(character: char) -> bool {
    in_ranges(WORD_START_RANGES, u32::from(character))
}

/// Returns true when a character continues a word token.
#[must_use]
pub fn is_word_continue(character: char) -> bool {
    in_ranges(WORD_CONTINUE_RANGES, u32::from(character))
}

/// Appends the full case fold of one character to the output.
pub fn fold_into(character: char, output: &mut String) {
    match lookup_fold(u32::from(character)) {
        Some(folded) => output.push_str(folded),
        None => output.push(character),
    }
}

/// Returns the full case fold of one character, or `None` when the
/// character maps to itself.
#[must_use]
pub fn fold(character: char) -> Option<&'static str> {
    lookup_fold(u32::from(character))
}

fn in_ranges(ranges: &[(u32, u32)], code_point: u32) -> bool {
    let index = ranges.partition_point(|&(_, end)| end < code_point);
    ranges
        .get(index)
        .is_some_and(|&(start, _)| start <= code_point)
}

fn lookup_fold(code_point: u32) -> Option<&'static str> {
    FULL_CASE_FOLD
        .binary_search_by_key(&code_point, |&(key, _)| key)
        .ok()
        .map(|index| FULL_CASE_FOLD[index].1)
}

fn lookup_decomposition(code_point: u32) -> Option<&'static str> {
    CANONICAL_DECOMPOSITION
        .binary_search_by_key(&code_point, |&(key, _)| key)
        .ok()
        .map(|index| CANONICAL_DECOMPOSITION[index].1)
}

fn lookup_combining_class(code_point: u32) -> u8 {
    COMBINING_CLASS
        .binary_search_by_key(&code_point, |&(key, _)| key)
        .ok()
        .map_or(0, |index| COMBINING_CLASS[index].1)
}

fn lookup_composition(left: char, right: char) -> Option<char> {
    let left = u32::from(left);
    let right = u32::from(right);
    CANONICAL_COMPOSITION
        .binary_search_by(|&(first, second, _)| (first, second).cmp(&(left, right)))
        .ok()
        .and_then(|index| char::from_u32(CANONICAL_COMPOSITION[index].2))
}

fn decompose_into(character: char, output: &mut Vec<char>) {
    let code_point = u32::from(character);
    if decompose_hangul(code_point, output) {
        return;
    }
    if let Some(mapping) = lookup_decomposition(code_point) {
        for part in mapping.chars() {
            push_ordered(output, part);
        }
    } else {
        push_ordered(output, character);
    }
}

fn decompose_hangul(code_point: u32, output: &mut Vec<char>) -> bool {
    if !(S_BASE..S_BASE + S_COUNT).contains(&code_point) {
        return false;
    }
    let syllable = code_point - S_BASE;
    let lead = L_BASE + syllable / N_COUNT;
    let vowel = V_BASE + (syllable % N_COUNT) / T_COUNT;
    let trail = syllable % T_COUNT;
    if let Some(character) = char::from_u32(lead) {
        push_ordered(output, character);
    }
    if let Some(character) = char::from_u32(vowel) {
        push_ordered(output, character);
    }
    if trail != 0
        && let Some(character) = char::from_u32(T_BASE + trail)
    {
        push_ordered(output, character);
    }
    true
}

fn push_ordered(output: &mut Vec<char>, character: char) {
    let combining_class = lookup_combining_class(u32::from(character));
    output.push(character);
    if combining_class == 0 {
        return;
    }
    let mut index = output.len() - 1;
    while index > 0 {
        let previous = lookup_combining_class(u32::from(output[index - 1]));
        if previous == 0 || previous <= combining_class {
            break;
        }
        output.swap(index - 1, index);
        index -= 1;
    }
}

fn compose(decomposed: Vec<char>) -> String {
    let mut output: Vec<char> = Vec::with_capacity(decomposed.len());
    let mut starter_index: Option<usize> = None;
    let mut last_class = 0_u8;
    for character in decomposed {
        let combining_class = lookup_combining_class(u32::from(character));
        if let Some(index) = starter_index {
            let blocked = combining_class != 0 && last_class >= combining_class;
            if !blocked && let Some(composite) = compose_pair(output[index], character) {
                output[index] = composite;
                continue;
            }
        }
        if combining_class == 0 {
            starter_index = Some(output.len());
            last_class = 0;
        } else {
            last_class = combining_class;
        }
        output.push(character);
    }
    output.into_iter().collect()
}

fn compose_pair(left: char, right: char) -> Option<char> {
    compose_hangul(left, right).or_else(|| lookup_composition(left, right))
}

fn compose_hangul(left: char, right: char) -> Option<char> {
    let left = u32::from(left);
    let right = u32::from(right);
    if (L_BASE..L_BASE + L_COUNT).contains(&left) && (V_BASE..V_BASE + V_COUNT).contains(&right) {
        let lead = left - L_BASE;
        let vowel = right - V_BASE;
        return char::from_u32(S_BASE + (lead * V_COUNT + vowel) * T_COUNT);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&left)
        && (left - S_BASE).is_multiple_of(T_COUNT)
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&right)
    {
        let syllable = left - S_BASE;
        let trail = right - T_BASE;
        return char::from_u32(S_BASE + syllable + trail);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_composes_latin_letters() {
        assert_eq!(normalize_nfc("e\u{0301}"), "\u{00e9}");
        assert_eq!(normalize_nfc("A\u{030a}"), "\u{00c5}");
        assert_eq!(normalize_nfc("\u{0153}"), "\u{0153}");
    }

    #[test]
    fn nfc_reorders_marks_before_composition() {
        assert_eq!(normalize_nfc("a\u{0323}\u{0302}"), "\u{1ead}");
        assert_eq!(normalize_nfc("e\u{0301}\u{0323}"), "\u{1eb9}\u{0301}");
    }

    #[test]
    fn nfc_composes_hangul() {
        assert_eq!(normalize_nfc("\u{1100}\u{1161}\u{11a8}"), "\u{ac01}");
        assert_eq!(normalize_nfc("\u{ac01}"), "\u{ac01}");
    }

    #[test]
    fn nfc_keeps_non_composable_marks() {
        assert_eq!(normalize_nfc("\u{0915}\u{093c}"), "\u{0915}\u{093c}");
        assert_eq!(normalize_nfc("\u{05d0}\u{05b8}"), "\u{05d0}\u{05b8}");
    }

    #[test]
    fn nfc_is_idempotent() {
        for text in [
            "Café",
            "Straße",
            "\u{1ead}",
            "\u{1100}\u{1161}",
            "日本語",
            "\u{0915}\u{093c}",
        ] {
            let once = normalize_nfc(text);
            assert_eq!(normalize_nfc(&once), once);
        }
    }

    #[test]
    fn case_folding_is_full() {
        let mut folded = String::new();
        for character in "Straße".chars() {
            fold_into(character, &mut folded);
        }
        assert_eq!(folded, "strasse");

        let mut dotted = String::new();
        for character in "\u{0130}".chars() {
            fold_into(character, &mut dotted);
        }
        assert_eq!(dotted, "i\u{0307}");
    }

    #[test]
    fn word_classes_are_frozen() {
        assert!(is_word_start('a'));
        assert!(is_word_start('9'));
        assert!(is_word_start('_'));
        assert!(is_word_start('\u{65e5}'));
        assert!(is_word_continue('\u{0301}'));
        assert!(!is_word_start('\u{0301}'));
        assert!(!is_word_continue(' '));
        assert!(!is_word_continue(','));
    }
}
