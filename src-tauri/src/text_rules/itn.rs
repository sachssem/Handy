//! Inverse text normalization (ITN) for German and English number words.
//!
//! Converts spoken numbers into digits, e.g. `fünfhundertneununddreißig` →
//! `539`, `five hundred thirty nine` → `539`, and decimal patterns
//! `zwei Komma fünf` → `2,5` / `five point three` → `5.3`.
//!
//! The engine is deliberately conservative to avoid false positives in normal
//! speech:
//! - German articles `ein` / `eine` are never converted on their own.
//! - A *standalone* simple number is only converted when its value is at least
//!   [`ITN_MIN_STANDALONE`] (German orthography convention). Compound German
//!   numbers and multi-word English numbers always convert, as do decimals.

use super::{lex, Token};

/// Standalone simple numbers below this value are left as words. German
/// orthography spells small numbers out; this constant is intentionally not a
/// user-facing setting.
const ITN_MIN_STANDALONE: u32 = 13;

/// Which language's number vocabulary to use.
#[derive(Clone, Copy)]
enum Lang {
    German,
    English,
}

/// Apply inverse text normalization to `text`.
pub fn apply_itn(text: &str) -> String {
    let tokens = lex(text);
    let mut out = String::new();
    let mut i = 0;

    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(word) => {
                if let Some((replacement, next)) = convert_at(&tokens, i, word) {
                    out.push_str(&replacement);
                    i = next;
                } else {
                    out.push_str(word);
                    i += 1;
                }
            }
            Token::Space(space) => {
                out.push_str(space);
                i += 1;
            }
            Token::Other(other) => {
                out.push_str(other);
                i += 1;
            }
        }
    }

    out
}

/// Attempt a number conversion starting at word token `i`.
///
/// Returns the replacement text plus the token index just past the consumed
/// tokens, or `None` when nothing should be converted here.
fn convert_at(tokens: &[Token], i: usize, word: &str) -> Option<(String, usize)> {
    let lower = word.to_lowercase();

    // Never convert the standalone German article.
    if lower == "ein" || lower == "eine" {
        return None;
    }

    // German numbers are a single compound token.
    if let Some(int_value) = parse_german(&lower) {
        if let Some((fraction, after)) = parse_decimal_tail(tokens, i + 1, "komma", Lang::German) {
            return Some((format!("{},{}", int_value, fraction), after));
        }
        if int_value >= ITN_MIN_STANDALONE {
            return Some((int_value.to_string(), i + 1));
        }
        return None;
    }

    // English numbers span multiple words.
    if let Some((value, run_end, word_count)) = parse_english_run(tokens, i) {
        if let Some((fraction, after)) = parse_decimal_tail(tokens, run_end, "point", Lang::English)
        {
            return Some((format!("{}.{}", value, fraction), after));
        }
        if word_count >= 2 || value >= ITN_MIN_STANDALONE {
            return Some((value.to_string(), run_end));
        }
        return None;
    }

    None
}

/// Parse the fractional tail of a decimal: a connective word (`komma` /
/// `point`) followed by one or more single-digit words. Returns the digit
/// string and the token index just past the last digit word.
fn parse_decimal_tail(
    tokens: &[Token],
    start: usize,
    connective: &str,
    lang: Lang,
) -> Option<(String, usize)> {
    let mut ti = skip_spaces(tokens, start);

    match tokens.get(ti) {
        Some(Token::Word(word)) if word.to_lowercase() == connective => ti += 1,
        _ => return None,
    }

    ti = skip_spaces(tokens, ti);

    let mut digits = String::new();
    let mut end = ti;
    while let Some(Token::Word(word)) = tokens.get(ti) {
        match single_digit(&word.to_lowercase(), lang) {
            Some(d) => {
                digits.push((b'0' + d as u8) as char);
                end = ti + 1;
                ti = skip_spaces(tokens, ti + 1);
            }
            None => break,
        }
    }

    if digits.is_empty() {
        None
    } else {
        Some((digits, end))
    }
}

fn skip_spaces(tokens: &[Token], mut ti: usize) -> usize {
    while let Some(Token::Space(_)) = tokens.get(ti) {
        ti += 1;
    }
    ti
}

/// Parse a run of consecutive English number words starting at token `start`.
///
/// Returns `(value, end_index, word_count)` where `end_index` is the token
/// index just past the last number word (trailing whitespace is left intact)
/// and `word_count` excludes the `and` connective.
fn parse_english_run(tokens: &[Token], start: usize) -> Option<(u32, usize, usize)> {
    let mut result: u32 = 0;
    let mut current: u32 = 0;
    let mut count = 0;
    let mut end = start;
    let mut matched_any = false;
    let mut ti = start;

    while let Some(Token::Word(raw)) = tokens.get(ti) {
        let word = raw.to_lowercase();

        if let Some(value) = english_below_hundred(&word) {
            current += value;
            count += 1;
            matched_any = true;
            end = ti + 1;
        } else if word == "hundred" {
            if current == 0 {
                current = 1;
            }
            current *= 100;
            count += 1;
            matched_any = true;
            end = ti + 1;
        } else if word == "thousand" {
            if current == 0 {
                current = 1;
            }
            result += current * 1000;
            current = 0;
            count += 1;
            matched_any = true;
            end = ti + 1;
        } else if word == "and" && matched_any {
            // Connective: consume only if it stays between number words. `end`
            // is left untouched so a trailing `and` is not included.
        } else {
            break;
        }

        ti = skip_spaces(tokens, ti + 1);
    }

    if matched_any {
        Some((result + current, end, count))
    } else {
        None
    }
}

/// Parse a German number word (up to 999_999) as a single token.
fn parse_german(word: &str) -> Option<u32> {
    if let Some(pos) = word.find("tausend") {
        let left = &word[..pos];
        let right = &word[pos + "tausend".len()..];
        let thousands = if left.is_empty() {
            1
        } else {
            parse_below_thousand(left)?
        };
        let rest = parse_below_thousand(right)?;
        return Some(thousands * 1000 + rest);
    }

    parse_below_thousand(word)
}

fn parse_below_thousand(word: &str) -> Option<u32> {
    if word.is_empty() {
        return Some(0);
    }

    if let Some(pos) = word.find("hundert") {
        let left = &word[..pos];
        let right = &word[pos + "hundert".len()..];
        let hundreds = if left.is_empty() {
            1
        } else {
            german_unit(left)?
        };
        let rest = parse_below_hundred(right)?;
        return Some(hundreds * 100 + rest);
    }

    parse_below_hundred(word)
}

fn parse_below_hundred(word: &str) -> Option<u32> {
    if word.is_empty() {
        return Some(0);
    }
    if let Some(value) = german_unit(word) {
        return Some(value);
    }
    if let Some(value) = german_teen(word) {
        return Some(value);
    }
    if let Some(value) = german_tens(word) {
        return Some(value);
    }

    // Compound: <unit> "und" <tens>, e.g. "einundzwanzig" → 21.
    if let Some(pos) = word.find("und") {
        let unit_part = &word[..pos];
        let tens_part = &word[pos + "und".len()..];
        if let (Some(unit), Some(tens)) = (german_unit(unit_part), german_tens(tens_part)) {
            if (1..=9).contains(&unit) {
                return Some(unit + tens);
            }
        }
    }

    None
}

fn german_unit(word: &str) -> Option<u32> {
    Some(match word {
        "null" => 0,
        "eins" | "ein" | "eine" => 1,
        "zwei" => 2,
        "drei" => 3,
        "vier" => 4,
        "fünf" => 5,
        "sechs" => 6,
        "sieben" => 7,
        "acht" => 8,
        "neun" => 9,
        _ => return None,
    })
}

fn german_teen(word: &str) -> Option<u32> {
    Some(match word {
        "zehn" => 10,
        "elf" => 11,
        "zwölf" => 12,
        "dreizehn" => 13,
        "vierzehn" => 14,
        "fünfzehn" => 15,
        "sechzehn" => 16,
        "siebzehn" => 17,
        "achtzehn" => 18,
        "neunzehn" => 19,
        _ => return None,
    })
}

fn german_tens(word: &str) -> Option<u32> {
    Some(match word {
        "zwanzig" => 20,
        "dreißig" => 30,
        "vierzig" => 40,
        "fünfzig" => 50,
        "sechzig" => 60,
        "siebzig" => 70,
        "achtzig" => 80,
        "neunzig" => 90,
        _ => return None,
    })
}

fn english_below_hundred(word: &str) -> Option<u32> {
    Some(match word {
        "zero" => 0,
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        "thirteen" => 13,
        "fourteen" => 14,
        "fifteen" => 15,
        "sixteen" => 16,
        "seventeen" => 17,
        "eighteen" => 18,
        "nineteen" => 19,
        "twenty" => 20,
        "thirty" => 30,
        "forty" => 40,
        "fifty" => 50,
        "sixty" => 60,
        "seventy" => 70,
        "eighty" => 80,
        "ninety" => 90,
        _ => return None,
    })
}

/// Map a single-digit number word (0–9) to its value for the given language.
fn single_digit(word: &str, lang: Lang) -> Option<u32> {
    match lang {
        Lang::German => german_unit(word).filter(|d| *d <= 9),
        Lang::English => english_below_hundred(word).filter(|d| *d <= 9),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn german_compound() {
        assert_eq!(apply_itn("fünfhundertneununddreißig"), "539");
    }

    #[test]
    fn german_compound_mixed_case() {
        assert_eq!(apply_itn("Einundzwanzig Katzen"), "21 Katzen");
    }

    #[test]
    fn english_multi_word() {
        assert_eq!(apply_itn("five hundred thirty nine"), "539");
    }

    #[test]
    fn article_guard() {
        assert_eq!(apply_itn("ein Haus"), "ein Haus");
        assert_eq!(apply_itn("eine Katze"), "eine Katze");
    }

    #[test]
    fn standalone_threshold() {
        assert_eq!(apply_itn("zwei Äpfel"), "zwei Äpfel");
        assert_eq!(apply_itn("dreizehn"), "13");
    }

    #[test]
    fn german_decimal() {
        assert_eq!(apply_itn("zwei Komma fünf"), "2,5");
    }

    #[test]
    fn english_decimal() {
        assert_eq!(apply_itn("five point three"), "5.3");
    }

    #[test]
    fn komma_not_decimal_when_not_number() {
        // "Komma" that is not between numbers is left for the substitution pass.
        assert_eq!(
            apply_itn("Ende Komma dann weiter"),
            "Ende Komma dann weiter"
        );
    }
}
