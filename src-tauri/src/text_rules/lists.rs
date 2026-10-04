//! Spoken lists → one item per line.
//!
//! Recognized markers (each family is a built-in key that can be disabled):
//! - **numbered** — `Punkt eins|1 … Punkt zwei|2 …` (DE), `number|item|point
//!   one|1 …` (EN) → `1. …`;
//! - **ordinals** — `erstens … zweitens …` (DE), `first|firstly … second …`
//!   (EN) → `1. …`;
//! - **bullets** — `nächster|neuer Punkt` (DE), `next point|item` (EN) → `- …`.
//!   An optional `neue Liste` / `new list` before the first item is dropped.
//!
//! False-positive guards (precision over recall):
//! - at least **two** markers of one family, numbered/ordinal ones in order
//!   starting at 1 (`1, 2, 3 …`, no gaps);
//! - a marker directly after a determiner/preposition is prose (`um Punkt 1`,
//!   `ein neuer Punkt`), see [`super::context::is_veto_word`];
//! - ordinals must open an ASR segment (text start or after punctuation) and
//!   every ordinal item may hold at most [`ORDINAL_MAX_ITEM_WORDS`] words, so
//!   flowing prose ("Erstens habe ich keine Zeit, zweitens …") stays prose.
//!   They additionally require a preceding label/colon or at least three
//!   items; two standalone rhetorical reasons remain prose;
//! - numbered and ordinal items starting with common finite verbs, pronouns
//!   or particles are prose (`Punkt eins ist erledigt`, `erstens bin ich müde`).
//!   Imperative tasks (`fix tests`, `ship it`) remain valid list items.
//!
//! Output: the text before the first marker becomes a label line (a trailing
//! `:` is added when it ends without punctuation), then one item per line with
//! the recognizer's trailing `. , ;` stripped. The last item ends at its first
//! sentence end; any remaining text follows on its own line. For bullets the
//! first item is the text right before the first marker: after `neue Liste`,
//! after a `:` label, or else the last clause of the preceding text (which may
//! be a whole short sentence: `Ich brauche Milch, nächster Punkt …` →
//! `- Ich brauche Milch`).

use super::context::{self, After, Before};
use super::{builtin_active, lex, TextRule, Token};

pub(crate) const KEY_NUMBERED_DE: &str = "Punkt eins";
pub(crate) const KEY_NUMBERED_EN: &str = "number one";
pub(crate) const KEY_ORDINAL_DE: &str = "erstens";
pub(crate) const KEY_ORDINAL_EN: &str = "first";
pub(crate) const KEY_BULLET_DE: &str = "nächster Punkt";
pub(crate) const KEY_BULLET_EN: &str = "next item";

/// Ordinal list items longer than this are treated as prose.
const ORDINAL_MAX_ITEM_WORDS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Numbered,
    Ordinal,
    Bullet,
}

/// A list marker spanning tokens `start..end`, with its number (0 = bullet).
#[derive(Debug, Clone, Copy)]
struct Marker {
    start: usize,
    end: usize,
    family: Family,
    number: u32,
}

struct Enabled {
    numbered_de: bool,
    numbered_en: bool,
    ordinal_de: bool,
    ordinal_en: bool,
    bullet_de: bool,
    bullet_en: bool,
}

/// Apply list formatting to `text` (first valid list only).
pub fn apply_lists(text: &str, custom: &[TextRule], disabled: &[String]) -> String {
    let active = |key| builtin_active(key, custom, disabled);
    let enabled = Enabled {
        numbered_de: active(KEY_NUMBERED_DE),
        numbered_en: active(KEY_NUMBERED_EN),
        ordinal_de: active(KEY_ORDINAL_DE),
        ordinal_en: active(KEY_ORDINAL_EN),
        bullet_de: active(KEY_BULLET_DE),
        bullet_en: active(KEY_BULLET_EN),
    };

    let tokens = lex(text);
    let markers = find_markers(&tokens, &enabled);

    for family in [Family::Numbered, Family::Ordinal, Family::Bullet] {
        let run = longest_run(&markers, family);
        if run.len() < 2 {
            continue;
        }
        if let Some(formatted) = format_list(&tokens, &run, family) {
            return formatted;
        }
    }

    text.to_string()
}

fn find_markers(tokens: &[Token], enabled: &Enabled) -> Vec<Marker> {
    let mut markers = Vec::new();

    for (i, token) in tokens.iter().enumerate() {
        let Token::Word(word) = token else { continue };
        let lower = word.to_lowercase();
        let prev = context::before(tokens, i);
        let vetoed = matches!(prev, Before::Word(p) if context::is_veto_word(tokens[p].text()));

        // Two-word markers: "<head> <number>" and "<adjective> <noun>".
        let next = match context::after(tokens, i + 1) {
            After::Word(j) => Some(j),
            _ => None,
        };
        let next_lower = next.map(|j| tokens[j].text().to_lowercase());

        let numbered_head = (enabled.numbered_de && lower == "punkt")
            || (enabled.numbered_en && matches!(lower.as_str(), "number" | "item" | "point"));
        if numbered_head && !vetoed {
            if let (Some(j), Some(n)) = (next, next_lower.as_deref().and_then(list_number)) {
                markers.push(Marker {
                    start: i,
                    end: j + 1,
                    family: Family::Numbered,
                    number: n,
                });
                continue;
            }
        }

        let bullet = match (lower.as_str(), next_lower.as_deref()) {
            ("nächster" | "neuer", Some("punkt" | "stichpunkt")) => enabled.bullet_de,
            ("next" | "new", Some("point" | "item")) => enabled.bullet_en,
            _ => false,
        };
        if bullet && !vetoed {
            if let Some(j) = next {
                markers.push(Marker {
                    start: i,
                    end: j + 1,
                    family: Family::Bullet,
                    number: 0,
                });
                continue;
            }
        }

        // Ordinals must open an ASR segment (start or after punctuation).
        if !matches!(prev, Before::Word(_)) {
            let ordinal = ordinal_de(&lower)
                .filter(|_| enabled.ordinal_de)
                .or_else(|| ordinal_en(&lower).filter(|_| enabled.ordinal_en));
            if let Some(n) = ordinal {
                markers.push(Marker {
                    start: i,
                    end: i + 1,
                    family: Family::Ordinal,
                    number: n,
                });
            }
        }
    }

    markers
}

/// The longest run of `family` markers: bullets in any number, numbered and
/// ordinal markers counting 1, 2, 3 … without gaps.
fn longest_run(markers: &[Marker], family: Family) -> Vec<Marker> {
    let of_family: Vec<Marker> = markers
        .iter()
        .filter(|m| m.family == family)
        .copied()
        .collect();
    if family == Family::Bullet {
        return of_family;
    }

    let mut best: Vec<Marker> = Vec::new();
    let mut current: Vec<Marker> = Vec::new();
    for marker in of_family {
        let expected = current.last().map_or(1, |m| m.number + 1);
        if marker.number == expected {
            current.push(marker);
        } else {
            if current.len() > best.len() {
                best = std::mem::take(&mut current);
            }
            current.clear();
            if marker.number == 1 {
                current.push(marker);
            }
        }
    }
    if current.len() > best.len() {
        best = current;
    }
    best
}

fn format_list(tokens: &[Token], run: &[Marker], family: Family) -> Option<String> {
    let first = run[0];
    let prefix = &tokens[..first.start];

    // Items between markers; the last runs to the first sentence end.
    let mut items: Vec<String> = Vec::new();
    for pair in run.windows(2) {
        items.push(clean_item(&concat(&tokens[pair[0].end..pair[1].start])));
    }
    let tail = concat(&tokens[run[run.len() - 1].end..]);
    let (last, suffix) = split_first_sentence(&tail);
    items.push(clean_item(&last));

    let (label, leading_item) = split_prefix(prefix, family == Family::Bullet);
    if let Some(item) = leading_item {
        items.insert(0, item);
    }

    if items.iter().any(String::is_empty) {
        return None;
    }
    if family != Family::Bullet && items.iter().any(|item| starts_prose_clause(item)) {
        return None;
    }
    if family == Family::Ordinal
        && ((label.is_none() && run.len() < 3)
            || items
                .iter()
                .any(|item| item.split_whitespace().count() > ORDINAL_MAX_ITEM_WORDS))
    {
        return None;
    }

    let mut lines: Vec<String> = Vec::new();
    if let Some(label) = label {
        lines.push(label);
    }
    for (idx, item) in items.iter().enumerate() {
        lines.push(match family {
            Family::Bullet => format!("- {}", item),
            _ => format!("{}. {}", idx + 1, item),
        });
    }
    let mut out = lines.join("\n");
    if let Some(suffix) = suffix {
        out.push('\n');
        out.push_str(&suffix);
    }
    Some(out)
}

/// A small bilingual guard for clauses rather than item fragments/tasks.
fn starts_prose_clause(item: &str) -> bool {
    let Some(Token::Word(first)) = lex(item).into_iter().next() else {
        return false;
    };
    matches!(
        first.to_lowercase().as_str(),
        "bin"
            | "bist"
            | "ist"
            | "sind"
            | "seid"
            | "war"
            | "waren"
            | "habe"
            | "hast"
            | "hat"
            | "haben"
            | "habt"
            | "wird"
            | "werden"
            | "kann"
            | "können"
            | "muss"
            | "müssen"
            | "soll"
            | "sollen"
            | "will"
            | "wollen"
            | "ich"
            | "du"
            | "er"
            | "sie"
            | "es"
            | "wir"
            | "ihr"
            | "man"
            | "zu"
            | "auch"
            | "aber"
            | "doch"
            | "ja"
            | "weil"
            | "dass"
            | "am"
            | "is"
            | "are"
            | "was"
            | "were"
            | "has"
            | "have"
            | "had"
            | "does"
            | "did"
            | "can"
            | "could"
            | "would"
            | "should"
            | "must"
            | "i"
            | "you"
            | "he"
            | "she"
            | "it"
            | "we"
            | "they"
            | "too"
            | "also"
            | "but"
            | "so"
            | "because"
            | "that"
            | "of"
    )
}

/// Split the text before the first marker into an optional label line and,
/// for bullet lists, the first item.
fn split_prefix(prefix: &[Token], bullets: bool) -> (Option<String>, Option<String>) {
    // "neue Liste" / "new list" is a command: drop it, it ends the label.
    if let Some((start, end)) = find_list_start(prefix) {
        let label = make_label(&concat(&prefix[..start]));
        let rest = clean_item(&concat(&prefix[end..]));
        let leading = if bullets && !rest.is_empty() {
            Some(rest)
        } else {
            if !rest.is_empty() {
                // Numbered list with text after "neue Liste": it is the label.
                return (make_label(&rest), None);
            }
            None
        };
        return (label, leading);
    }

    let text = concat(prefix);
    // The recognizer's comma/period right before the first marker is not a
    // clause boundary to cut at.
    let text = text.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ';'));
    if !bullets {
        return (make_label(text), None);
    }

    // Bullets: the first item follows the label colon or the last clause end.
    let cut = text
        .rfind(':')
        .or_else(|| text.rfind(['.', '!', '?', ',', ';']))
        .map(|pos| pos + 1);
    match cut {
        Some(pos) => {
            let item = clean_item(&text[pos..]);
            let label = make_label(&text[..pos]);
            (label, (!item.is_empty()).then_some(item))
        }
        None => {
            let item = clean_item(text);
            (None, (!item.is_empty()).then_some(item))
        }
    }
}

fn find_list_start(tokens: &[Token]) -> Option<(usize, usize)> {
    tokens.iter().enumerate().find_map(|(i, token)| {
        let Token::Word(word) = token else {
            return None;
        };
        let After::Word(j) = context::after(tokens, i + 1) else {
            return None;
        };
        let pair = format!(
            "{} {}",
            word.to_lowercase(),
            tokens[j].text().to_lowercase()
        );
        matches!(pair.as_str(), "neue liste" | "new list").then_some((i, j + 1))
    })
}

/// A label line: trimmed, a dangling `,`/`;` dropped, `:` added when the label
/// ends in a word. Empty → none.
fn make_label(text: &str) -> Option<String> {
    let mut label = text
        .trim()
        .trim_end_matches([',', ';'])
        .trim_end()
        .to_string();
    if label.is_empty() {
        return None;
    }
    if label.chars().last().is_some_and(char::is_alphanumeric) {
        label.push(':');
    }
    Some(label)
}

/// Strip whitespace and the recognizer's `. , ; :` from both ends of an item.
fn clean_item(text: &str) -> String {
    text.trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':'))
        .to_string()
}

/// Split `text` after its first sentence end (`.` `!` `?` followed by
/// whitespace or the end). Returns the first sentence and the trimmed rest.
fn split_first_sentence(text: &str) -> (String, Option<String>) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (k, &(pos, c)) in chars.iter().enumerate() {
        if matches!(c, '.' | '!' | '?') {
            let next = chars.get(k + 1).map(|&(_, n)| n);
            if next.is_none_or(char::is_whitespace) {
                let rest = text[pos + c.len_utf8()..].trim();
                return (
                    text[..pos].to_string(),
                    (!rest.is_empty()).then(|| rest.to_string()),
                );
            }
        }
    }
    (text.to_string(), None)
}

fn concat(tokens: &[Token]) -> String {
    tokens.iter().map(Token::text).collect()
}

/// A list number: digits 1–20 or a number word up to ten (DE/EN).
pub(crate) fn list_number(word: &str) -> Option<u32> {
    if let Ok(n) = word.parse::<u32>() {
        return (1..=20).contains(&n).then_some(n);
    }
    Some(match word {
        "eins" | "one" => 1,
        "zwei" | "two" => 2,
        "drei" | "three" => 3,
        "vier" | "four" => 4,
        "fünf" | "five" => 5,
        "sechs" | "six" => 6,
        "sieben" | "seven" => 7,
        "acht" | "eight" => 8,
        "neun" | "nine" => 9,
        "zehn" | "ten" => 10,
        _ => return None,
    })
}

fn ordinal_de(word: &str) -> Option<u32> {
    Some(match word {
        "erstens" => 1,
        "zweitens" => 2,
        "drittens" => 3,
        "viertens" => 4,
        "fünftens" => 5,
        "sechstens" => 6,
        "siebtens" => 7,
        "achtens" => 8,
        "neuntens" => 9,
        "zehntens" => 10,
        _ => return None,
    })
}

fn ordinal_en(word: &str) -> Option<u32> {
    Some(match word {
        "first" | "firstly" => 1,
        "second" | "secondly" => 2,
        "third" | "thirdly" => 3,
        "fourth" | "fourthly" => 4,
        "fifth" | "fifthly" => 5,
        "sixth" => 6,
        "seventh" => 7,
        "eighth" => 8,
        "ninth" => 9,
        "tenth" => 10,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists(text: &str) -> String {
        apply_lists(text, &[], &[])
    }

    #[test]
    fn user_sample_numbered_punkt() {
        // Sample 1: "Einkaufsliste Doppelpunkt Punkt eins Milch Punkt zwei Eier
        // Punkt drei Brot" as Parakeet wrote it.
        assert_eq!(
            lists("Einkaufsliste: Punkt 1 Milch. Punkt 2 Eier und. Punkt drei Brot."),
            "Einkaufsliste:\n1. Milch\n2. Eier und\n3. Brot"
        );
    }

    #[test]
    fn numbered_without_asr_punctuation_gets_label_colon() {
        assert_eq!(
            lists("Einkaufsliste Punkt eins Milch Punkt zwei Eier"),
            "Einkaufsliste:\n1. Milch\n2. Eier"
        );
    }

    #[test]
    fn user_sample_ordinals() {
        assert_eq!(
            lists("Erstens Milch, zweitens Eier, drittens Brot."),
            "1. Milch\n2. Eier\n3. Brot"
        );
    }

    #[test]
    fn user_sample_bullets_with_neue_liste() {
        assert_eq!(
            lists("Neue Liste Milch, nächster Punkt Eier, nächster Punkt Brot."),
            "- Milch\n- Eier\n- Brot"
        );
    }

    #[test]
    fn bullets_without_list_start_use_last_clause() {
        assert_eq!(
            lists("Gut. Milch, nächster Punkt Eier, nächster Punkt Brot."),
            "Gut.\n- Milch\n- Eier\n- Brot"
        );
        assert_eq!(
            lists("Zutaten: Milch, nächster Punkt Eier, nächster Punkt Brot."),
            "Zutaten:\n- Milch\n- Eier\n- Brot"
        );
    }

    #[test]
    fn english_lists() {
        assert_eq!(
            lists("Groceries: first, milk. Second, eggs. Third, bread."),
            "Groceries:\n1. milk\n2. eggs\n3. bread"
        );
        assert_eq!(
            lists("New list milk, next item eggs, next item bread."),
            "- milk\n- eggs\n- bread"
        );
        assert_eq!(
            lists("Todo number one fix tests number two ship it"),
            "Todo:\n1. fix tests\n2. ship it"
        );
    }

    #[test]
    fn text_after_last_item_moves_to_its_own_line() {
        assert_eq!(
            lists("Punkt 1 Milch. Punkt 2 Brot. Danach gehe ich heim."),
            "1. Milch\n2. Brot\nDanach gehe ich heim."
        );
    }

    #[test]
    fn ordinals_in_flowing_prose_stay_prose() {
        let prose = "Erstens habe ich keine Zeit, zweitens habe ich keine Lust.";
        assert_eq!(lists(prose), prose);
        let prose = "First of all, I think the second option is better.";
        assert_eq!(lists(prose), prose);
    }

    #[test]
    fn ordinal_lists_require_label_or_three_fragment_items() {
        for text in [
            "erstens bin ich müde, zweitens hungrig",
            "Erstens zu teuer, zweitens zu spät.",
            "Erstens Milch, zweitens Eier.",
            "First too expensive, second too late.",
            "Gründe: erstens bin ich müde, zweitens hungrig",
            "First I am tired, second hungry, third sleepy.",
        ] {
            assert_eq!(lists(text), text, "input: {text}");
        }
        assert_eq!(
            lists("Zutaten: erstens Milch, zweitens Eier."),
            "Zutaten:\n1. Milch\n2. Eier"
        );
        assert_eq!(
            lists("First milk, second eggs, third bread."),
            "1. milk\n2. eggs\n3. bread"
        );
    }

    #[test]
    fn numbered_finite_verb_items_stay_prose() {
        for text in [
            "Punkt eins ist erledigt, Punkt zwei ist offen.",
            "Item 1 is done, item 2 is open.",
            "Status: Punkt eins ist erledigt, Punkt zwei ist offen.",
        ] {
            assert_eq!(lists(text), text, "input: {text}");
        }
    }

    #[test]
    fn single_or_out_of_order_markers_stay_prose() {
        let one = "Der nächste Punkt ist wichtig. Nächster Punkt Budget.";
        assert_eq!(lists(one), one);
        let gap = "Punkt 1 Milch, Punkt 3 Brot.";
        assert_eq!(lists(gap), gap);
        let vetoed = "Wir treffen uns um Punkt 1 und gehen um Punkt 2.";
        assert_eq!(lists(vetoed), vetoed);
    }

    #[test]
    fn disabled_family_stays_prose() {
        let disabled = vec![KEY_ORDINAL_DE.to_string()];
        let text = "Erstens Milch, zweitens Eier.";
        assert_eq!(apply_lists(text, &[], &disabled), text);
    }
}
