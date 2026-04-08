#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionResult {
    Japanese,
    NotJapanese,
}

pub fn detect_japanese(text: &str) -> DetectionResult {
    if is_japanese(text) {
        DetectionResult::Japanese
    } else {
        DetectionResult::NotJapanese
    }
}

pub fn is_hiragana(c: char) -> bool {
    ('\u{3040}'..='\u{309F}').contains(&c)
}

pub fn is_katakana(c: char) -> bool {
    ('\u{30A0}'..='\u{30FF}').contains(&c)
}

pub fn is_kanji(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c)
}

pub fn is_japanese(text: &str) -> bool {
    text.chars()
        .any(|c| is_hiragana(c) || is_katakana(c) || is_kanji(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_detection_matches_expected_script_examples() {
        assert_eq!(
            detect_japanese("日本語のテキスト"),
            DetectionResult::Japanese
        );
        assert_eq!(
            detect_japanese("東京は大きな都市です"),
            DetectionResult::Japanese
        );
        assert_eq!(detect_japanese("日本語"), DetectionResult::Japanese);
        assert_eq!(detect_japanese("Hello world"), DetectionResult::NotJapanese);
    }

    #[test]
    fn cjk_ideographs_are_treated_as_japanese_by_current_detector() {
        assert_eq!(detect_japanese("北京"), DetectionResult::Japanese);
        assert_eq!(detect_japanese("你好世界"), DetectionResult::Japanese);
    }
}
