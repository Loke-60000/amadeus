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
    request_in_flight_ = false;
    reveal_active_ = false;
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
    return Snapshot{
        agent_enabled_,
        voice_enabled_,
        request_in_flight_,
        reveal_active_,
        status_,
        input_,
        subtitle_,
        visible_reply_,
        transcript_,
    };
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

    if ((mods & GLFW_MOD_CONTROL) != 0 && key == GLFW_KEY_V) {
        const char* clipboard = glfwGetClipboardString(window);
        if (clipboard != nullptr) {
            std::lock_guard<std::mutex> lock(mutex_);
            input_.append(clipboard);
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
    }

    for (const std::string& segment : segments) {
        if (!QueueVoiceSegment(segment)) {
            break;
        }
    }
}
