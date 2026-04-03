use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    sync::{Mutex, OnceLock},
};

use lindera::{
    dictionary::load_dictionary, mode::Mode, segmenter::Segmenter, tokenizer::Tokenizer,
};

use crate::tts::detection::{is_japanese, is_kanji};

const IPADIC_READING_INDEX: usize = 7;
const IPADIC_PRONUNCIATION_INDEX: usize = 8;
const ACRONYMISH_ASCII_WORD_MAX_LEN: usize = 4;
const JAPANESE_NORMALIZATION_CACHE_LIMIT: usize = 1024;
const JAPANESE_PRONUNCIATION_OVERRIDES: &[(&str, &str)] = &[
    ("こんにちは", "コンニチワ"),
    ("こんばんは", "コンバンワ"),
    ("牧瀬紅莉栖", "マキセクリス"),
    ("紅莉栖", "クリス"),
    ("牧瀬", "マキセ"),
];

struct JapaneseNormalizationCache {
    entries: HashMap<String, String>,
    order: VecDeque<String>,
}

impl JapaneseNormalizationCache {
    fn get(&mut self, text: &str) -> Option<String> {
        let value = self.entries.get(text)?.clone();
        self.touch(text);
        Some(value)
    }

    fn insert(&mut self, text: String, normalized: String) {
        if self.entries.insert(text.clone(), normalized).is_some() {
            self.touch(&text);
            return;
        }

        self.order.push_back(text);
        while self.entries.len() > JAPANESE_NORMALIZATION_CACHE_LIMIT {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    fn touch(&mut self, text: &str) {
        if let Some(index) = self.order.iter().position(|existing| existing == text) {
            self.order.remove(index);
        }
        self.order.push_back(text.to_string());
    }
}

pub(crate) fn japanese_char_count(text: &str) -> usize {
    text.chars()
        .filter(|ch| is_japanese(&ch.to_string()))
        .count()
}

#[cfg(test)]
pub(crate) fn english_word_count(text: &str) -> usize {
    let mut count = 0usize;
    let mut current_len = 0usize;

    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            current_len += 1;
        } else {
            if current_len >= 2 {
                count += 1;
            }
            current_len = 0;
        }
    }

    if current_len >= 2 {
        count += 1;
    }

    count
}

pub(crate) fn should_prefer_full_japanese_voice(text: &str) -> bool {
    let japanese_chars = japanese_char_count(text);
    japanese_chars > 0 && english_words_requiring_voice_switch(text) <= 1
}

fn english_words_requiring_voice_switch(text: &str) -> usize {
    let mut count = 0usize;
    let mut current_len = 0usize;
    let mut current_is_uppercase = true;

    let mut flush_word = |len: usize, is_uppercase: bool| {
        if len >= 2 && !(is_uppercase && len <= ACRONYMISH_ASCII_WORD_MAX_LEN) {
            count += 1;
        }
    };

    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            current_len += 1;
            current_is_uppercase &= ch.is_ascii_uppercase();
        } else {
            flush_word(current_len, current_is_uppercase);
            current_len = 0;
            current_is_uppercase = true;
        }
    }

    flush_word(current_len, current_is_uppercase);
    count
}

pub(crate) fn normalize_japanese_for_tts(text: &str) -> String {
    if !is_japanese(text) {
        return text.to_string();
    }

    if let Some(cached) = japanese_normalization_cache()
        .lock()
        .ok()
        .and_then(|mut cache| cache.get(text))
    {
        return cached;
    }

    let normalized = normalize_japanese_for_tts_uncached(text);
    if let Ok(mut cache) = japanese_normalization_cache().lock() {
        cache.insert(text.to_string(), normalized.clone());
    }

    normalized
}

pub(crate) fn preload_japanese_tts_support() {
    let _ = japanese_normalization_cache();
    let _ = normalize_japanese_for_tts("こんにちは。今日はAIについて話します。");
}

pub(crate) fn should_prebuffer_mixed_japanese_stream(text: &str) -> bool {
    is_japanese(text) && !should_prefer_full_japanese_voice(text)
}

fn normalize_japanese_for_tts_uncached(text: &str) -> String {
    if !is_japanese(text) {
        return text.to_string();
    }

    let text = apply_pronunciation_overrides(text);
    let text = text.as_ref();

    if !contains_kanji(text) {
        return normalize_kana_without_tokenizer(text);
    }

    let Some(tokenizer) = tokenizer() else {
        return text.to_string();
    };

    let mut tokens = match tokenizer.tokenize(text) {
        Ok(tokens) => tokens,
        Err(_) => return text.to_string(),
    };

    let mut output = String::with_capacity(text.len() * 2);
    let mut cursor = 0usize;

    for token in &mut tokens {
        if cursor < token.byte_start {
            output.push_str(&text[cursor..token.byte_start]);
        }

        if let Some(reading) = reading_for_token(token) {
            output.push_str(&reading);
        } else {
            output.push_str(token.surface.as_ref());
        }
        cursor = token.byte_end;
    }

    if cursor < text.len() {
        output.push_str(&text[cursor..]);
    }

    output
}

fn contains_kanji(text: &str) -> bool {
    text.chars().any(is_kanji)
}

fn normalize_kana_without_tokenizer(text: &str) -> String {
    text.chars().map(hiragana_to_katakana).collect()
}

fn hiragana_to_katakana(ch: char) -> char {
    match ch {
        '\u{3041}'..='\u{3096}' | '\u{309D}'..='\u{309F}' => {
            char::from_u32(ch as u32 + 0x60).unwrap_or(ch)
        }
        _ => ch,
    }
}

fn japanese_normalization_cache() -> &'static Mutex<JapaneseNormalizationCache> {
    static CACHE: OnceLock<Mutex<JapaneseNormalizationCache>> = OnceLock::new();

    CACHE.get_or_init(|| {
        Mutex::new(JapaneseNormalizationCache {
            entries: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

fn apply_pronunciation_overrides<'a>(text: &'a str) -> Cow<'a, str> {
    let mut overridden = None::<String>;

    for (surface, pronunciation) in JAPANESE_PRONUNCIATION_OVERRIDES {
        let target = overridden.as_deref().unwrap_or(text);
        if !target.contains(surface) {
            continue;
        }

        let owned = overridden.get_or_insert_with(|| text.to_string());
        *owned = owned.replace(surface, pronunciation);
    }

    overridden.map(Cow::Owned).unwrap_or(Cow::Borrowed(text))
}

fn tokenizer() -> Option<&'static Tokenizer> {
    static TOKENIZER: OnceLock<Option<Tokenizer>> = OnceLock::new();

    TOKENIZER
        .get_or_init(|| {
            let dictionary = load_dictionary("embedded://ipadic").ok()?;
            let segmenter = Segmenter::new(Mode::Normal, dictionary, None);
            Some(Tokenizer::new(segmenter))
        })
        .as_ref()
}

fn reading_for_token(token: &mut lindera::token::Token<'_>) -> Option<String> {
    let pronunciation = token.get_detail(IPADIC_PRONUNCIATION_INDEX)?;
    if pronunciation != "*" && pronunciation != "UNK" {
        return Some(pronunciation.to_string());
    }

    let reading = token.get_detail(IPADIC_READING_INDEX)?;
    if reading == "*" || reading == "UNK" {
        None
    } else {
        Some(reading.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        english_word_count, normalize_japanese_for_tts, preload_japanese_tts_support,
        should_prebuffer_mixed_japanese_stream, should_prefer_full_japanese_voice,
    };

    #[test]
    fn kana_conversion_uses_dictionary_pronunciations_for_tts() {
        let converted = normalize_japanese_for_tts("東京は大きな都市です。");

        assert!(!converted.contains("東京"));
        assert!(!converted.contains("都市"));
        assert!(converted.ends_with('。'));
    }

    #[test]
    fn greeting_particles_use_spoken_pronunciation() {
        assert_eq!(normalize_japanese_for_tts("こんにちは。"), "コンニチワ。");
        assert_eq!(normalize_japanese_for_tts("こんばんは。"), "コンバンワ。");
    }

    #[test]
    fn kana_only_text_skips_dictionary_lookup() {
        assert_eq!(normalize_japanese_for_tts("おはようございます。"), "オハヨウゴザイマス。");
        assert_eq!(normalize_japanese_for_tts("アマデウスです！"), "アマデウスデス！");
    }

    #[test]
    fn proper_name_overrides_fix_makise_kurisu() {
        let converted = normalize_japanese_for_tts("牧瀬紅莉栖・アマデウスです！");

        assert!(converted.contains("マキセクリス・アマデウス"));
        assert!(!converted.contains("紅莉栖"));
        assert!(!converted.contains("牧瀬"));
    }

    #[test]
    fn given_name_override_fix_kurisu() {
        assert_eq!(normalize_japanese_for_tts("紅莉栖です。"), "クリスデス。");
    }

    #[test]
    fn japanese_dominant_text_prefers_single_japanese_voice() {
        assert!(should_prefer_full_japanese_voice(
            "今日はAIについて話します。"
        ));
        assert!(should_prefer_full_japanese_voice(
            "今日はAIとGPUについて話します。"
        ));
        assert!(!should_prefer_full_japanese_voice(
            "今日はdeep learning modelについて話します。"
        ));
        assert!(!should_prefer_full_japanese_voice(
            "Say こんにちは to Amadeus."
        ));
    }

    #[test]
    fn english_word_counter_ignores_short_ascii_fragments() {
        assert_eq!(english_word_count("AIとGPUの話"), 2);
        assert_eq!(english_word_count("a と b"), 0);
    }

    #[test]
    fn japanese_stream_prebuffer_only_applies_to_real_voice_switches() {
        assert!(!should_prebuffer_mixed_japanese_stream(
            "今日はAIとGPUについて話します。"
        ));
        assert!(should_prebuffer_mixed_japanese_stream(
            "今日はdeep learning modelについて話します。"
        ));
        assert!(should_prebuffer_mixed_japanese_stream(
            "Hello こんにちは hello こんばんは"
        ));
    }

    #[test]
    fn japanese_tts_preload_warms_normalization_support() {
        preload_japanese_tts_support();
        assert_eq!(normalize_japanese_for_tts("こんにちは。"), "コンニチワ。");
    }
}
