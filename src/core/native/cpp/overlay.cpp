#include <GLFW/glfw3.h>

#include "overlay.hpp"

#include <algorithm>
#include <cctype>
#include <cstdlib>
#include <fstream>
#include <string>
#include <thread>
#include <vector>

#include "font_renderer.hpp"

extern "C"
{
const char* amadeus_native_bridge_status_message();
int amadeus_native_agent_available();
int amadeus_native_voice_available();
int amadeus_native_stt_available();
int amadeus_native_stt_state();
int amadeus_native_stt_start();
int amadeus_native_stt_stop();
void amadeus_native_stt_set_sensitivity(int level);
int amadeus_native_stt_device_count();
const char* amadeus_native_stt_device_name(int index);
void amadeus_native_stt_select_device(int index);
float amadeus_native_stt_mic_level();
int amadeus_native_stt_active_device_index();
const char* amadeus_native_stt_partial_text();
void amadeus_native_set_mic_gain_db(float db);
void amadeus_native_set_mic_gate(float threshold);
void amadeus_native_set_mic_compressor(float threshold_db, float ratio);
void amadeus_native_voice_set_language(int lang);
const char* amadeus_native_agent_runtime_info();
char* amadeus_native_agent_turn(const char* prompt);
int amadeus_native_agent_turn_stream(
    const char* prompt,
    void* user_data,
    void (*on_text_delta)(void* user_data, const char* delta),
    void (*on_event)(void* user_data, int event_kind, const char* message));
void amadeus_native_free_string(char* value);
void amadeus_native_voice_clear();
int amadeus_native_voice_enqueue(const char* text);
const char* amadeus_native_backend_last_error_message();
int amadeus_native_providers_count();
const char* amadeus_native_providers_name_at(int index);
int amadeus_native_providers_active_index();
void amadeus_native_providers_select(int index);
int amadeus_native_provider_type_count();
const char* amadeus_native_provider_type_name(int index);
int amadeus_native_provider_active_type_index();
const char* amadeus_native_provider_current_model();
const char* amadeus_native_provider_current_endpoint();
const char* amadeus_native_provider_current_apikey();
const char* amadeus_native_provider_current_model_path();
void amadeus_native_provider_set_config(
    int type_index,
    const char* model,
    const char* endpoint,
    const char* api_key);
void amadeus_native_ollama_fetch_models(const char* endpoint);
int  amadeus_native_ollama_fetch_status();
int  amadeus_native_ollama_model_count();
const char* amadeus_native_ollama_model_at(int index);
int  amadeus_native_ollama_model_index(const char* model_name);
// GGUF model download (types 5 & 6)
int  amadeus_native_gguf_model_exists(int type_index);
void amadeus_native_gguf_download_start(int type_index);
int  amadeus_native_gguf_download_status();
int  amadeus_native_gguf_download_progress();
// STT model download
int  amadeus_native_stt_model_exists();
void amadeus_native_stt_download_start();
int  amadeus_native_stt_download_status();
int  amadeus_native_stt_download_progress();
// TTS model cache
int  amadeus_native_tts_model_cached();
// Local LLM preload status: 1 while background loading, 0 when ready
int  amadeus_native_llm_loading();
// 1 while the model is inside a <think>…</think> block
int  amadeus_native_llm_thinking();
}

namespace {

constexpr double kRevealIntervalSeconds = 0.028;
constexpr double kRevealWhitespaceIntervalSeconds = 0.016;
constexpr double kRevealSoftPauseSeconds = 0.140;
constexpr double kRevealSentencePauseSeconds = 0.260;
constexpr double kRevealLinePauseSeconds = 0.320;
constexpr double kRevealCompletedCatchUpMultiplier = 2.75;
constexpr double kSubtitleStageIdleHideSeconds = 8.0;
constexpr std::size_t kSubtitleCharLimit = 40;
constexpr std::size_t kTranscriptLimit = 3;
constexpr std::size_t kSpeechSoftThreshold = 96;
constexpr std::size_t kSpeechHardThreshold = 220;
constexpr int kNativeStreamEventToolRound = 1;
constexpr int kNativeStreamEventCompleted = 2;
constexpr int kNativeStreamEventError = 3;
constexpr const char* kNativeLogFileEnv = "AMADEUS_NATIVE_LOG_FILE";

std::string ReadBridgeMessage(const char* value) {
    return value == nullptr ? std::string() : std::string(value);
}

void AppendTranscriptToLogFile(const std::string& speaker, const std::string& text) {
    const char* log_file = std::getenv(kNativeLogFileEnv);
    if (log_file == nullptr || log_file[0] == '\0') {
        return;
    }

    std::ofstream file(log_file, std::ios::app);
    if (!file.is_open()) {
        return;
    }

    file << "\n[" << speaker << "]\n" << text << "\n";
}

bool DecodeUtf8Codepoint(const std::string& text, std::size_t* index, std::uint32_t* codepoint) {
    if (index == nullptr || codepoint == nullptr || *index >= text.size()) {
        return false;
    }

    const unsigned char first = static_cast<unsigned char>(text[*index]);
    if ((first & 0x80u) == 0u) {
        *codepoint = first;
        ++(*index);
        return true;
    }

    if ((first & 0xE0u) == 0xC0u && *index + 1 < text.size()) {
        *codepoint = ((first & 0x1Fu) << 6)
            | (static_cast<unsigned char>(text[*index + 1]) & 0x3Fu);
        *index += 2;
        return true;
    }

    if ((first & 0xF0u) == 0xE0u && *index + 2 < text.size()) {
        *codepoint = ((first & 0x0Fu) << 12)
            | ((static_cast<unsigned char>(text[*index + 1]) & 0x3Fu) << 6)
            | (static_cast<unsigned char>(text[*index + 2]) & 0x3Fu);
        *index += 3;
        return true;
    }

    if ((first & 0xF8u) == 0xF0u && *index + 3 < text.size()) {
        *codepoint = ((first & 0x07u) << 18)
            | ((static_cast<unsigned char>(text[*index + 1]) & 0x3Fu) << 12)
            | ((static_cast<unsigned char>(text[*index + 2]) & 0x3Fu) << 6)
            | (static_cast<unsigned char>(text[*index + 3]) & 0x3Fu);
        *index += 4;
        return true;
    }

    ++(*index);
    return false;
}

std::uint32_t LastUtf8Codepoint(const std::string& text) {
    if (text.empty()) {
        return 0;
    }

    std::size_t index = text.size();
    while (index > 0) {
        --index;
        if ((static_cast<unsigned char>(text[index]) & 0xC0u) != 0x80u) {
            break;
        }
    }

    std::size_t decode_index = index;
    std::uint32_t codepoint = 0;
    if (DecodeUtf8Codepoint(text, &decode_index, &codepoint)) {
        return codepoint;
    }

    return static_cast<unsigned char>(text.back());
}

bool IsDisplayWhitespace(std::uint32_t codepoint) {
    return codepoint == ' '
        || codepoint == '\t'
        || codepoint == '\n'
        || codepoint == '\r'
        || codepoint == 0x3000u;
}

std::size_t CountUtf8Codepoints(const std::string& text) {
    std::size_t count = 0;
    std::size_t index = 0;
    std::uint32_t codepoint = 0;
    while (index < text.size()) {
        const std::size_t start = index;
        if (!DecodeUtf8Codepoint(text, &index, &codepoint)) {
            if (index == start) {
                ++index;
            }
        }
        ++count;
    }

    return count;
}

std::size_t ByteOffsetForUtf8Codepoints(const std::string& text, std::size_t codepoint_count) {
    std::size_t index = 0;
    std::uint32_t codepoint = 0;
    while (codepoint_count > 0 && index < text.size()) {
        const std::size_t start = index;
        if (!DecodeUtf8Codepoint(text, &index, &codepoint) && index == start) {
            ++index;
        }
        --codepoint_count;
    }

    return index;
}

std::size_t ByteOffsetForUtf8Tail(const std::string& text, std::size_t tail_codepoints) {
    const std::size_t total = CountUtf8Codepoints(text);
    if (tail_codepoints >= total) {
        return 0;
    }

    return ByteOffsetForUtf8Codepoints(text, total - tail_codepoints);
}

std::size_t PrefixByteOffsetForWidth(
    const AmadeusTextRenderer& text_renderer,
    const std::string& text,
    float max_width) {
    if (max_width <= 0.0f) {
        return 0;
    }

    float width = 0.0f;
    std::size_t index = 0;
    std::uint32_t codepoint = 0;
    while (index < text.size()) {
        const std::size_t start = index;
        if (!DecodeUtf8Codepoint(text, &index, &codepoint)) {
            if (index == start) {
                ++index;
            }
            continue;
        }

        const float advance = static_cast<float>(std::max(1, text_renderer.CodepointAdvance(codepoint)));
        if (width + advance > max_width) {
            return start == 0 ? index : start;
        }
        width += advance;
    }

    return text.size();
}

}  // namespace

extern "C" void AmadeusOverlayNativeTextDelta(void* user_data, const char* delta) {
    auto* context = static_cast<AmadeusOverlay::NativeStreamContext*>(user_data);
    if (context == nullptr || context->overlay == nullptr) {
        return;
    }

    context->overlay->ApplyStreamTextDelta(context->generation, ReadBridgeMessage(delta));
}

extern "C" void AmadeusOverlayNativeStreamEvent(void* user_data, int event_kind, const char* message) {
    auto* context = static_cast<AmadeusOverlay::NativeStreamContext*>(user_data);
    if (context == nullptr || context->overlay == nullptr) {
        return;
    }

    const std::string text = ReadBridgeMessage(message);
    switch (event_kind) {
    case kNativeStreamEventToolRound:
        context->overlay->ApplyToolRound(context->generation, text);
        break;
    case kNativeStreamEventCompleted:
        context->overlay->ApplyStreamCompleted(context->generation, text);
        break;
    case kNativeStreamEventError:
    default:
        context->overlay->ApplyStreamError(context->generation, text);
        break;
    }
}

AmadeusOverlay::AmadeusOverlay() = default;

void AmadeusOverlay::Initialize() {
    std::lock_guard<std::mutex> lock(mutex_);
    agent_enabled_ = amadeus_native_agent_available() != 0;
    voice_enabled_ = amadeus_native_voice_available() != 0;
    stt_enabled_ = amadeus_native_stt_available() != 0;
    runtime_info_ = ReadBridgeMessage(amadeus_native_agent_runtime_info());
    request_in_flight_ = false;
    reveal_active_ = false;
    settings_open_ = false;
    settings_row_ = 0;
    provider_sub_open_ = false;
    provider_sub_row_ = 0;
    provider_sub_type_idx_ = amadeus_native_provider_active_type_index();
    sub_editing_ = false;
    stt_device_index_ = 0;
    provider_index_ = amadeus_native_providers_active_index();
    app_mode_ = AppMode::Chat;
    voice_lang_ = VoiceLang::Auto;
    stt_sensitivity_ = VadSensitivity::Medium;
    active_generation_ = 0;
    status_ = ReadBridgeMessage(amadeus_native_bridge_status_message());
    if (status_.empty()) {
        status_ = "Native renderer is ready.";
    }
    input_.clear();
    subtitle_ = agent_enabled_
        ? "Type below and press Enter."
        : "Configure the agent runtime to enable native chat.";
    full_reply_.clear();
    visible_reply_.clear();
    speech_cursor_ = 0;
    reply_pending_commit_ = false;
    reveal_budget_seconds_ = 0.0;
    reveal_last_tick_seconds_ = 0.0;
    subtitle_hide_deadline_seconds_ = 0.0;
    subtitle_hide_pending_ = false;
    transcript_.clear();
}

void AmadeusOverlay::Shutdown() {
    std::lock_guard<std::mutex> lock(mutex_);
    ++active_generation_;
    request_in_flight_ = false;
    reveal_active_ = false;
    full_reply_.clear();
    visible_reply_.clear();
    reply_pending_commit_ = false;
    reveal_budget_seconds_ = 0.0;
    reveal_last_tick_seconds_ = 0.0;
    subtitle_hide_deadline_seconds_ = 0.0;
    subtitle_hide_pending_ = false;
    amadeus_native_voice_clear();
}

bool AmadeusOverlay::IsUtf8ContinuationByte(unsigned char byte) {
    return (byte & 0xC0u) == 0x80u;
}

std::size_t AmadeusOverlay::NextUtf8Boundary(const std::string& text, std::size_t index, std::size_t steps) {
    while (steps > 0 && index < text.size()) {
        ++index;
        while (index < text.size() && IsUtf8ContinuationByte(static_cast<unsigned char>(text[index]))) {
            ++index;
        }
        --steps;
    }
    return index;
}

void AmadeusOverlay::PopUtf8Codepoint(std::string* text) {
    if (text == nullptr || text->empty()) {
        return;
    }

    std::size_t index = text->size() - 1;
    while (index > 0 && IsUtf8ContinuationByte(static_cast<unsigned char>((*text)[index]))) {
        --index;
    }
    text->erase(index);
}

void AmadeusOverlay::AppendUtf8Codepoint(std::string* text, unsigned int codepoint) {
    if (text == nullptr || codepoint < 32u) {
        return;
    }

    if (codepoint <= 0x7Fu) {
        text->push_back(static_cast<char>(codepoint));
    } else if (codepoint <= 0x7FFu) {
        text->push_back(static_cast<char>(0xC0u | ((codepoint >> 6) & 0x1Fu)));
        text->push_back(static_cast<char>(0x80u | (codepoint & 0x3Fu)));
    } else if (codepoint <= 0xFFFFu) {
        text->push_back(static_cast<char>(0xE0u | ((codepoint >> 12) & 0x0Fu)));
        text->push_back(static_cast<char>(0x80u | ((codepoint >> 6) & 0x3Fu)));
        text->push_back(static_cast<char>(0x80u | (codepoint & 0x3Fu)));
    } else if (codepoint <= 0x10FFFFu) {
        text->push_back(static_cast<char>(0xF0u | ((codepoint >> 18) & 0x07u)));
        text->push_back(static_cast<char>(0x80u | ((codepoint >> 12) & 0x3Fu)));
        text->push_back(static_cast<char>(0x80u | ((codepoint >> 6) & 0x3Fu)));
        text->push_back(static_cast<char>(0x80u | (codepoint & 0x3Fu)));
    }
}

std::string AmadeusOverlay::TrimCopy(const std::string& value) {
    const std::size_t start = value.find_first_not_of(" \t\r\n");
    if (start == std::string::npos) {
        return std::string();
    }

    const std::size_t end = value.find_last_not_of(" \t\r\n");
    return value.substr(start, end - start + 1);
}

std::string AmadeusOverlay::SanitizeForDisplay(const std::string& text) {
    std::string output;
    output.reserve(text.size());

    std::size_t index = 0;
    std::uint32_t codepoint = 0;
    while (index < text.size()) {
        const std::size_t start = index;
        if (!DecodeUtf8Codepoint(text, &index, &codepoint)) {
            output.push_back('?');
            continue;
        }

        if (codepoint == '\t') {
            output.push_back(' ');
            continue;
        }

        if (codepoint == '\n' || codepoint == '\r' || codepoint >= 32u) {
            output.append(text, start, index - start);
            continue;
        }
    }

    return output;
}

std::string AmadeusOverlay::TailDisplaySnippet(
    const AmadeusTextRenderer& text_renderer,
    const std::string& raw,
    float max_width) {
    const std::string sanitized = SanitizeForDisplay(raw);
    if (text_renderer.MeasureTextWidth(sanitized) <= max_width) {
        return sanitized;
    }

    const std::string prefix = "... ";
    const float prefix_width = static_cast<float>(text_renderer.MeasureTextWidth(prefix));
    std::string tail = sanitized;
    while (!tail.empty()
        && prefix_width + static_cast<float>(text_renderer.MeasureTextWidth(tail)) > max_width) {
        const std::size_t next = ByteOffsetForUtf8Codepoints(tail, 1);
        if (next == 0 || next > tail.size()) {
            break;
        }
        tail.erase(0, next);
    }

    return tail.empty() ? prefix : prefix + tail;
}

std::string AmadeusOverlay::SubtitleSnippet(const std::string& value) {
    const std::string trimmed = TrimCopy(SanitizeForDisplay(value));
    if (CountUtf8Codepoints(trimmed) <= kSubtitleCharLimit) {
        return trimmed;
    }

    std::size_t start = ByteOffsetForUtf8Tail(trimmed, kSubtitleCharLimit);
    std::size_t scan = start;
    std::uint32_t codepoint = 0;
    while (scan < trimmed.size()) {
        const std::size_t boundary = scan;
        if (!DecodeUtf8Codepoint(trimmed, &scan, &codepoint)) {
            if (scan == boundary) {
                ++scan;
            }
            continue;
        }

        if (IsDisplayWhitespace(codepoint)) {
            start = scan;
            break;
        }
    }

    return std::string("... ") + TrimCopy(trimmed.substr(start));
}

double AmadeusOverlay::RevealDelaySeconds(const std::string& visible_text) {
    if (visible_text.empty()) {
        return 0.0;
    }

    switch (LastUtf8Codepoint(visible_text)) {
    case '\n':
    case '\r':
        return kRevealLinePauseSeconds;
    case '.':
    case '!':
    case '?':
    case 0x3002u:
    case 0xFF01u:
    case 0xFF1Fu:
        return kRevealSentencePauseSeconds;
    case ',':
    case ';':
    case ':':
    case 0x3001u:
    case 0xFF0Cu:
    case 0xFF1Bu:
    case 0xFF1Au:
        return kRevealSoftPauseSeconds;
    case ' ':
    case '\t':
        return kRevealWhitespaceIntervalSeconds;
    default:
        return kRevealIntervalSeconds;
    }
}

std::vector<std::string> AmadeusOverlay::WrapDisplayText(
    const AmadeusTextRenderer& text_renderer,
    const std::string& raw,
    float max_width) {
    std::vector<std::string> lines;
    if (max_width <= 0.0f) {
        return lines;
    }

    const std::string sanitized = SanitizeForDisplay(raw);
    std::string current;
    std::string word;
    float current_width = 0.0f;
    float word_width = 0.0f;
    const float space_width = static_cast<float>(std::max(1, text_renderer.CodepointAdvance(' ')));

    const auto flush_current = [&]() {
        lines.push_back(current.empty() ? std::string(" ") : current);
        current.clear();
        current_width = 0.0f;
    };

    const auto append_word = [&]() {
        if (word.empty()) {
            return;
        }

        while (word_width > max_width) {
            if (!current.empty()) {
                flush_current();
            }
            const std::size_t split = PrefixByteOffsetForWidth(text_renderer, word, max_width);
            if (split == 0 || split > word.size()) {
                break;
            }
            lines.push_back(word.substr(0, split));
            word.erase(0, split);
            word_width = static_cast<float>(text_renderer.MeasureTextWidth(word));
        }

        if (current.empty()) {
            current = word;
            current_width = word_width;
        } else if (current_width + space_width + word_width <= max_width) {
            current.push_back(' ');
            current.append(word);
            current_width += space_width + word_width;
        } else {
            flush_current();
            current = word;
            current_width = word_width;
        }

        word.clear();
        word_width = 0.0f;
    };

    std::size_t index = 0;
    std::uint32_t codepoint = 0;
    while (index < sanitized.size()) {
        const std::size_t start = index;
        if (!DecodeUtf8Codepoint(sanitized, &index, &codepoint)) {
            word.push_back('?');
            word_width += static_cast<float>(text_renderer.char_width());
            continue;
        }

        if (codepoint == '\n' || codepoint == '\r') {
            append_word();
            flush_current();
            continue;
        }
        if (IsDisplayWhitespace(codepoint)) {
            append_word();
            continue;
        }
        word.append(sanitized, start, index - start);
        word_width += static_cast<float>(std::max(1, text_renderer.CodepointAdvance(codepoint)));
    }

    append_word();
    if (!current.empty()) {
        lines.push_back(current);
    }
    if (lines.empty()) {
        lines.push_back(" ");
    }
    return lines;
}

bool AmadeusOverlay::IsHardSpeechBoundary(std::uint32_t codepoint) {
    switch (codepoint) {
    case '.':
    case '!':
    case '?':
    case '\n':
    case 0x3002u:
    case 0xFF01u:
    case 0xFF1Fu:
        return true;
    default:
        return false;
    }
}

bool AmadeusOverlay::IsSoftSpeechBoundary(std::uint32_t codepoint) {
    switch (codepoint) {
    case ',':
    case ';':
    case ':':
    case ')':
    case ']':
    case '}':
    case 0x3001u:
    case 0xFF0Cu:
    case 0xFF1Bu:
    case 0xFF1Au:
        return true;
    default:
        return false;
    }
}

void AmadeusOverlay::CollectSpeechSegments(
    const std::string& full_text,
    std::size_t visible_end,
    bool flush_tail,
    std::size_t* speech_cursor,
    std::vector<std::string>* segments) {
    if (speech_cursor == nullptr || segments == nullptr) {
        return;
    }

    std::size_t scan = *speech_cursor;
    std::size_t char_count = 0;
    while (scan < visible_end) {
        const std::size_t next = NextUtf8Boundary(full_text, scan, 1);
        std::size_t decode_index = scan;
        std::uint32_t codepoint = 0;
        const bool decoded = DecodeUtf8Codepoint(full_text, &decode_index, &codepoint);
        bool boundary = false;

        if (decoded) {
            if (IsHardSpeechBoundary(codepoint)) {
                boundary = true;
            } else if (IsSoftSpeechBoundary(codepoint) && char_count >= kSpeechSoftThreshold) {
                boundary = true;
            } else if (codepoint <= 0x7Fu
                && std::isspace(static_cast<unsigned char>(codepoint))
                && char_count >= kSpeechHardThreshold) {
                boundary = true;
            }
        }

        ++char_count;
        if (char_count >= kSpeechHardThreshold) {
            boundary = true;
        }

        if (boundary) {
            const std::string segment = TrimCopy(full_text.substr(*speech_cursor, next - *speech_cursor));
            if (!segment.empty()) {
                segments->push_back(segment);
            }
            *speech_cursor = next;
            scan = next;
            char_count = 0;
            continue;
        }

        scan = next;
    }

    if (flush_tail && *speech_cursor < visible_end) {
        const std::string tail = TrimCopy(full_text.substr(*speech_cursor, visible_end - *speech_cursor));
        if (!tail.empty()) {
            segments->push_back(tail);
        }
        *speech_cursor = visible_end;
    }
}

void AmadeusOverlay::PushTranscriptLocked(const std::string& speaker, const std::string& text) {
    const std::string trimmed = TrimCopy(text);
    if (trimmed.empty()) {
        return;
    }

    transcript_.push_back({speaker, trimmed});
    if (transcript_.size() > kTranscriptLimit) {
        transcript_.erase(transcript_.begin(), transcript_.begin() + (transcript_.size() - kTranscriptLimit));
    }

    AppendTranscriptToLogFile(speaker, trimmed);
}

void AmadeusOverlay::ClearSubtitleBubbleLocked() {
    subtitle_.clear();
    full_reply_.clear();
    visible_reply_.clear();
    speech_cursor_ = 0;
    subtitle_hide_deadline_seconds_ = 0.0;
    subtitle_hide_pending_ = false;
}

void AmadeusOverlay::ScheduleSubtitleHideLocked() {
    subtitle_hide_pending_ = true;
}

bool AmadeusOverlay::QueueVoiceSegment(const std::string& segment) {
    if (segment.empty()) {
        return true;
    }

    if (amadeus_native_voice_enqueue(segment.c_str()) != 0) {
        return true;
    }

    const std::string backend_error = ReadBridgeMessage(amadeus_native_backend_last_error_message());
    std::lock_guard<std::mutex> lock(mutex_);
    voice_enabled_ = false;
    status_ = backend_error.empty()
        ? "Voice playback is unavailable. Text replies will continue without audio."
        : "Voice playback stopped: " + backend_error;
    return false;
}

void AmadeusOverlay::SubmitPrompt() {
    std::string prompt;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        // Re-query live: the agent may have been configured after Initialize() ran.
        agent_enabled_ = amadeus_native_agent_available() != 0;
        if (!agent_enabled_) {
            status_ = "The native agent is unavailable. Configure .amadeus/config.json first.";
            subtitle_ = "Agent unavailable.";
            return;
        }
        prompt = TrimCopy(input_);
    }

    if (prompt.empty()) {
        return;
    }

    amadeus_native_voice_clear();

    std::uint64_t generation = 0;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        ++active_generation_;
        generation = active_generation_;
        request_in_flight_ = true;
        reveal_active_ = false;
        ClearSubtitleBubbleLocked();
        reply_pending_commit_ = false;
        reveal_budget_seconds_ = 0.0;
        reveal_last_tick_seconds_ = 0.0;
        subtitle_ = "Thinking...";
        status_ = "Running the streamed agent turn...";
        PushTranscriptLocked("You", prompt);
        input_.clear();
    }

    std::thread([this, prompt, generation]() {
        NativeStreamContext context{this, generation};
        amadeus_native_agent_turn_stream(
            prompt.c_str(),
            &context,
            &AmadeusOverlayNativeTextDelta,
            &AmadeusOverlayNativeStreamEvent);
    }).detach();
}

AmadeusOverlay::Snapshot AmadeusOverlay::CaptureSnapshot() {
    std::lock_guard<std::mutex> lock(mutex_);
    Snapshot snap;
    snap.agent_enabled    = amadeus_native_agent_available() != 0;
    snap.llm_loading      = amadeus_native_llm_loading() != 0;
    snap.llm_thinking     = amadeus_native_llm_thinking() != 0;
    // Re-poll every frame: STT and voice finish loading after Initialize() is called.
    voice_enabled_        = amadeus_native_voice_available() != 0;
    stt_enabled_          = amadeus_native_stt_available() != 0;
    snap.voice_enabled    = voice_enabled_;
    snap.stt_enabled      = stt_enabled_;
    snap.request_in_flight = request_in_flight_;
    snap.reveal_active    = reveal_active_;
    snap.settings_open    = settings_open_;
    snap.settings_row     = settings_row_;
    snap.stt_state        = amadeus_native_stt_state();
    snap.stt_device_count = amadeus_native_stt_device_count();
    // Sync displayed device index with what the worker actually has open.
    // If a switch failed, the worker's index stays at the last working device.
    {
        const int active = amadeus_native_stt_active_device_index();
        if (active >= 0 && active != stt_device_index_) {
            stt_device_index_ = active;
        }
    }
    snap.stt_device_index = stt_device_index_;
    snap.stt_mic_level    = amadeus_native_stt_mic_level();
    snap.stt_partial_text = ReadBridgeMessage(amadeus_native_stt_partial_text());
    if (snap.stt_device_count > 0) {
        const char* name = amadeus_native_stt_device_name(stt_device_index_);
        snap.stt_device_name = name ? name : "";
    }
    snap.mic_gain_step    = mic_gain_step_;
    snap.mic_gate_step    = mic_gate_step_;
    snap.mic_comp_step    = mic_comp_step_;
    snap.app_mode         = app_mode_;
    snap.voice_lang       = voice_lang_;
    snap.stt_sensitivity  = stt_sensitivity_;
    snap.provider_count   = amadeus_native_providers_count();
    snap.provider_index   = provider_index_;
    if (provider_index_ >= 0) {
        const char* name = amadeus_native_providers_name_at(provider_index_);
        snap.provider_name = name ? name : "";
    } else if (snap.provider_count > 0) {
        snap.provider_name = "Custom";
    }
    // provider sub-panel
    snap.provider_sub_open      = provider_sub_open_;
    snap.provider_sub_row       = provider_sub_row_;
    snap.provider_sub_type_idx  = provider_sub_type_idx_;
    snap.sub_editing            = sub_editing_;
    {
        const char* tn = amadeus_native_provider_type_name(provider_sub_type_idx_);
        snap.provider_sub_type_name = tn ? tn : "";
    }
    snap.sub_field_model      = sub_field_model_;
    snap.sub_field_endpoint   = sub_field_endpoint_;
    snap.sub_field_apikey     = sub_field_apikey_;
    snap.sub_edit_buffer      = sub_edit_buffer_;
    snap.ollama_fetch_status  = amadeus_native_ollama_fetch_status();
    snap.ollama_model_count   = amadeus_native_ollama_model_count();
    snap.ollama_model_idx     = ollama_model_idx_;
    {
        const char* mn = amadeus_native_ollama_model_at(ollama_model_idx_);
        snap.ollama_model_name = mn ? mn : "";
    }
    // GGUF download state (queried live — no lock needed, atomics)
    snap.gguf_model_exists    = amadeus_native_gguf_model_exists(provider_sub_type_idx_);
    snap.gguf_download_status = amadeus_native_gguf_download_status();
    snap.gguf_download_progress = amadeus_native_gguf_download_progress();
    snap.status           = status_;
    snap.input            = input_;
    snap.subtitle         = subtitle_;
    snap.visible_reply    = visible_reply_;
    snap.runtime_info     = runtime_info_;
    snap.transcript       = transcript_;
    return snap;
}

void AmadeusOverlay::ApplyStreamTextDelta(std::uint64_t generation, const std::string& delta) {
    if (delta.empty()) {
        return;
    }

    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (generation != active_generation_) {
            return;
        }
        const bool was_caught_up = visible_reply_.size() >= full_reply_.size();
        request_in_flight_ = true;
        reveal_active_ = true;
        full_reply_.append(delta);
        if (was_caught_up) {
            reveal_budget_seconds_ = 0.0;
            reveal_last_tick_seconds_ = 0.0;
        }
        status_ = voice_enabled_ ? "Streaming reply and voice..." : "Streaming reply...";
    }
}

void AmadeusOverlay::ApplyToolRound(std::uint64_t generation, const std::string& status) {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (generation != active_generation_) {
            return;
        }
        request_in_flight_ = true;
        reveal_active_ = false;
        ClearSubtitleBubbleLocked();
        reply_pending_commit_ = false;
        reveal_budget_seconds_ = 0.0;
        reveal_last_tick_seconds_ = 0.0;
        subtitle_ = "Working...";
        status_ = status.empty() ? "Running tools..." : status;
    }

    amadeus_native_voice_clear();
}

void AmadeusOverlay::ApplyStreamCompleted(std::uint64_t generation, const std::string& reply) {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (generation != active_generation_) {
            return;
        }

        request_in_flight_ = false;
        if (!reply.empty() && reply != full_reply_) {
            full_reply_ = reply;
        }
        if (visible_reply_.size() < full_reply_.size()) {
            reveal_active_ = true;
            reply_pending_commit_ = true;
            status_ = voice_enabled_ ? "Speaking reply..." : "Finishing reply...";
        } else {
            reveal_active_ = false;
            visible_reply_ = full_reply_;
            subtitle_ = SubtitleSnippet(full_reply_);
            status_ = "Ready for the next prompt.";
            ScheduleSubtitleHideLocked();
            reply_pending_commit_ = false;
            PushTranscriptLocked("Amadeus", full_reply_);
        }
    }
}

void AmadeusOverlay::ApplyStreamError(std::uint64_t generation, const std::string& error) {
    std::lock_guard<std::mutex> lock(mutex_);
    if (generation != active_generation_) {
        return;
    }

    request_in_flight_ = false;
    reveal_active_ = false;
    full_reply_.clear();
    visible_reply_.clear();
    speech_cursor_ = 0;
    reply_pending_commit_ = false;
    reveal_budget_seconds_ = 0.0;
    reveal_last_tick_seconds_ = 0.0;
    subtitle_ = SubtitleSnippet(error);
    subtitle_hide_deadline_seconds_ = 0.0;
    ScheduleSubtitleHideLocked();
    status_ = "The agent turn failed.";
    PushTranscriptLocked("System", error.empty() ? "The native agent turn failed." : error);
}

void AmadeusOverlay::HandleKey(GLFWwindow* window, int key, int action, int mods) {
    if (action != GLFW_PRESS && action != GLFW_REPEAT) {
        return;
    }

    // Tab toggles the settings panel
    if (key == GLFW_KEY_TAB && action == GLFW_PRESS) {
        std::lock_guard<std::mutex> lock(mutex_);
        settings_open_ = !settings_open_;
        settings_row_ = 0;
        return;
    }

    // Settings navigation while panel is open
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (settings_open_) {

            // Provider sub-panel intercepts all navigation when open.
            if (provider_sub_open_) {
                // helper: max navigable row for the current provider type
                // 0=Anthropic  1=OpenAI  2=Gemini  3=OpenAI-compat
                // 4=Ollama     5=Llama.cpp          6=Amadeus
                const auto sub_max_row = [&]() -> int {
                    switch (provider_sub_type_idx_) {
                    case 3:  return 4; // type, model, endpoint, api_key, save
                    case 4:  return 3; // type, endpoint, model (cycle), save
                    case 5:  return 3; // type, model_path, download, save
                    case 6:  return 2; // type, download, save
                    default: return 3; // type, model, api_key, save
                    }
                };

                // helper: returns a pointer to the editable text field for the current row.
                // Returns nullptr if the row is not a text field (cycle rows, Amadeus).
                const auto active_field = [&]() -> std::string* {
                    switch (provider_sub_type_idx_) {
                    case 0: case 1: case 2:          // Anthropic/OpenAI/Gemini
                        if (provider_sub_row_ == 1) return &sub_field_model_;
                        if (provider_sub_row_ == 2) return &sub_field_apikey_;
                        break;
                    case 3:                           // OpenAI-compatible
                        if (provider_sub_row_ == 1) return &sub_field_model_;
                        if (provider_sub_row_ == 2) return &sub_field_endpoint_;
                        if (provider_sub_row_ == 3) return &sub_field_apikey_;
                        break;
                    case 4:                           // Ollama: endpoint is text, model is cycle
                        if (provider_sub_row_ == 1) return &sub_field_endpoint_;
                        break;
                    case 5:                           // Llama.cpp
                        if (provider_sub_row_ == 1) return &sub_field_model_;
                        break;
                    default: break;
                    }
                    return nullptr;
                };

                // helper: apply the current sub-panel state to config.json
                const auto apply_config = [&]() {
                    const char* endpoint = "";
                    if (provider_sub_type_idx_ == 3 || provider_sub_type_idx_ == 4) {
                        endpoint = sub_field_endpoint_.c_str();
                    }
                    // For Ollama, the selected model comes from the fetched list.
                    std::string ollama_sel;
                    if (provider_sub_type_idx_ == 4) {
                        const char* mn = amadeus_native_ollama_model_at(ollama_model_idx_);
                        ollama_sel = mn ? mn : "";
                    }
                    const std::string& model = (provider_sub_type_idx_ == 4)
                        ? ollama_sel : sub_field_model_;
                    amadeus_native_provider_set_config(
                        provider_sub_type_idx_,
                        model.c_str(),
                        endpoint,
                        sub_field_apikey_.c_str());
                };

                // helper: reload field values from config for the active provider type
                const auto reload_fields = [&]() {
                    if (provider_sub_type_idx_ == 5 || provider_sub_type_idx_ == 6) {
                        const char* mp = amadeus_native_provider_current_model_path();
                        sub_field_model_ = mp ? mp : "";
                    } else {
                        const char* m = amadeus_native_provider_current_model();
                        sub_field_model_ = m ? m : "";
                    }
                    const char* ep = amadeus_native_provider_current_endpoint();
                    sub_field_endpoint_ = ep ? ep : "";
                    const char* ak = amadeus_native_provider_current_apikey();
                    sub_field_apikey_ = ak ? ak : "";
                };

                if (sub_editing_) {
                    // Text edit mode: only backspace and enter are handled here;
                    // printable chars come via HandleChar.
                    switch (key) {
                    case GLFW_KEY_ESCAPE:
                        // Cancel edit: restore from saved field value
                        if (std::string* field = active_field()) {
                            sub_edit_buffer_ = *field;
                        }
                        sub_editing_ = false;
                        return;
                    case GLFW_KEY_ENTER:
                    case GLFW_KEY_KP_ENTER: {
                        // Confirm: save buffer into field (no config write yet — use Save row)
                        if (std::string* field = active_field()) {
                            *field = sub_edit_buffer_;
                        }
                        sub_editing_ = false;
                        // When Ollama endpoint is confirmed, auto-fetch models
                        if (provider_sub_type_idx_ == 4 && provider_sub_row_ == 1) {
                            const std::string& ep = sub_field_endpoint_.empty()
                                ? std::string("http://127.0.0.1:11434")
                                : sub_field_endpoint_;
                            amadeus_native_ollama_fetch_models(ep.c_str());
                            ollama_model_idx_ = 0;
                        }
                        return;
                    }
                    case GLFW_KEY_BACKSPACE:
                        PopUtf8Codepoint(&sub_edit_buffer_);
                        return;
                    default:
                        return;
                    }
                }

                // Navigation mode (not editing)
                switch (key) {
                case GLFW_KEY_ESCAPE:
                    provider_sub_open_ = false;
                    return;
                case GLFW_KEY_UP:
                    if (provider_sub_row_ > 0) {
                        --provider_sub_row_;
                    }
                    return;
                case GLFW_KEY_DOWN:
                    if (provider_sub_row_ < sub_max_row()) {
                        ++provider_sub_row_;
                    }
                    return;
                case GLFW_KEY_LEFT:
                case GLFW_KEY_RIGHT: {
                    const int dir = (key == GLFW_KEY_RIGHT) ? 1 : -1;
                    if (provider_sub_row_ == 0) {
                        // Cycle provider type
                        const int total = amadeus_native_provider_type_count();
                        if (total > 0) {
                            provider_sub_type_idx_ =
                                (provider_sub_type_idx_ + dir + total) % total;
                            provider_sub_row_ = 0;
                            ollama_model_idx_ = 0;
                            // Load saved values for the new type from config
                            reload_fields();
                            // Trigger model fetch when switching to Ollama
                            if (provider_sub_type_idx_ == 4) {
                                const std::string& ep = sub_field_endpoint_.empty()
                                    ? std::string("http://127.0.0.1:11434")
                                    : sub_field_endpoint_;
                                amadeus_native_ollama_fetch_models(ep.c_str());
                                // Restore previously-selected model index from config
                                const char* cur = amadeus_native_provider_current_model();
                                if (cur && *cur) {
                                    const int idx = amadeus_native_ollama_model_index(cur);
                                    if (idx >= 0) ollama_model_idx_ = idx;
                                }
                            }
                        }
                    } else if (provider_sub_type_idx_ == 4 && provider_sub_row_ == 2) {
                        // Ollama: cycle through fetched model list (no config write until Save)
                        const int count = amadeus_native_ollama_model_count();
                        if (count > 0) {
                            ollama_model_idx_ = (ollama_model_idx_ + dir + count) % count;
                        }
                    }
                    // Left/Right on other text fields does nothing special (Enter to edit)
                    return;
                }
                case GLFW_KEY_ENTER:
                case GLFW_KEY_KP_ENTER: {
                    if (provider_sub_row_ == sub_max_row()) {
                        // Save row: write all staged changes to config and close
                        apply_config();
                        provider_sub_open_ = false;
                    } else if (
                        (provider_sub_type_idx_ == 5 && provider_sub_row_ == 2) ||
                        (provider_sub_type_idx_ == 6 && provider_sub_row_ == 1)) {
                        // Download row: kick off GGUF download if not already running
                        const int status = amadeus_native_gguf_download_status();
                        if (status != 1) {
                            amadeus_native_gguf_download_start(provider_sub_type_idx_);
                        }
                    } else if (provider_sub_row_ > 0) {
                        // Text field: start editing
                        if (std::string* field = active_field()) {
                            sub_edit_buffer_ = *field;
                            sub_editing_ = true;
                        }
                    }
                    return;
                }
                default:
                    return;
                }
            }

            switch (key) {
            case GLFW_KEY_ESCAPE:
                settings_open_ = false;
                return;
            case GLFW_KEY_UP:
                if (settings_row_ > 0) {
                    --settings_row_;
                }
                return;
            case GLFW_KEY_DOWN: {
                // Rows: 0=Mode, 1=Voice Language, 2=Provider
                //       [3=Sensitivity, 4=Device, 5=Gain, 6=Gate, 7=Compressor] (stt only)
                const int max_row = stt_enabled_ ? 7 : 2;
                if (settings_row_ < max_row) {
                    ++settings_row_;
                }
                return;
            }
            case GLFW_KEY_LEFT:
            case GLFW_KEY_RIGHT: {
                const int dir = (key == GLFW_KEY_RIGHT) ? 1 : -1;
                switch (settings_row_) {
                case 0: {
                    // Mode
                    const int m = static_cast<int>(app_mode_) + dir;
                    app_mode_ = static_cast<AppMode>(std::max(0, std::min(1, m)));
                    if (app_mode_ == AppMode::SpeechToSpeech && stt_enabled_) {
                        amadeus_native_stt_start();
                    } else {
                        amadeus_native_stt_stop();
                    }
                    break;
                }
                case 1: {
                    // Voice Language
                    const int l = static_cast<int>(voice_lang_) + dir;
                    voice_lang_ = static_cast<VoiceLang>(std::max(0, std::min(2, l)));
                    amadeus_native_voice_set_language(static_cast<int>(voice_lang_));
                    break;
                }
                case 2: {
                    // Open provider sub-panel; load current config values.
                    provider_sub_type_idx_ = amadeus_native_provider_active_type_index();
                    provider_sub_row_ = 0;
                    sub_editing_ = false;
                    ollama_model_idx_ = 0;
                    if (provider_sub_type_idx_ == 5 || provider_sub_type_idx_ == 6) {
                        const char* mp = amadeus_native_provider_current_model_path();
                        sub_field_model_ = mp ? mp : "";
                    } else {
                        const char* m = amadeus_native_provider_current_model();
                        sub_field_model_ = m ? m : "";
                    }
                    const char* ep = amadeus_native_provider_current_endpoint();
                    sub_field_endpoint_ = ep ? ep : "";
                    const char* ak = amadeus_native_provider_current_apikey();
                    sub_field_apikey_ = ak ? ak : "";
                    // If opening on Ollama, fetch models immediately
                    if (provider_sub_type_idx_ == 4) {
                        const std::string base = sub_field_endpoint_.empty()
                            ? "http://127.0.0.1:11434" : sub_field_endpoint_;
                        amadeus_native_ollama_fetch_models(base.c_str());
                        const char* cur = amadeus_native_provider_current_model();
                        if (cur && *cur) {
                            const int idx = amadeus_native_ollama_model_index(cur);
                            if (idx >= 0) ollama_model_idx_ = idx;
                        }
                    }
                    provider_sub_open_ = true;
                    break;
                }
                case 3: {
                    // Microphone Sensitivity (only when STT available)
                    const int s = static_cast<int>(stt_sensitivity_) + dir;
                    stt_sensitivity_ = static_cast<VadSensitivity>(std::max(0, std::min(2, s)));
                    amadeus_native_stt_set_sensitivity(static_cast<int>(stt_sensitivity_));
                    break;
                }
                case 4: {
                    // Mic Device
                    const int count = amadeus_native_stt_device_count();
                    if (count > 0) {
                        const int d = stt_device_index_ + dir;
                        stt_device_index_ = std::max(0, std::min(count - 1, d));
                        amadeus_native_stt_select_device(stt_device_index_);
                    }
                    break;
                }
                case 5: {
                    // Mic Gain: 9 steps, -12..+12 dB in 3 dB increments (index 4 = 0 dB)
                    mic_gain_step_ = std::max(0, std::min(8, mic_gain_step_ + dir));
                    amadeus_native_set_mic_gain_db(static_cast<float>((mic_gain_step_ - 4) * 3));
                    break;
                }
                case 6: {
                    // Noise Gate: Off / Low / Medium / High
                    mic_gate_step_ = std::max(0, std::min(3, mic_gate_step_ + dir));
                    const float gate_thresholds[] = { 0.0f, 0.005f, 0.010f, 0.020f };
                    amadeus_native_set_mic_gate(gate_thresholds[mic_gate_step_]);
                    break;
                }
                case 7: {
                    // Compressor: Off / Light / Medium / Heavy
                    mic_comp_step_ = std::max(0, std::min(3, mic_comp_step_ + dir));
                    // {threshold_db, ratio}
                    const float comp_threshold[] = { -30.0f, -24.0f, -30.0f, -36.0f };
                    const float comp_ratio[]     = {   1.0f,   3.0f,   5.0f,   8.0f };
                    amadeus_native_set_mic_compressor(
                        comp_threshold[mic_comp_step_],
                        comp_ratio[mic_comp_step_]);
                    break;
                }
                default:
                    break;
                }
                return;
            }
            default:
                break;
            }
        }
    }

    if ((mods & GLFW_MOD_CONTROL) != 0 && key == GLFW_KEY_V) {
        const char* clipboard = glfwGetClipboardString(window);
        if (clipboard != nullptr) {
            std::lock_guard<std::mutex> lock(mutex_);
            if (provider_sub_open_ && sub_editing_) {
                sub_edit_buffer_.append(clipboard);
            } else {
                input_.append(clipboard);
            }
        }
        return;
    }

    switch (key) {
    case GLFW_KEY_BACKSPACE: {
        std::lock_guard<std::mutex> lock(mutex_);
        PopUtf8Codepoint(&input_);
        break;
    }
    case GLFW_KEY_ENTER:
    case GLFW_KEY_KP_ENTER:
        SubmitPrompt();
        break;
    case GLFW_KEY_ESCAPE: {
        std::lock_guard<std::mutex> lock(mutex_);
        ++active_generation_;
        request_in_flight_ = false;
        reveal_active_ = false;
        ClearSubtitleBubbleLocked();
        reply_pending_commit_ = false;
        reveal_budget_seconds_ = 0.0;
        reveal_last_tick_seconds_ = 0.0;
        status_ = "Current reply stopped.";
        amadeus_native_voice_clear();
        break;
    }
    default:
        break;
    }
}

void AmadeusOverlay::HandleChar(unsigned int codepoint) {
    if (codepoint < 32u) {
        return;
    }

    std::lock_guard<std::mutex> lock(mutex_);

    // While a provider text field is active, route input there.
    if (provider_sub_open_ && sub_editing_) {
        if (sub_edit_buffer_.size() < 1024) {
            AppendUtf8Codepoint(&sub_edit_buffer_, codepoint);
        }
        return;
    }

    if (input_.size() >= 2048) {
        return;
    }
    AppendUtf8Codepoint(&input_, codepoint);
}

void AmadeusOverlay::Update() {
    std::vector<std::string> segments;

    {
        std::lock_guard<std::mutex> lock(mutex_);
        const double now = glfwGetTime();
        if (subtitle_hide_pending_ && (!subtitle_.empty() || !visible_reply_.empty())) {
            subtitle_hide_deadline_seconds_ = now + kSubtitleStageIdleHideSeconds;
            subtitle_hide_pending_ = false;
        }

        if (!reveal_active_) {
            reveal_budget_seconds_ = 0.0;
            reveal_last_tick_seconds_ = 0.0;
        } else {
            if (reveal_last_tick_seconds_ <= 0.0) {
                reveal_last_tick_seconds_ = now;
            } else {
                double elapsed = std::max(0.0, now - reveal_last_tick_seconds_);
                if (!request_in_flight_) {
                    elapsed *= kRevealCompletedCatchUpMultiplier;
                }
                reveal_budget_seconds_ += elapsed;
                reveal_last_tick_seconds_ = now;
            }

            bool advanced = false;
            while (visible_reply_.size() < full_reply_.size()) {
                const double delay = RevealDelaySeconds(visible_reply_);
                if (delay > 0.0 && reveal_budget_seconds_ + 1e-9 < delay) {
                    break;
                }

                reveal_budget_seconds_ = std::max(0.0, reveal_budget_seconds_ - delay);
                const std::size_t next = NextUtf8Boundary(full_reply_, visible_reply_.size(), 1);
                visible_reply_.append(full_reply_, visible_reply_.size(), next - visible_reply_.size());
                advanced = true;
            }

            if (advanced) {
                subtitle_ = SubtitleSnippet(visible_reply_);
                subtitle_hide_deadline_seconds_ = now + kSubtitleStageIdleHideSeconds;
                subtitle_hide_pending_ = false;
                const bool flush_tail = !request_in_flight_ && visible_reply_.size() >= full_reply_.size();
                CollectSpeechSegments(full_reply_, visible_reply_.size(), flush_tail, &speech_cursor_, &segments);
            }

            if (!request_in_flight_ && visible_reply_.size() >= full_reply_.size()) {
                reveal_active_ = false;
                reveal_budget_seconds_ = 0.0;
                reveal_last_tick_seconds_ = 0.0;
                subtitle_ = SubtitleSnippet(full_reply_);
                status_ = "Ready for the next prompt.";
                subtitle_hide_deadline_seconds_ = now + kSubtitleStageIdleHideSeconds;
                subtitle_hide_pending_ = false;
                if (reply_pending_commit_) {
                    reply_pending_commit_ = false;
                    PushTranscriptLocked("Amadeus", full_reply_);
                }
            }
        }

        if (!request_in_flight_
            && !reveal_active_
            && subtitle_hide_deadline_seconds_ > 0.0
            && now >= subtitle_hide_deadline_seconds_) {
            ClearSubtitleBubbleLocked();
        }

        // Keep status line updated while the built-in model is downloading.
        if (!request_in_flight_ && !reveal_active_) {
            const int dl = amadeus_native_gguf_download_status();
            if (dl == 1) {
                const int pct = amadeus_native_gguf_download_progress();
                status_ = "Downloading Amadeus model... " + std::to_string(pct) + "%";
            } else if (dl == 2 && status_.rfind("Downloading", 0) == 0) {
                status_ = "Amadeus model ready. Type a message to start.";
            } else if (dl == 3 && status_.rfind("Downloading", 0) == 0) {
                status_ = "Model download failed. Open Settings to retry.";
            }
        }
    }

    for (const std::string& segment : segments) {
        if (!QueueVoiceSegment(segment)) {
            break;
        }
    }
}
