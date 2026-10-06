//! Domains, file names, versions and e-mail addresses.
//!
//! **Dot chains.** A spoken `Punkt` / `dot` between word tokens is joined to
//! `.` without spaces when the chain `w (Punkt w)+` ends in a known top-level
//! domain ([`TLDS`]) or file extension ([`EXTENSIONS`]), starts with `www`, or
//! consists of numbers only (`1 Punkt 2` → `1.2`). The recognizer's commas and
//! periods around the spoken dot are dropped, so `Handy, Punkt, Computer.` →
//! `handy.computer.` and `readme. Punkt, md` → `readme.md`. Domains are
//! lowercased as a whole; for file names only the extension is lowercased
//! (`README.md` keeps its stem). A chain starting with a determiner is prose
//! (`the dot com bubble`). Words that double as everyday German/English words
//! (`in`, `at`, `es`, `it`, `go` …) are deliberately absent from the lists.
//! Governed by the `Punkt` / `dot` built-in keys.
//!
//! In a German utterance an English `dot` is no prose word, so `w dot w` joins
//! verbatim even without a TLD/extension (`Discount dot value` →
//! `Discount.value`) when separated by single plain spaces only and the
//! words are no determiner/veto words.
//!
//! **Dashes.** A ticket id — an all-caps acronym (2–6 ASCII letters), a spoken
//! `dash` / `Bindestrich` / `minus` and a number — joins in any language
//! (`PP Dash 106` → `PP-106`). In a German utterance an English `dash` between
//! two words also glues (`voice dash control` → `voice-control`). Governed by
//! the `dash` / `Bindestrich` built-in keys.
//!
//! **E-mail.** `local at|ät domain.tld` → `local@domain.tld` (lowercased) when
//! the domain is dotted (spoken or written) and a positive address signal is
//! present: explicit `ät` / `Klammeraffe`, a spoken local-part joiner, or a
//! recipient cue within three words before the local part. A known first name
//! alone is insufficient; ordinary `online at example.com` stays prose. The
//! local part may itself be a spoken chain (`marc Punkt sachsse at …` →
//! `marc.sachsse@…`). Key: [`KEY_EMAIL_AT`].
//!
//! **Lone token period.** When the whole result is a single path/URL/e-mail
//! token (`~/code/handy.`, `handy.computer.`), the recognizer's sentence period
//! is dropped.

use super::context;
use super::{builtin_active, is_german, lex, TextRule, Token};

pub(crate) const KEY_EMAIL_AT: &str = "at";
const KEY_DOT_DE: &str = "Punkt";
const KEY_DOT_EN: &str = "dot";
const KEY_EMAIL_AT_DE: &str = "Klammeraffe";
const KEY_DASH_DE: &str = "Bindestrich";
const KEY_DASH_EN: &str = "dash";

/// Recipient instructions shared with the symbol substitution pass.
pub(crate) const ADDRESS_VERBS: &[&str] = &[
    "schreib", "schreibe", "mail", "send", "sende", "schick", "cc", "adresse", "address",
];

/// Top-level domains recognized after a spoken dot. Word-like ones
/// (`computer`, `app`, `ai`, `cloud`) carry a small risk when a spoken sentence
/// period is followed by a sentence starting with that word; they are kept for
/// common developer domains (`handy.computer`, `claude.ai`).
const TLDS: &[&str] = &[
    "com", "de", "org", "net", "io", "dev", "app", "ai", "ch", "eu", "uk", "co", "biz", "fm", "gg",
    "xyz", "cloud", "computer", "edu", "gov",
];

/// File extensions recognized after a spoken dot.
const EXTENSIONS: &[&str] = &[
    "md", "rs", "ts", "tsx", "js", "jsx", "mjs", "json", "toml", "yaml", "yml", "txt", "py", "sh",
    "zsh", "html", "css", "scss", "csv", "pdf", "png", "jpg", "jpeg", "gif", "svg", "lock", "log",
    "env", "swift", "kt", "java", "sql", "xml", "zip", "mp3", "mp4", "wav", "docx", "xlsx", "pptx",
    "svelte", "vue", "lua", "rb", "php", "ini", "cfg", "conf",
];

/// Spoken joiners allowed inside an e-mail local part.
fn local_part_joiner(word: &str) -> Option<char> {
    match word.to_lowercase().as_str() {
        "punkt" | "dot" => Some('.'),
        "unterstrich" | "underscore" => Some('_'),
        "bindestrich" | "dash" => Some('-'),
        _ => None,
    }
}

/// Words that are never an e-mail local part ("we met at example.com").
const NOT_LOCAL_PART: &[&str] = &[
    "met", "meet", "work", "works", "worked", "am", "is", "are", "was", "were", "be", "look",
    "looking", "me", "us", "it", "him", "them", "here", "there", "live", "lives", "ich", "bin",
];

/// Apply dot-chain joining, dash joining and e-mail detection to `text`.
/// `language` is the utterance's language code if known.
pub fn apply_links(
    text: &str,
    language: Option<&str>,
    custom: &[TextRule],
    disabled: &[String],
) -> String {
    let german = is_german(language);
    let punkt = builtin_active(KEY_DOT_DE, custom, disabled);
    let dot = builtin_active(KEY_DOT_EN, custom, disabled);
    // Detect addresses before dot joining erases the spoken local-part signal.
    let out = if builtin_active(KEY_EMAIL_AT, custom, disabled) {
        join_emails(
            text,
            punkt,
            dot,
            builtin_active(KEY_EMAIL_AT_DE, custom, disabled),
        )
    } else {
        text.to_string()
    };
    let out = join_dashes(
        &out,
        german,
        builtin_active(KEY_DASH_EN, custom, disabled),
        builtin_active(KEY_DASH_DE, custom, disabled),
    );
    if punkt || dot {
        join_dot_chains(&out, punkt, dot, german && dot)
    } else {
        out
    }
}

/// Join ticket ids (`PP Dash 106` → `PP-106`) and, in a German utterance,
/// `w dash w` → `w-w`. Only single plain spaces may separate the words.
fn join_dashes(text: &str, german: bool, dash: bool, bindestrich: bool) -> String {
    let tokens = lex(text);
    let plain_space = |i: usize| matches!(tokens.get(i), Some(Token::Space(s)) if s == " ");
    let mut out = String::new();
    let mut i = 0;
    while i < tokens.len() {
        out.push_str(tokens[i].text());
        if let (Token::Word(left), Some(Token::Word(joiner)), Some(Token::Word(right))) =
            (&tokens[i], tokens.get(i + 2), tokens.get(i + 4))
        {
            let joiner = joiner.to_lowercase();
            let is_dash = (dash && joiner == "dash")
                || (bindestrich && joiner == "bindestrich")
                || joiner == "minus";
            let ticket = is_dash
                && (2..=6).contains(&left.len())
                && left.chars().all(|c| c.is_ascii_uppercase())
                && right.chars().all(|c| c.is_ascii_digit());
            let german_glue = german
                && dash
                && joiner == "dash"
                && !context::is_veto_word(left)
                && !context::is_veto_word(right);
            if plain_space(i + 1) && plain_space(i + 3) && (ticket || german_glue) {
                // Continue at `right` so chains (`a dash b dash c`) join too.
                out.push('-');
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// With `identifier`, a chain linked only by English `dot` over single plain
/// spaces is valid at any length (German utterance: `dot` is no prose word).
fn join_dot_chains(text: &str, punkt: bool, dot: bool, identifier: bool) -> String {
    let tokens = lex(text);
    let is_dot_word = |token: &Token| match token {
        Token::Word(w) => {
            let lower = w.to_lowercase();
            (punkt && lower == "punkt") || (dot && lower == "dot")
        }
        _ => false,
    };

    let mut out = String::new();
    let mut i = 0;
    while i < tokens.len() {
        let Token::Word(_) = &tokens[i] else {
            out.push_str(tokens[i].text());
            i += 1;
            continue;
        };

        // Collect `w (sep dot sep w)*`, where sep is spaces/ASR punctuation.
        let mut words = vec![i];
        // Per link: an identifier join (English `dot`, plain spaces, no veto).
        let mut plain_dots = Vec::new();
        let mut j = i + 1;
        loop {
            let k = skip_separator(&tokens, j);
            if !tokens.get(k).is_some_and(is_dot_word) {
                break;
            }
            let m = skip_separator(&tokens, k + 1);
            match tokens.get(m) {
                Some(Token::Word(_)) if !is_dot_word(&tokens[m]) => {
                    plain_dots.push(
                        identifier
                            && k == j + 1
                            && m == k + 2
                            && tokens[j].text() == " "
                            && tokens[k + 1].text() == " "
                            && tokens[k].text().eq_ignore_ascii_case("dot")
                            && !context::is_veto_word(tokens[m].text()),
                    );
                    words.push(m);
                    j = m + 1;
                }
                _ => break,
            }
        }

        match longest_valid_chain(&tokens, &words, &plain_dots) {
            Some(n) => {
                out.push_str(&render_chain(&tokens, &words[..n]));
                i = words[n - 1] + 1;
            }
            None => {
                out.push_str(tokens[i].text());
                i += 1;
            }
        }
    }
    out
}

/// Skip whitespace (not line breaks) and `.`/`,` runs.
fn skip_separator(tokens: &[Token], mut i: usize) -> usize {
    loop {
        match tokens.get(i) {
            Some(Token::Space(space)) if !space.contains('\n') => i += 1,
            Some(Token::Other(text)) if text.chars().all(|c| c == '.' || c == ',') => i += 1,
            _ => return i,
        }
    }
}

/// The longest prefix (≥ 2 words) of the chain that forms a domain, file name
/// or version number, or links only identifier dots (`plain_dots`).
fn longest_valid_chain(tokens: &[Token], words: &[usize], plain_dots: &[bool]) -> Option<usize> {
    if words.len() < 2 || context::is_veto_word(tokens[words[0]].text()) {
        return None;
    }
    let is_www = tokens[words[0]].text().eq_ignore_ascii_case("www");
    (2..=words.len()).rev().find(|&n| {
        let chain = &words[..n];
        let last = tokens[chain[n - 1]].text().to_lowercase();
        let numeric = chain
            .iter()
            .all(|&w| tokens[w].text().chars().all(|c| c.is_ascii_digit()));
        numeric
            || TLDS.contains(&last.as_str())
            || EXTENSIONS.contains(&last.as_str())
            || (is_www && n == words.len())
            || plain_dots[..n - 1].iter().all(|&plain| plain)
    })
}

fn render_chain(tokens: &[Token], chain: &[usize]) -> String {
    let parts: Vec<&str> = chain.iter().map(|&w| tokens[w].text()).collect();
    let last = parts[parts.len() - 1].to_lowercase();
    let numeric = parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()));
    if numeric {
        parts.join(".")
    } else if EXTENSIONS.contains(&last.as_str()) {
        let stem = parts[..parts.len() - 1].join(".");
        format!("{}.{}", stem, last)
    } else if TLDS.contains(&last.as_str()) || parts[0].eq_ignore_ascii_case("www") {
        parts.join(".").to_lowercase()
    } else {
        // Identifier in a German utterance (`Discount.value`): join only.
        parts.join(".")
    }
}

/// Join `local at domain.tld` into an e-mail address.
fn join_emails(text: &str, punkt: bool, dot: bool, klammeraffe: bool) -> String {
    let tokens = lex(text);
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        if let Some((local_start, end, address)) = email_at(&tokens, i, punkt, dot, klammeraffe) {
            // Drop the already-emitted local part (tokens local_start..i).
            out.truncate(out.len() - (i - local_start));
            out.push(address);
            for _ in local_start..end - 1 {
                out.push(String::new());
            }
            i = end;
            continue;
        }
        out.push(tokens[i].text().to_string());
        i += 1;
    }
    out.concat()
}

/// If token `i` is an address joiner between a local part and a dotted domain,
/// return (first local-part token, index past the domain, address).
fn email_at(
    tokens: &[Token],
    i: usize,
    punkt: bool,
    dot: bool,
    klammeraffe: bool,
) -> Option<(usize, usize, String)> {
    let Token::Word(word) = &tokens[i] else {
        return None;
    };
    let joiner = word.to_lowercase();
    if !matches!(joiner.as_str(), "at" | "ät" | "klammeraffe") {
        return None;
    }
    if joiner == "klammeraffe" && !klammeraffe {
        return None;
    }

    // Written or spoken dotted domain; leave its final sentence punctuation.
    let mut d = i + 1;
    if !matches!(tokens.get(d), Some(Token::Space(s)) if !s.contains('\n')) {
        return None;
    }
    d += 1;
    let mut labels = Vec::new();
    loop {
        match tokens.get(d) {
            Some(Token::Word(label)) => {
                labels.push(label.as_str());
                d += 1;
            }
            _ => return None,
        }
        match (tokens.get(d), tokens.get(d + 1)) {
            (Some(Token::Other(dot)), Some(Token::Word(_))) if dot == "." => d += 1,
            _ => {
                let spoken = skip_separator(tokens, d);
                let is_dot = tokens.get(spoken).is_some_and(|token| {
                    (punkt && token.text().eq_ignore_ascii_case("punkt"))
                        || (dot && token.text().eq_ignore_ascii_case("dot"))
                });
                let next = skip_separator(tokens, spoken + 1);
                if is_dot && matches!(tokens.get(next), Some(Token::Word(_))) {
                    d = next;
                } else {
                    break;
                }
            }
        }
    }
    let last_label = tokens[d - 1].text();
    let tld_like = (2..=10).contains(&last_label.chars().count())
        && last_label.chars().all(char::is_alphabetic);
    if labels.len() < 2 || !tld_like {
        return None;
    }
    let domain = labels.join(".");

    // Local part: a word, optionally a spoken chain `w (joiner w)*`.
    let context::Before::Word(mut local_start) = context::before(tokens, i) else {
        return None;
    };
    let first_local = tokens[local_start].text().to_lowercase();
    if context::is_veto_word(&first_local) || NOT_LOCAL_PART.contains(&first_local.as_str()) {
        return None;
    }
    let mut local = first_local;
    let mut spoken_local = false;
    loop {
        // Existing dotted/underscored local parts are parsed too, but are not
        // a spoken signal: they still need a cue or an explicit address joiner.
        if local_start >= 2 {
            if let (Token::Word(word), Token::Other(symbol)) =
                (&tokens[local_start - 2], &tokens[local_start - 1])
            {
                if matches!(symbol.as_str(), "." | "_" | "-") {
                    local = format!("{}{}{}", word.to_lowercase(), symbol, local);
                    local_start -= 2;
                    continue;
                }
            }
        }
        let context::Before::Word(joiner) = context::before(tokens, local_start) else {
            break;
        };
        let Some(symbol) = local_part_joiner(tokens[joiner].text()) else {
            break;
        };
        let context::Before::Word(word) = context::before(tokens, joiner) else {
            break;
        };
        local = format!("{}{}{}", tokens[word].text().to_lowercase(), symbol, local);
        local_start = word;
        spoken_local = true;
    }

    if joiner == "at" && !spoken_local && !has_recipient_cue(tokens, local_start) {
        return None;
    }

    Some((
        local_start,
        d,
        format!("{}@{}", local, domain.to_lowercase()),
    ))
}

/// A recipient instruction must be close to the local part, in the same clause.
fn has_recipient_cue(tokens: &[Token], mut start: usize) -> bool {
    for _ in 0..3 {
        let context::Before::Word(prev) = context::before(tokens, start) else {
            return false;
        };
        let word = tokens[prev].text().to_lowercase();
        if ADDRESS_VERBS.contains(&word.as_str()) || matches!(word.as_str(), "an" | "to" | "e-mail")
        {
            return true;
        }
        start = prev;
    }
    false
}

/// Drop the recognizer's sentence period after a lone path/URL/e-mail token.
pub fn strip_lone_token_period(text: &str) -> String {
    let Some(body) = text.strip_suffix('.') else {
        return text.to_string();
    };
    if body.is_empty() || body.ends_with('.') || body.chars().any(char::is_whitespace) {
        return text.to_string();
    }
    let has_path_symbol = body.contains(['/', '~', '@']);
    let ends_like_domain = body.rsplit_once('.').is_some_and(|(stem, last)| {
        !stem.is_empty()
            && (TLDS.contains(&last) || EXTENSIONS.contains(&last.to_lowercase().as_str()))
    });
    if has_path_symbol || ends_like_domain {
        body.to_string()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str) -> String {
        apply_links(text, None, &[], &[])
    }

    fn links_de(text: &str) -> String {
        apply_links(text, Some("de"), &[], &[])
    }

    #[test]
    fn ticket_ids_join_in_any_language() {
        assert_eq!(links("Fix PP Dash 106 today"), "Fix PP-106 today");
        assert_eq!(links_de("der PP Bindestrich 7 Branch"), "der PP-7 Branch");
        assert_eq!(links("see HANDY minus 42"), "see HANDY-42");
        for text in [
            "Pp dash 106",
            "PP dash v2",
            "A dash 1",
            "I made a mad dash for the door.",
        ] {
            assert_eq!(links(text), text, "input: {text}");
        }
        let disabled = vec![KEY_DASH_EN.to_string()];
        assert_eq!(
            apply_links("PP dash 106", None, &[], &disabled),
            "PP dash 106"
        );
    }

    #[test]
    fn english_joiners_in_a_german_utterance() {
        assert_eq!(links_de("Discount dot value wird"), "Discount.value wird");
        assert_eq!(links_de("voice dash control"), "voice-control");
        assert_eq!(
            links_de("Gehe auf Handy, Punkt, Computer."),
            "Gehe auf handy.computer."
        );
        // English utterances keep the TLD/extension requirement.
        assert_eq!(links("Discount dot value"), "Discount dot value");
        assert_eq!(links("voice dash control"), "voice dash control");
        for text in [
            "Das ist der Punkt Hallo",
            "Das war's. Dot. Weiter",
            "Er sagt der dot com Boom",
            "Ein dash der Sache",
        ] {
            assert_eq!(links_de(text), text, "input: {text}");
        }
    }

    #[test]
    fn domain_wrapped_in_asr_commas() {
        assert_eq!(
            links("Gehe auf Handy, Punkt, Computer."),
            "Gehe auf handy.computer."
        );
    }

    #[test]
    fn file_name_wrapped_in_asr_punctuation() {
        assert_eq!(
            links("Öffne die Datei readme. Punkt, md."),
            "Öffne die Datei readme.md."
        );
        assert_eq!(links("Öffne README Punkt MD"), "Öffne README.md");
    }

    #[test]
    fn www_and_multi_label_domains() {
        assert_eq!(links("www Punkt example Punkt com"), "www.example.com");
        assert_eq!(
            links("docs dot google dot com is down"),
            "docs.google.com is down"
        );
    }

    #[test]
    fn version_numbers() {
        assert_eq!(links("Version 1 Punkt 2 Punkt 3"), "Version 1.2.3");
    }

    #[test]
    fn prose_dots_stay_words() {
        for text in [
            "Der Punkt ist, dass wir warten.",
            "Look at the dot com bubble.",
            "Das ist mein Ziel Punkt In Berlin.",
            "Punkt 1 Milch Punkt 2 Eier",
        ] {
            assert_eq!(links(text), text);
        }
    }

    #[test]
    fn email_addresses() {
        assert_eq!(
            links("Schreib an marc at example Punkt com"),
            "Schreib an marc@example.com"
        );
        assert_eq!(
            links("Mail an Marc Punkt Sachsse ät example.de bitte"),
            "Mail an marc.sachsse@example.de bitte"
        );
        assert_eq!(links("We met at example.com"), "We met at example.com");
        assert_eq!(links("I am at home."), "I am at home.");
    }

    #[test]
    fn email_key_disabled() {
        let disabled = vec![KEY_EMAIL_AT.to_string()];
        assert_eq!(
            apply_links("marc at example.com", None, &[], &disabled),
            "marc at example.com"
        );
    }

    #[test]
    fn email_joining_requires_a_positive_address_signal() {
        for text in [
            "Sign up at example.com.",
            "Read more at handy.computer",
            "marc at example.com",
            "John at example.com",
            "Schreib bitte heute noch marc at example.com",
        ] {
            assert_eq!(links(text), text, "input: {text}");
        }
        assert_eq!(
            links("Find us online at example dot com"),
            "Find us online at example.com"
        );
        for (text, expected) in [
            ("marc ät example.com", "marc@example.com"),
            ("marc Klammeraffe example.com", "marc@example.com"),
            (
                "marc dot sachsse at example.com",
                "marc.sachsse@example.com",
            ),
            (
                "marc underscore sachsse at example.com",
                "marc_sachsse@example.com",
            ),
            (
                "marc dash sachsse at example.com",
                "marc-sachsse@example.com",
            ),
            (
                "Mail an marc.sachsse at example.com",
                "Mail an marc.sachsse@example.com",
            ),
            (
                "send please marc at example.com",
                "send please marc@example.com",
            ),
            (
                "Adresse bitte für marc at example.com",
                "Adresse bitte für marc@example.com",
            ),
        ] {
            assert_eq!(links(text), expected, "input: {text}");
        }
    }

    #[test]
    fn lone_token_period() {
        assert_eq!(strip_lone_token_period("~/code/handy."), "~/code/handy");
        assert_eq!(strip_lone_token_period("handy.computer."), "handy.computer");
        assert_eq!(strip_lone_token_period("Hallo."), "Hallo.");
        assert_eq!(strip_lone_token_period("z.B."), "z.B.");
        assert_eq!(
            strip_lone_token_period("Gehe auf handy.computer."),
            "Gehe auf handy.computer."
        );
    }
}
