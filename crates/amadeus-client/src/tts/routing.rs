use serde::Deserialize;

use crate::{
    core::error::{AppError, AppResult},
    tts::{
        detection::{DetectionResult, detect_japanese},
        filter::filter_for_tts,
        japanese::{normalize_japanese_for_tts, should_prefer_full_japanese_voice},
    },
};

const MAX_TTS_TEXT_CHARS: usize = 280;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TtsRequest {
    pub text: String,
    #[serde(default)]
    pub speaker: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedTtsRequest {
    pub(crate) spans: Vec<ValidatedTtsSpan>,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedTtsSpan {
    pub(crate) text: String,
    pub(crate) speaker: ChristinaSpeaker,
    pub(crate) language: ChristinaLanguage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpeakerPreference {
    Auto,
    Christina,
    ChristinaJp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LanguagePreference {
    Auto,
    English,
    Japanese,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChristinaSpeaker {
    Christina,
    ChristinaJp,
}

impl ChristinaSpeaker {
    pub(crate) fn token_id(self) -> u32 {
        match self {
            Self::Christina => 3000,
            Self::ChristinaJp => 3001,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChristinaLanguage {
    English,
    Japanese,
}

impl ChristinaLanguage {
    pub(crate) fn token_id(self) -> u32 {
        match self {
            Self::English => 2050,
            Self::Japanese => 2058,
        }
    }
}

pub(crate) fn validate_request(request: TtsRequest) -> AppResult<ValidatedTtsRequest> {
    if request.text.trim().is_empty() {
        return Err(AppError::InvalidTtsRequest {
            reason: "text must not be empty".to_string(),
        });
    }

    let text = filter_for_tts(&request.text).trim().to_string();
    if text.is_empty() {
        return Ok(ValidatedTtsRequest { spans: Vec::new() });
    }

    if text.chars().count() > MAX_TTS_TEXT_CHARS {
        return Err(AppError::InvalidTtsRequest {
            reason: format!("text must be {MAX_TTS_TEXT_CHARS} characters or fewer"),
        });
    }

    let speaker = normalize_speaker_preference(request.speaker.as_deref().unwrap_or("auto"))?;
    let language = normalize_language_preference(request.language.as_deref().unwrap_or("auto"))?;
    let spans = build_tts_spans(&text, speaker, language);
    if spans.is_empty() {
        return Err(AppError::InvalidTtsRequest {
            reason: "text did not contain any synthesizable content".to_string(),
        });
    }

    Ok(ValidatedTtsRequest { spans })
}

fn normalize_speaker_preference(input: &str) -> AppResult<SpeakerPreference> {
    match input.trim().to_ascii_lowercase().as_str() {
        "auto" | "automatic" => Ok(SpeakerPreference::Auto),
        "christina" => Ok(SpeakerPreference::Christina),
        "christina-jp" | "christina_jp" | "christinajp" => Ok(SpeakerPreference::ChristinaJp),
        other => Err(AppError::UnsupportedTtsSpeaker {
            speaker: other.to_string(),
        }),
    }
}

fn normalize_language_preference(input: &str) -> AppResult<LanguagePreference> {
    match input.trim().to_ascii_lowercase().as_str() {
        "auto" | "automatic" => Ok(LanguagePreference::Auto),
        "english" | "en" => Ok(LanguagePreference::English),
        "japanese" | "ja" => Ok(LanguagePreference::Japanese),
        other => Err(AppError::UnsupportedTtsLanguage {
            language: other.to_string(),
        }),
    }
}

fn build_tts_spans(
    text: &str,
    speaker: SpeakerPreference,
    language: LanguagePreference,
) -> Vec<ValidatedTtsSpan> {
    if speaker == SpeakerPreference::Auto && language == LanguagePreference::Auto {
        return detect_auto_tts_spans(text);
    }

    let (speaker, language) = resolve_fixed_profile(speaker, language);
    vec![build_validated_tts_span(
        text.to_string(),
        speaker,
        language,
    )]
}

fn resolve_fixed_profile(
    speaker: SpeakerPreference,
    language: LanguagePreference,
) -> (ChristinaSpeaker, ChristinaLanguage) {
    match (speaker, language) {
        (SpeakerPreference::Christina, LanguagePreference::Japanese) => {
            (ChristinaSpeaker::Christina, ChristinaLanguage::Japanese)
        }
        (SpeakerPreference::ChristinaJp, LanguagePreference::English) => {
            (ChristinaSpeaker::ChristinaJp, ChristinaLanguage::English)
        }
        (SpeakerPreference::ChristinaJp, LanguagePreference::Auto) => {
            (ChristinaSpeaker::ChristinaJp, ChristinaLanguage::Japanese)
        }
        (SpeakerPreference::Auto, LanguagePreference::Japanese) => {
            (ChristinaSpeaker::ChristinaJp, ChristinaLanguage::Japanese)
        }
        (SpeakerPreference::Auto, LanguagePreference::Auto)
        | (SpeakerPreference::Christina, LanguagePreference::Auto)
        | (SpeakerPreference::Christina, LanguagePreference::English)
        | (SpeakerPreference::Auto, LanguagePreference::English) => {
            (ChristinaSpeaker::Christina, ChristinaLanguage::English)
        }
        (SpeakerPreference::ChristinaJp, LanguagePreference::Japanese) => {
            (ChristinaSpeaker::ChristinaJp, ChristinaLanguage::Japanese)
        }
    }
}

fn detect_auto_tts_spans(text: &str) -> Vec<ValidatedTtsSpan> {
    if should_prefer_full_japanese_voice(text) {
        return vec![build_validated_tts_span(
            text.to_string(),
            ChristinaSpeaker::ChristinaJp,
            ChristinaLanguage::Japanese,
        )];
    }

    let mut spans = Vec::new();
    let mut current_text = String::new();
    let mut current_language = None;

    for ch in text.chars() {
        let detected_language = detect_char_language(ch);
        match (current_language, detected_language) {
            (Some(active_language), Some(next_language)) if active_language != next_language => {
                push_tts_span(
                    &mut spans,
                    std::mem::take(&mut current_text),
                    active_language,
                );
                current_language = Some(next_language);
                current_text.push(ch);
            }
            (None, Some(next_language)) => {
                current_language = Some(next_language);
                current_text.push(ch);
            }
            _ => {
                if current_language.is_none() {
                    current_language = Some(ChristinaLanguage::English);
                }
                current_text.push(ch);
            }
        }
    }

    if let Some(language) = current_language {
        push_tts_span(&mut spans, current_text, language);
    }

    spans
}

fn push_tts_span(spans: &mut Vec<ValidatedTtsSpan>, text: String, language: ChristinaLanguage) {
    if text.trim().is_empty() {
        if let Some(previous) = spans.last_mut() {
            previous.text.push_str(&text);
        }
        return;
    }

    let speaker = match language {
        ChristinaLanguage::English => ChristinaSpeaker::Christina,
        ChristinaLanguage::Japanese => ChristinaSpeaker::ChristinaJp,
    };
    let text = normalize_span_text(text, language);

    if let Some(previous) = spans.last_mut()
        && previous.language == language
        && previous.speaker == speaker
    {
        previous.text.push_str(&text);
        return;
    }

    spans.push(ValidatedTtsSpan {
        text,
        speaker,
        language,
    });
}

fn build_validated_tts_span(
    text: String,
    speaker: ChristinaSpeaker,
    language: ChristinaLanguage,
) -> ValidatedTtsSpan {
    ValidatedTtsSpan {
        text: normalize_span_text(text, language),
        speaker,
        language,
    }
}

fn normalize_span_text(text: String, language: ChristinaLanguage) -> String {
    match language {
        ChristinaLanguage::English => text,
        ChristinaLanguage::Japanese => normalize_japanese_for_tts(&text),
    }
}

fn detect_char_language(ch: char) -> Option<ChristinaLanguage> {
    let mut encoded = [0; 4];
    if detect_japanese(ch.encode_utf8(&mut encoded)) == DetectionResult::Japanese {
        Some(ChristinaLanguage::Japanese)
    } else if ch.is_whitespace() || is_neutral_tts_char(ch) {
        None
    } else {
        Some(ChristinaLanguage::English)
    }
}

fn is_neutral_tts_char(ch: char) -> bool {
    ch.is_ascii_punctuation()
        || ch.is_ascii_digit()
        || matches!(
            ch,
            '“' | '”' | '‘' | '’' | '…' | '—' | '–' | '•' | '·' | '«' | '»'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tts_span_auto_detects_japanese_runs() {
        let spans = build_tts_spans(
            "Say こんにちは to Amadeus.",
            SpeakerPreference::Auto,
            LanguagePreference::Auto,
        );

        assert_eq!(spans.len(), 3);

        assert_eq!(spans[0].text, "Say ");
        assert_eq!(spans[0].speaker, ChristinaSpeaker::Christina);
        assert_eq!(spans[0].language, ChristinaLanguage::English);

        assert_eq!(spans[1].text, "コンニチワ ");
        assert_eq!(spans[1].speaker, ChristinaSpeaker::ChristinaJp);
        assert_eq!(spans[1].language, ChristinaLanguage::Japanese);

        assert_eq!(spans[2].text, "to Amadeus.");
        assert_eq!(spans[2].speaker, ChristinaSpeaker::Christina);
        assert_eq!(spans[2].language, ChristinaLanguage::English);
    }

    #[test]
    fn tts_span_respects_explicit_japanese_language() {
        let spans = build_tts_spans(
            "Please say this in Japanese.",
            SpeakerPreference::Auto,
            LanguagePreference::Japanese,
        );

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "Please say this in Japanese.");
        assert_eq!(spans[0].speaker, ChristinaSpeaker::ChristinaJp);
        assert_eq!(spans[0].language, ChristinaLanguage::Japanese);
    }

    #[test]
    fn japanese_dominant_auto_text_stays_on_japanese_voice() {
        let spans = build_tts_spans(
            "今日はAIについて話します。",
            SpeakerPreference::Auto,
            LanguagePreference::Auto,
        );

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].speaker, ChristinaSpeaker::ChristinaJp);
        assert_eq!(spans[0].language, ChristinaLanguage::Japanese);
        assert!(!spans[0].text.contains("今日"));
        assert!(spans[0].text.ends_with('。'));
    }

    #[test]
    fn embedded_ascii_acronyms_stay_on_japanese_voice() {
        let spans = build_tts_spans(
            "今日はAIとGPUについて話します。",
            SpeakerPreference::Auto,
            LanguagePreference::Auto,
        );

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].speaker, ChristinaSpeaker::ChristinaJp);
        assert_eq!(spans[0].language, ChristinaLanguage::Japanese);
        assert!(spans[0].text.contains("AI"));
        assert!(spans[0].text.contains("GPU"));
    }

    #[test]
    fn explicit_japanese_request_converts_kanji_to_kana() {
        let spans = build_tts_spans(
            "東京です。",
            SpeakerPreference::ChristinaJp,
            LanguagePreference::Japanese,
        );

        assert_eq!(spans.len(), 1);
        assert!(!spans[0].text.contains("東京"));
        assert!(spans[0].text.ends_with("デス。"));
    }

    #[test]
    fn validate_request_filters_markdown_before_routing() {
        let request = validate_request(TtsRequest {
            text: "**Hello** [world](https://example.com)".to_string(),
            speaker: None,
            language: None,
        })
        .expect("request should validate");

        assert_eq!(request.spans.len(), 1);
        assert_eq!(request.spans[0].text, "Hello world");
    }

    #[test]
    fn validate_request_turns_markdown_only_chunks_into_noops() {
        let request = validate_request(TtsRequest {
            text: "```rust\nfn main() {}\n```".to_string(),
            speaker: None,
            language: None,
        })
        .expect("request should validate");

        assert!(request.spans.is_empty());
    }
}
