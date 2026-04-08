#pragma once

#include <cstdint>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

class AmadeusTextRenderer;
struct GLFWwindow;

extern "C" void AmadeusOverlayNativeTextDelta(void* user_data, const char* delta);
extern "C" void AmadeusOverlayNativeStreamEvent(void* user_data, int event_kind, const char* message);

extern "C" {
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
// Ollama model list
void amadeus_native_ollama_fetch_models(const char* endpoint);
int  amadeus_native_ollama_fetch_status();
int  amadeus_native_ollama_model_count();
const char* amadeus_native_ollama_model_at(int index);
int  amadeus_native_ollama_model_index(const char* model_name);
// GGUF model download (types 5 = Llama.cpp, 6 = Amadeus built-in)
int  amadeus_native_gguf_model_exists(int type_index);
void amadeus_native_gguf_download_start(int type_index);
int  amadeus_native_gguf_download_status();
int  amadeus_native_gguf_download_progress();
}

enum class AppMode { Chat, SpeechToSpeech };
enum class VoiceLang { Auto, English, Japanese };
enum class VadSensitivity { Low, Medium, High };

class AmadeusOverlay {
public:
    AmadeusOverlay();
    ~AmadeusOverlay() = default;

    void Initialize();
    void Shutdown();
    void HandleKey(GLFWwindow* window, int key, int action, int mods);
    void HandleChar(unsigned int codepoint);
    void Update();
    void Render(const AmadeusTextRenderer& text_renderer, int window_width, int window_height);

private:
    friend void AmadeusOverlayNativeTextDelta(void* user_data, const char* delta);
    friend void AmadeusOverlayNativeStreamEvent(void* user_data, int event_kind, const char* message);

    struct TranscriptEntry {
        std::string speaker;
        std::string text;
    };

    struct NativeStreamContext {
        AmadeusOverlay* overlay = nullptr;
        std::uint64_t generation = 0;
    };

    struct Snapshot {
        bool agent_enabled = false;
        bool llm_loading   = false;  // true while background model preload is in progress
        bool llm_thinking  = false;  // true while model is inside <think>…</think>
        bool voice_enabled = false;
        bool stt_enabled = false;
        bool request_in_flight = false;
        bool reveal_active = false;
        bool settings_open = false;
        int  settings_row = 0;
        int  stt_state = 0;
        int  stt_device_count = 0;
        int  stt_device_index = 0;
        int  mic_gain_step = 4;  // index into gain table; 4 = 0 dB
        int  mic_gate_step = 0;  // 0=Off, 1=Low, 2=Medium, 3=High
        int  mic_comp_step = 0;  // 0=Off, 1=Light, 2=Medium, 3=Heavy
        float stt_mic_level = 0.0f;
        AppMode app_mode = AppMode::Chat;
        VoiceLang voice_lang = VoiceLang::Auto;
        VadSensitivity stt_sensitivity = VadSensitivity::Medium;
        int  provider_count = 0;
        int  provider_index = -1;
        std::string provider_name;
        // provider sub-panel
        bool provider_sub_open = false;
        int  provider_sub_row = 0;
        int  provider_sub_type_idx = 0;
        bool sub_editing = false;
        std::string provider_sub_type_name;
        // text fields (display values)
        std::string sub_field_model;
        std::string sub_field_endpoint;
        std::string sub_field_apikey;
        // the string currently being typed
        std::string sub_edit_buffer;
        // Ollama model list
        int  ollama_fetch_status = 0;  // 0=idle 1=fetching 2=done 3=error
        int  ollama_model_count = 0;
        int  ollama_model_idx = 0;
        std::string ollama_model_name;
        // GGUF model download (types 5 & 6)
        int  gguf_model_exists = 0;    // 1 if model file is present on disk
        int  gguf_download_status = 0; // 0=idle 1=downloading 2=done 3=error
        int  gguf_download_progress = 0; // 0–100
        std::string status;
        std::string input;
        std::string subtitle;
        std::string visible_reply;
        std::string runtime_info;
        std::string stt_device_name;
        std::string stt_partial_text;
        std::vector<TranscriptEntry> transcript;
    };

    static bool IsUtf8ContinuationByte(unsigned char byte);
    static std::size_t NextUtf8Boundary(const std::string& text, std::size_t index, std::size_t steps = 1);
    static void PopUtf8Codepoint(std::string* text);
    static void AppendUtf8Codepoint(std::string* text, unsigned int codepoint);
    static std::string TrimCopy(const std::string& value);
    static std::string SanitizeForDisplay(const std::string& text);
    static std::string TailDisplaySnippet(
        const AmadeusTextRenderer& text_renderer,
        const std::string& raw,
        float max_width);
    static std::string SubtitleSnippet(const std::string& value);
    static double RevealDelaySeconds(const std::string& visible_text);
    static std::vector<std::string> WrapDisplayText(
        const AmadeusTextRenderer& text_renderer,
        const std::string& raw,
        float max_width);
    static bool IsHardSpeechBoundary(std::uint32_t codepoint);
    static bool IsSoftSpeechBoundary(std::uint32_t codepoint);
    static void CollectSpeechSegments(
        const std::string& full_text,
        std::size_t visible_end,
        bool flush_tail,
        std::size_t* speech_cursor,
        std::vector<std::string>* segments);
    void ClearSubtitleBubbleLocked();
    void ScheduleSubtitleHideLocked();

    bool QueueVoiceSegment(const std::string& segment);
    void PushTranscriptLocked(const std::string& speaker, const std::string& text);
    void SubmitPrompt();
    Snapshot CaptureSnapshot();
    void ApplyStreamTextDelta(std::uint64_t generation, const std::string& delta);
    void ApplyToolRound(std::uint64_t generation, const std::string& status);
    void ApplyStreamCompleted(std::uint64_t generation, const std::string& reply);
    void ApplyStreamError(std::uint64_t generation, const std::string& error);
    float DrawWrappedParagraph(
        const AmadeusTextRenderer& text_renderer,
        float x,
        float y,
        float width,
        const std::string& text,
        float red,
        float green,
        float blue,
        float alpha) const;
    void DrawConversationPanel(
        const AmadeusTextRenderer& text_renderer,
        const Snapshot& snapshot,
        int window_width,
        int window_height) const;
    void DrawSubtitleStage(
        const AmadeusTextRenderer& text_renderer,
        const Snapshot& snapshot,
        int window_width,
        int window_height) const;
    void DrawInputBar(
        const AmadeusTextRenderer& text_renderer,
        const Snapshot& snapshot,
        int window_width,
        int window_height) const;
    void DrawSettingsPanel(
        const AmadeusTextRenderer& text_renderer,
        const Snapshot& snapshot,
        int window_width,
        int window_height) const;
    void DrawSettingsButton(
        const AmadeusTextRenderer& text_renderer,
        int window_width) const;
    void DrawSttMicIndicator(
        const AmadeusTextRenderer& text_renderer,
        const Snapshot& snapshot,
        int window_width,
        int window_height) const;

    void ApplySettingsRowChange(int direction);
    void ApplyModeChange(int direction);
    void ApplyLangChange(int direction);
    void ApplySensitivityChange(int direction);

    mutable std::mutex mutex_;
    bool agent_enabled_ = false;
    bool voice_enabled_ = false;
    bool stt_enabled_ = false;
    bool request_in_flight_ = false;
    bool reveal_active_ = false;
    bool settings_open_ = false;
    int  settings_row_ = 0;
    bool provider_sub_open_ = false;
    int  provider_sub_row_ = 0;
    int  provider_sub_type_idx_ = 0;
    bool sub_editing_ = false;        // a text field is active for input
    std::string sub_field_model_;     // saved model name / path
    std::string sub_field_endpoint_;  // saved endpoint URL
    std::string sub_field_apikey_;    // saved API key (plain, masked in display)
    std::string sub_edit_buffer_;     // text being typed in the active field
    int  ollama_model_idx_ = 0;       // selected index in the Ollama model list
    int  stt_device_index_ = 0;
    int  provider_index_ = -1;
    int  mic_gain_step_ = 4;  // 0..8, maps to -12..+12 dB in steps of 3
    int  mic_gate_step_ = 0;  // 0=Off, 1=Low, 2=Medium, 3=High
    int  mic_comp_step_ = 0;  // 0=Off, 1=Light, 2=Medium, 3=Heavy
    AppMode app_mode_ = AppMode::Chat;
    VoiceLang voice_lang_ = VoiceLang::Auto;
    VadSensitivity stt_sensitivity_ = VadSensitivity::Medium;
    std::uint64_t active_generation_ = 0;
    std::string status_;
    std::string input_;
    std::string subtitle_;
    std::string full_reply_;
    std::string visible_reply_;
    std::string runtime_info_;
    std::size_t speech_cursor_ = 0;
    bool reply_pending_commit_ = false;
    double reveal_budget_seconds_ = 0.0;
    double reveal_last_tick_seconds_ = 0.0;
    double subtitle_hide_deadline_seconds_ = 0.0;
    bool subtitle_hide_pending_ = false;
    std::vector<TranscriptEntry> transcript_;
};