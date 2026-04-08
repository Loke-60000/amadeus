#include <GL/glew.h>
#include <GLFW/glfw3.h>

#include "overlay.hpp"
#include "font_renderer.hpp"

#include <algorithm>
#include <string>
#include <vector>

namespace {

constexpr std::size_t kSubtitleStageMaxLines = 5;
constexpr float kUiOuterMargin = 24.0f;
constexpr float kUiPanelPadding = 24.0f;
constexpr float kUiSectionGap = 16.0f;
constexpr float kConversationPanelMaxWidth = 820.0f;
constexpr float kConversationPanelMinWidth = 540.0f;
constexpr float kConversationPanelHeight = 680.0f;
constexpr float kSubtitleStageMaxWidth = 1280.0f;
constexpr float kSubtitleStageMinWidth = 760.0f;
constexpr float kSubtitleStageBottomGap = 28.0f;
constexpr float kSubtitleStageMinTop = 180.0f;
constexpr float kInputBarMaxWidth = 1280.0f;
constexpr float kInputBarMinWidth = 840.0f;
constexpr float kInputBarHeight = 120.0f;

void DrawFilledRect(float x, float y, float width, float height, float red, float green, float blue, float alpha) {
    glColor4f(red, green, blue, alpha);
    glBegin(GL_QUADS);
    glVertex2f(x, y);
    glVertex2f(x + width, y);
    glVertex2f(x + width, y + height);
    glVertex2f(x, y + height);
    glEnd();
}

void BeginOverlay(int window_width, int window_height) {
    glUseProgram(0);
    glBindBuffer(GL_ARRAY_BUFFER, 0);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
    glPushAttrib(GL_COLOR_BUFFER_BIT | GL_CURRENT_BIT | GL_ENABLE_BIT | GL_TEXTURE_BIT | GL_TRANSFORM_BIT);
    glMatrixMode(GL_PROJECTION);
    glPushMatrix();
    glLoadIdentity();
    glOrtho(0.0, static_cast<double>(window_width), static_cast<double>(window_height), 0.0, -1.0, 1.0);
    glMatrixMode(GL_MODELVIEW);
    glPushMatrix();
    glLoadIdentity();
    glDisable(GL_DEPTH_TEST);
    glDisable(GL_CULL_FACE);
    glDisable(GL_SCISSOR_TEST);
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
}

void EndOverlay() {
    glMatrixMode(GL_MODELVIEW);
    glPopMatrix();
    glMatrixMode(GL_PROJECTION);
    glPopMatrix();
    glPopAttrib();
}


}  // namespace

float AmadeusOverlay::DrawWrappedParagraph(
    const AmadeusTextRenderer& text_renderer,
    float x,
    float y,
    float width,
    const std::string& text,
    float red,
    float green,
    float blue,
    float alpha) const {
    const std::vector<std::string> lines = WrapDisplayText(text_renderer, text, width);
    for (const std::string& line : lines) {
        text_renderer.DrawText(x, y, line, red, green, blue, alpha);
        y += static_cast<float>(text_renderer.line_height());
    }
    return y;
}

void AmadeusOverlay::DrawConversationPanel(
    const AmadeusTextRenderer& text_renderer,
    const Snapshot& snapshot,
    int window_width,
    int window_height) const {
    const float x = kUiOuterMargin;
    const float y = kUiOuterMargin;
    const float width = std::min(kConversationPanelMaxWidth, std::max(kConversationPanelMinWidth, window_width * 0.48f));
    const float height = std::min(window_height - 380.0f, kConversationPanelHeight);

    DrawFilledRect(x, y, width, height, 0.02f, 0.03f, 0.04f, 0.84f);

    float cursor_y = y + kUiPanelPadding;
    text_renderer.DrawText(x + kUiPanelPadding, cursor_y, "AMADEUS", 0.95f, 0.97f, 0.99f, 1.0f);
    cursor_y += static_cast<float>(text_renderer.line_height()) + kUiSectionGap;
    cursor_y = DrawWrappedParagraph(text_renderer, x + kUiPanelPadding, cursor_y, width - (kUiPanelPadding * 2.0f), snapshot.status, 0.82f, 0.87f, 0.91f, 1.0f);
    cursor_y += kUiSectionGap;

    const std::string hint = snapshot.voice_enabled
        ? "Enter sends. Ctrl+V pastes. Esc stops."
        : "Enter sends. Ctrl+V pastes. Esc stops. Voice is off.";
    cursor_y = DrawWrappedParagraph(text_renderer, x + kUiPanelPadding, cursor_y, width - (kUiPanelPadding * 2.0f), hint, 0.55f, 0.67f, 0.72f, 1.0f);
    cursor_y += kUiSectionGap;

    for (const TranscriptEntry& entry : snapshot.transcript) {
        text_renderer.DrawText(x + kUiPanelPadding, cursor_y, entry.speaker, 0.96f, 0.77f, 0.58f, 1.0f);
        cursor_y += static_cast<float>(text_renderer.line_height());
        cursor_y = DrawWrappedParagraph(text_renderer, x + kUiPanelPadding, cursor_y, width - (kUiPanelPadding * 2.0f), entry.text, 0.93f, 0.95f, 0.98f, 1.0f);
        cursor_y += kUiSectionGap;
        if (cursor_y > y + height - static_cast<float>(text_renderer.line_height()) * 4.0f) {
            break;
        }
    }

    if (snapshot.request_in_flight && !snapshot.reveal_active) {
        text_renderer.DrawText(x + kUiPanelPadding, cursor_y, "Amadeus", 0.96f, 0.77f, 0.58f, 1.0f);
        cursor_y += static_cast<float>(text_renderer.line_height());
        DrawWrappedParagraph(text_renderer, x + kUiPanelPadding, cursor_y, width - (kUiPanelPadding * 2.0f), snapshot.status.empty() ? "Thinking..." : snapshot.status, 0.93f, 0.95f, 0.98f, 1.0f);
    } else if (snapshot.reveal_active && !snapshot.visible_reply.empty()) {
        text_renderer.DrawText(x + kUiPanelPadding, cursor_y, "Amadeus", 0.96f, 0.77f, 0.58f, 1.0f);
        cursor_y += static_cast<float>(text_renderer.line_height());
        DrawWrappedParagraph(text_renderer, x + kUiPanelPadding, cursor_y, width - (kUiPanelPadding * 2.0f), snapshot.visible_reply, 0.93f, 0.95f, 0.98f, 1.0f);
    }
}

void AmadeusOverlay::DrawSubtitleStage(
    const AmadeusTextRenderer& text_renderer,
    const Snapshot& snapshot,
    int window_width,
    int window_height) const {
    if (snapshot.request_in_flight && !snapshot.reveal_active) {
        return;
    }

    const std::string subtitle = !snapshot.visible_reply.empty() ? snapshot.visible_reply : snapshot.subtitle;
    const std::string trimmed_subtitle = TrimCopy(subtitle);
    if (snapshot.transcript.empty() && snapshot.visible_reply.empty()) {
        return;
    }
    if (trimmed_subtitle.empty() || trimmed_subtitle == "Ready." || trimmed_subtitle == "Thinking..." || trimmed_subtitle == "Working...") {
        return;
    }

    const float width = std::min(kSubtitleStageMaxWidth, std::max(kSubtitleStageMinWidth, window_width - 120.0f));
    const float text_width = width - (kUiPanelPadding * 2.0f);
    std::vector<std::string> lines = WrapDisplayText(text_renderer, trimmed_subtitle, text_width);

    const auto trim_lines_from_top = [&](std::size_t keep_count) {
        if (lines.size() <= keep_count) {
            return;
        }

        lines.erase(lines.begin(), lines.end() - keep_count);

        const std::string prefix = "... ";
        const float prefix_width = static_cast<float>(text_renderer.MeasureTextWidth(prefix));
        std::string first_line = lines.front();
        while (!first_line.empty()
            && prefix_width + static_cast<float>(text_renderer.MeasureTextWidth(first_line)) > text_width) {
            const std::size_t next = NextUtf8Boundary(first_line, 0, 1);
            if (next == 0 || next > first_line.size()) {
                break;
            }
            first_line.erase(0, next);
        }
        lines.front() = prefix + first_line;
    };

    if (lines.size() > kSubtitleStageMaxLines) {
        trim_lines_from_top(kSubtitleStageMaxLines);
    }

    const float x = (window_width - width) * 0.5f;
    const float line_height = std::max(1.0f, static_cast<float>(text_renderer.line_height()));
    const float header_height = line_height + 10.0f;
    const float stage_chrome_height = (kUiPanelPadding * 2.0f) + header_height;
    const float input_bar_y = window_height - (kInputBarHeight + 36.0f);
    const float max_stage_height = std::max(
        stage_chrome_height + line_height,
        input_bar_y - kSubtitleStageBottomGap - kSubtitleStageMinTop);
    const float available_text_height = std::max(line_height, max_stage_height - stage_chrome_height);
    const std::size_t height_limited_line_count = std::max<std::size_t>(
        1,
        static_cast<std::size_t>(available_text_height / line_height));
    if (lines.size() > height_limited_line_count) {
        trim_lines_from_top(height_limited_line_count);
    }

    const float stage_height = stage_chrome_height + (static_cast<float>(lines.size()) * line_height);
    const float y = std::max(kSubtitleStageMinTop, input_bar_y - kSubtitleStageBottomGap - stage_height);
    const float title_y = kUiPanelPadding;
    const float body_y = title_y + header_height;

    DrawFilledRect(x, y, width, stage_height, 0.01f, 0.01f, 0.02f, 0.82f);
    text_renderer.DrawText(x + kUiPanelPadding, y + title_y, "AMADEUS", 0.93f, 0.95f, 0.98f, 1.0f);

    float line_y = y + body_y;
    for (const std::string& line : lines) {
        text_renderer.DrawText(
            x + kUiPanelPadding,
            line_y,
            line,
            0.98f,
            0.99f,
            1.0f,
            1.0f);
        line_y += line_height;
    }
}

void AmadeusOverlay::DrawInputBar(
    const AmadeusTextRenderer& text_renderer,
    const Snapshot& snapshot,
    int window_width,
    int window_height) const {
    const float width = std::min(kInputBarMaxWidth, std::max(kInputBarMinWidth, window_width - 96.0f));
    const float x = (window_width - width) * 0.5f;
    const float y = window_height - (kInputBarHeight + 36.0f);
    const bool show_caret = (static_cast<int>(glfwGetTime() * 2.0) % 2) == 0;
    const float input_width = width - (kUiPanelPadding * 2.0f);

    DrawFilledRect(x, y, width, kInputBarHeight, 0.03f, 0.05f, 0.06f, 0.90f);
    text_renderer.DrawText(x + kUiPanelPadding, y + 20.0f, "Message", 0.55f, 0.67f, 0.72f, 1.0f);

    std::string input = snapshot.input.empty()
        ? std::string("Type a message and press Enter...")
        : TailDisplaySnippet(text_renderer, snapshot.input, input_width);
    if (!snapshot.input.empty() && show_caret) {
        input.push_back('_');
    }

    text_renderer.DrawText(
        x + kUiPanelPadding,
        y + 20.0f + static_cast<float>(text_renderer.line_height()) + 12.0f,
        input,
        snapshot.input.empty() ? 0.44f : 0.95f,
        snapshot.input.empty() ? 0.53f : 0.97f,
        snapshot.input.empty() ? 0.58f : 1.0f,
        1.0f);
}

void AmadeusOverlay::DrawSettingsButton(
    const AmadeusTextRenderer& text_renderer,
    int window_width) const {
    constexpr float kButtonMargin = 16.0f;
    const std::string label = "[Tab] Settings";
    const float label_w = static_cast<float>(text_renderer.MeasureTextWidth(label));
    const float btn_w = label_w + 24.0f;
    const float btn_h = static_cast<float>(text_renderer.line_height()) + 14.0f;
    const float x = window_width - btn_w - kButtonMargin;
    const float y = kButtonMargin;
    DrawFilledRect(x, y, btn_w, btn_h, 0.07f, 0.11f, 0.14f, 0.82f);
    text_renderer.DrawText(x + 12.0f, y + 8.0f, label, 0.68f, 0.78f, 0.85f, 1.0f);
}

void AmadeusOverlay::DrawSettingsPanel(
    const AmadeusTextRenderer& text_renderer,
    const Snapshot& snapshot,
    int window_width,
    int window_height) const {
    const float line_h = static_cast<float>(text_renderer.line_height());

    // Panel width: fill most of the window but cap at 760px
    const float panel_w = std::min(760.0f, std::max(560.0f, window_width * 0.62f));
    const float pad_x   = 32.0f;
    const float pad_y   = 28.0f;
    const float row_h   = line_h + 24.0f;   // generous vertical breathing room
    const float label_w = panel_w * 0.40f;  // 40% for label, 60% for value
    const float header_h = line_h + 36.0f;

    // Row count: 0=Mode, 1=Voice Language, 2=Provider
    //            [3=Sensitivity, 4=Device, 5=Gain, 6=Gate, 7=Compressor] (stt only)
    // When STT: extra row_h for the level meter below the device row
    const int num_setting_rows = snapshot.stt_enabled ? 8 : 3;
    const float level_meter_extra = snapshot.stt_enabled ? row_h : 0.0f;
    const float panel_h = header_h + pad_y + (num_setting_rows * row_h) + level_meter_extra + pad_y;

    const float x = (window_width - panel_w) * 0.5f;
    const float y = (window_height - panel_h) * 0.38f;

    // Background
    DrawFilledRect(x, y, panel_w, panel_h, 0.04f, 0.06f, 0.09f, 0.95f);

    // Title + hint on the same header line
    text_renderer.DrawText(x + pad_x, y + pad_y, "SETTINGS", 0.95f, 0.97f, 1.0f, 1.0f);
    const std::string hint = "Tab / Esc to close    \xE2\x86\x90\xE2\x86\x92 change value";
    const float hint_w = static_cast<float>(text_renderer.MeasureTextWidth(hint));
    text_renderer.DrawText(
        x + panel_w - pad_x - hint_w,
        y + pad_y,
        hint,
        0.42f, 0.52f, 0.58f, 1.0f);

    // Separator line under header
    DrawFilledRect(x + pad_x, y + header_h - 4.0f, panel_w - pad_x * 2.0f, 1.0f,
                   0.20f, 0.30f, 0.38f, 0.70f);

    float row_y = y + header_h + pad_y * 0.5f;

    const auto draw_row = [&](int row_idx, const std::string& label, const std::string& value, bool read_only) {
        const bool selected = (!read_only && row_idx == snapshot.settings_row);

        // Highlight background for the selected row
        if (selected) {
            DrawFilledRect(
                x + 6.0f,
                row_y - 4.0f,
                panel_w - 12.0f,
                row_h - 2.0f,
                0.10f, 0.22f, 0.32f, 0.90f);
        }

        // Vertically center text within row_h
        const float text_y = row_y + (row_h - line_h) * 0.5f - 2.0f;

        // Label
        const float label_alpha = read_only ? 0.55f : (selected ? 1.0f : 0.80f);
        text_renderer.DrawText(x + pad_x, text_y, label,
                               0.75f, 0.82f, 0.88f, label_alpha);

        // Value
        const float val_x = x + pad_x + label_w;
        if (!read_only) {
            const float arrow_alpha = selected ? 0.85f : 0.35f;
            text_renderer.DrawText(val_x, text_y, "<  ",
                                   0.55f, 0.68f, 0.75f, arrow_alpha);
            const float left_w = static_cast<float>(text_renderer.MeasureTextWidth("<  "));
            text_renderer.DrawText(val_x + left_w, text_y, value,
                                   0.96f, 0.98f, 1.0f, 1.0f);
            const float val_w = static_cast<float>(text_renderer.MeasureTextWidth(value));
            text_renderer.DrawText(val_x + left_w + val_w, text_y, "  >",
                                   0.55f, 0.68f, 0.75f, arrow_alpha);
        } else {
            const std::string display = value.empty() ? "not configured" : value;
            text_renderer.DrawText(val_x, text_y, display,
                                   0.68f, 0.74f, 0.80f, 0.90f);
        }

        row_y += row_h;
    };

    // Row 0: Mode
    draw_row(0, "Mode",
             snapshot.app_mode == AppMode::SpeechToSpeech ? "Speech-to-speech" : "Chat",
             false);

    // Row 1: Voice Language
    std::string lang_val;
    switch (snapshot.voice_lang) {
        case VoiceLang::English:  lang_val = "English";  break;
        case VoiceLang::Japanese: lang_val = "Japanese"; break;
        default:                  lang_val = "Auto";     break;
    }
    draw_row(1, "Voice Language", lang_val, false);

    // Row 2: Provider — left/right opens the provider sub-panel
    {
        const std::string model_hint = snapshot.sub_field_model.empty()
            ? "" : "  /  " + snapshot.sub_field_model;
        std::string prov_val = snapshot.provider_sub_type_name.empty()
            ? "Configure..."
            : snapshot.provider_sub_type_name + model_hint;
        if (snapshot.llm_loading) {
            prov_val += "  (loading model...)";
        }
        draw_row(2, "Provider", prov_val, false);
    }

    // Row 3: Mic Sensitivity (only when STT available)
    if (snapshot.stt_enabled) {
        std::string sens_val;
        switch (snapshot.stt_sensitivity) {
            case VadSensitivity::Low:  sens_val = "Low";    break;
            case VadSensitivity::High: sens_val = "High";   break;
            default:                   sens_val = "Medium"; break;
        }
        draw_row(3, "Mic Sensitivity", sens_val, false);

        // Row 4: Mic Device
        std::string device_val = snapshot.stt_device_name.empty()
            ? "Default"
            : snapshot.stt_device_name;
        // Truncate long device names to fit the column
        const float max_val_w = panel_w - pad_x - label_w - 80.0f;
        while (!device_val.empty()
            && static_cast<float>(text_renderer.MeasureTextWidth(device_val)) > max_val_w) {
            // Trim from end, codepoint by codepoint
            std::size_t trim = device_val.size() - 1;
            while (trim > 0 && (static_cast<unsigned char>(device_val[trim]) & 0xC0u) == 0x80u) {
                --trim;
            }
            device_val = device_val.substr(0, trim) + "...";
            // Break after one truncation attempt to avoid infinite loop
            break;
        }
        draw_row(4, "Mic Device", device_val, false);

        // Mic level meter — drawn below the device row
        {
            const float meter_x     = x + pad_x + label_w;
            const float meter_y     = row_y + (row_h - 10.0f) * 0.5f - 4.0f;
            const float meter_w     = panel_w - pad_x - label_w - pad_x;
            const float meter_h     = 8.0f;
            const float fill        = std::min(1.0f, snapshot.stt_mic_level * 8.0f); // scale up small values

            // Track background
            DrawFilledRect(x + pad_x, meter_y, meter_w + (label_w - pad_x), meter_h,
                           0.08f, 0.12f, 0.16f, 0.80f);
            // Level label
            text_renderer.DrawText(x + pad_x, meter_y - line_h * 0.5f - 2.0f,
                                   "Mic Level", 0.55f, 0.62f, 0.68f, 0.80f);
            // Fill bar — colour shifts green → yellow → red with level
            const float bar_r = std::min(1.0f, fill * 2.0f);
            const float bar_g = std::min(1.0f, 2.0f - fill * 2.0f);
            DrawFilledRect(meter_x, meter_y,
                           (meter_w) * fill, meter_h,
                           bar_r * 0.6f + 0.1f,
                           bar_g * 0.7f + 0.2f,
                           0.20f,
                           0.90f);
            // Tick marks every 25%
            for (int t = 1; t < 4; ++t) {
                const float tick_x = meter_x + meter_w * (t * 0.25f);
                DrawFilledRect(tick_x, meter_y, 1.0f, meter_h,
                               0.30f, 0.40f, 0.50f, 0.60f);
            }

            row_y += row_h;
        }

        // Row 5: Mic Gain
        {
            const int gain_db = (snapshot.mic_gain_step - 4) * 3;
            std::string gain_val;
            if (gain_db > 0)       gain_val = "+" + std::to_string(gain_db) + " dB";
            else if (gain_db == 0) gain_val = "0 dB";
            else                   gain_val = std::to_string(gain_db) + " dB";
            draw_row(5, "Mic Gain", gain_val, false);
        }

        // Row 6: Noise Gate
        {
            const char* gate_labels[] = { "Off", "Low", "Medium", "High" };
            draw_row(6, "Noise Gate", gate_labels[snapshot.mic_gate_step], false);
        }

        // Row 7: Compressor
        {
            const char* comp_labels[] = { "Off", "Light", "Medium", "Heavy" };
            draw_row(7, "Compressor", comp_labels[snapshot.mic_comp_step], false);
        }
    }

    // STT state badge when speech mode is active
    if (snapshot.stt_enabled && snapshot.app_mode == AppMode::SpeechToSpeech) {
        const char* badge = nullptr;
        float br = 0.60f, bg = 0.90f, bb = 0.65f;
        switch (snapshot.stt_state) {
            case 1:  badge = "  Listening...";  br = 0.40f; bg = 0.85f; bb = 0.55f; break;
            case 2:  badge = "  Processing..."; br = 0.90f; bg = 0.80f; bb = 0.30f; break;
            case 3:  badge = "  Responding..."; br = 0.40f; bg = 0.70f; bb = 0.95f; break;
            default: badge = "  Mic standby";                                         break;
        }
        text_renderer.DrawText(x + pad_x, y + panel_h - line_h - 10.0f, badge, br, bg, bb, 0.92f);
    }

    // Provider sub-panel — drawn as a second panel to the right of the main one
    if (snapshot.provider_sub_open) {
        // Determine how many text rows the current provider has.
        // type 0,1,2 (Anthropic/OpenAI/Gemini): model + api_key
        // type 3 (OpenAI-compat): model + endpoint + api_key
        // type 4 (Ollama): endpoint + model (cycle)
        // type 5 (Llama.cpp): model_path + download status
        // type 6 (Amadeus built-in): download status
        struct SubRow {
            std::string label;
            std::string value;
            bool is_cycle;   // arrows change value (no text edit)
            bool read_only;  // purely informational
        };
        std::vector<SubRow> rows;
        // Row 0: provider type (cycle with arrows)
        rows.push_back({"Provider",
            snapshot.provider_sub_type_name.empty() ? "Unknown" : snapshot.provider_sub_type_name,
            /*is_cycle=*/true, /*read_only=*/false});

        switch (snapshot.provider_sub_type_idx) {
        case 0: case 1: case 2:  // Anthropic / OpenAI / Gemini
            rows.push_back({"Model",   snapshot.sub_field_model,  false, false});
            rows.push_back({"API Key", snapshot.sub_field_apikey, false, false});
            break;
        case 3:  // OpenAI-compatible
            rows.push_back({"Model",    snapshot.sub_field_model,    false, false});
            rows.push_back({"Endpoint", snapshot.sub_field_endpoint, false, false});
            rows.push_back({"API Key",  snapshot.sub_field_apikey,   false, false});
            break;
        case 4: {  // Ollama
            rows.push_back({"Endpoint", snapshot.sub_field_endpoint.empty()
                ? "http://127.0.0.1:11434" : snapshot.sub_field_endpoint, false, false});
            // Model row: cycle from fetched list
            std::string model_val;
            if (snapshot.ollama_fetch_status == 1) {
                model_val = "Fetching models...";
            } else if (snapshot.ollama_fetch_status == 3) {
                model_val = "Error — check endpoint";
            } else if (snapshot.ollama_model_count == 0) {
                model_val = "No models — confirm endpoint";
            } else {
                model_val = snapshot.ollama_model_name.empty()
                    ? "Unknown" : snapshot.ollama_model_name;
            }
            const bool model_cycle = (snapshot.ollama_fetch_status == 2
                                      && snapshot.ollama_model_count > 0);
            rows.push_back({"Model", model_val, model_cycle, !model_cycle});
            break;
        }
        case 5:  // Llama.cpp
            rows.push_back({"Model Path", snapshot.sub_field_model, false, false});
            {
                std::string dl_val;
                bool dl_read_only = true;
                if (snapshot.gguf_model_exists) {
                    dl_val = "Model ready";
                } else if (snapshot.gguf_download_status == 1) {
                    dl_val = "Downloading... " + std::to_string(snapshot.gguf_download_progress) + "%";
                } else if (snapshot.gguf_download_status == 3) {
                    dl_val = "Download failed — Enter to retry";
                    dl_read_only = false;
                } else {
                    dl_val = "Not downloaded — Enter to download";
                    dl_read_only = false;
                }
                rows.push_back({"Model File", dl_val, false, dl_read_only});
            }
            break;
        case 6:  // Amadeus built-in
            {
                std::string dl_val;
                bool dl_read_only = true;
                if (snapshot.gguf_model_exists) {
                    dl_val = "Model ready";
                } else if (snapshot.gguf_download_status == 1) {
                    dl_val = "Downloading... " + std::to_string(snapshot.gguf_download_progress) + "%";
                } else if (snapshot.gguf_download_status == 3) {
                    dl_val = "Download failed — Enter to retry";
                    dl_read_only = false;
                } else {
                    dl_val = "Not downloaded — Enter to download";
                    dl_read_only = false;
                }
                rows.push_back({"Model File", dl_val, false, dl_read_only});
            }
            break;
        default: break;
        }
        // Save row: always last
        rows.push_back({"", "SAVE", /*is_cycle=*/false, /*read_only=*/false});

        const int num_sub_rows = static_cast<int>(rows.size());
        const float sub_w = std::min(580.0f, static_cast<float>(window_width) * 0.46f);
        constexpr float sub_pad_x = 28.0f;
        constexpr float sub_pad_y = 24.0f;
        const float sub_row_h = row_h;
        const float sub_header_h = line_h + 32.0f;
        const float sub_panel_h =
            sub_header_h + sub_pad_y + num_sub_rows * sub_row_h + sub_pad_y;
        const float sub_x = std::min(
            static_cast<float>(window_width) - sub_w - 12.0f,
            x + panel_w + 12.0f);
        const float sub_y = y;

        DrawFilledRect(sub_x, sub_y, sub_w, sub_panel_h, 0.04f, 0.07f, 0.10f, 0.97f);

        // Title
        text_renderer.DrawText(sub_x + sub_pad_x, sub_y + sub_pad_y,
                               "PROVIDER CONFIG", 0.95f, 0.97f, 1.0f, 1.0f);
        const std::string sub_hint = snapshot.sub_editing
            ? "Enter confirm    Esc cancel"
            : "Enter edit / save    Esc close";
        const float sub_hint_w = static_cast<float>(text_renderer.MeasureTextWidth(sub_hint));
        text_renderer.DrawText(sub_x + sub_w - sub_pad_x - sub_hint_w,
                               sub_y + sub_pad_y, sub_hint, 0.42f, 0.52f, 0.58f, 1.0f);

        DrawFilledRect(sub_x + sub_pad_x, sub_y + sub_header_h - 4.0f,
                       sub_w - sub_pad_x * 2.0f, 1.0f, 0.20f, 0.30f, 0.38f, 0.70f);

        float sub_row_y = sub_y + sub_header_h + sub_pad_y * 0.5f;
        const float sub_label_w = sub_w * 0.35f;
        const float val_area_w = sub_w - sub_pad_x - sub_label_w - sub_pad_x;

        for (int ri = 0; ri < num_sub_rows; ++ri) {
            const SubRow& sr = rows[static_cast<std::size_t>(ri)];
            const bool sel = (ri == snapshot.provider_sub_row);
            const bool editing = sel && snapshot.sub_editing && !sr.is_cycle && !sr.read_only;
            const bool is_save_row = sr.label.empty() && sr.value == "SAVE";

            if (is_save_row) {
                // Save row: distinct accent highlight
                const float bg_r = sel ? 0.10f : 0.06f;
                const float bg_g = sel ? 0.32f : 0.20f;
                const float bg_b = sel ? 0.18f : 0.10f;
                DrawFilledRect(sub_x + 6.0f, sub_row_y - 4.0f,
                               sub_w - 12.0f, sub_row_h - 2.0f,
                               bg_r, bg_g, bg_b, 0.92f);
                const float text_y = sub_row_y + (sub_row_h - line_h) * 0.5f - 2.0f;
                const float save_w = static_cast<float>(text_renderer.MeasureTextWidth("SAVE"));
                text_renderer.DrawText(sub_x + (sub_w - save_w) * 0.5f, text_y, "SAVE",
                                       sel ? 0.40f : 0.30f,
                                       sel ? 0.96f : 0.68f,
                                       sel ? 0.58f : 0.40f,
                                       sel ? 1.0f  : 0.75f);
                sub_row_y += sub_row_h;
                continue;
            }

            if (sel && !sr.read_only) {
                const float bg_r = editing ? 0.08f : 0.10f;
                const float bg_g = editing ? 0.18f : 0.22f;
                const float bg_b = editing ? 0.28f : 0.32f;
                DrawFilledRect(sub_x + 6.0f, sub_row_y - 4.0f,
                               sub_w - 12.0f, sub_row_h - 2.0f,
                               bg_r, bg_g, bg_b, 0.90f);
            }

            const float text_y = sub_row_y + (sub_row_h - line_h) * 0.5f - 2.0f;
            const float la = (sr.read_only) ? 0.55f : (sel ? 1.0f : 0.80f);
            text_renderer.DrawText(sub_x + sub_pad_x, text_y, sr.label,
                                   0.75f, 0.82f, 0.88f, la);

            const float val_x = sub_x + sub_pad_x + sub_label_w;

            if (sr.is_cycle) {
                const float aa = sel ? 0.85f : 0.35f;
                text_renderer.DrawText(val_x, text_y, "<  ",
                                       0.55f, 0.68f, 0.75f, aa);
                const float lw =
                    static_cast<float>(text_renderer.MeasureTextWidth("<  "));
                text_renderer.DrawText(val_x + lw, text_y, sr.value,
                                       0.96f, 0.98f, 1.0f, 1.0f);
                const float vw =
                    static_cast<float>(text_renderer.MeasureTextWidth(sr.value));
                text_renderer.DrawText(val_x + lw + vw, text_y, "  >",
                                       0.55f, 0.68f, 0.75f, aa);
            } else if (sr.read_only) {
                text_renderer.DrawText(val_x, text_y, sr.value,
                                       0.68f, 0.74f, 0.82f, 0.65f);
            } else if (editing) {
                // Active text input: show buffer with cursor, clip from left if too wide.
                const std::string buf = snapshot.sub_edit_buffer + "|";
                std::string visible_buf = buf;
                while (!visible_buf.empty() &&
                       static_cast<float>(text_renderer.MeasureTextWidth(visible_buf)) > val_area_w) {
                    std::size_t i = 1;
                    while (i < visible_buf.size() &&
                           (static_cast<unsigned char>(visible_buf[i]) & 0xC0u) == 0x80u) {
                        ++i;
                    }
                    visible_buf = visible_buf.substr(i);
                }
                text_renderer.DrawText(val_x, text_y, visible_buf,
                                       0.96f, 0.98f, 1.0f, 1.0f);
            } else {
                // Inactive editable field
                std::string display = sr.value;
                if (sr.label == "API Key" && !display.empty()) {
                    display = "\xE2\x80\xA2\xE2\x80\xA2\xE2\x80\xA2\xE2\x80\xA2\xE2\x80\xA2\xE2\x80\xA2";
                }
                if (display.empty()) {
                    display = sel ? "Press Enter to edit" : "(not set)";
                }
                // Truncate to fit
                while (!display.empty() &&
                       static_cast<float>(text_renderer.MeasureTextWidth(display)) > val_area_w) {
                    std::size_t trim = display.size() - 1;
                    while (trim > 0 &&
                           (static_cast<unsigned char>(display[trim]) & 0xC0u) == 0x80u) {
                        --trim;
                    }
                    display = display.substr(0, trim) + "..";
                    break;
                }
                text_renderer.DrawText(val_x, text_y, display,
                                       0.80f, 0.88f, 0.96f, sel ? 0.95f : 0.70f);
            }

            sub_row_y += sub_row_h;
        }
    }
}

void AmadeusOverlay::DrawSttMicIndicator(
    const AmadeusTextRenderer& text_renderer,
    const Snapshot& snapshot,
    int window_width,
    int window_height) const {
    const float line_h = static_cast<float>(text_renderer.line_height());
    const float pad    = 18.0f;
    const float bar_w  = 220.0f;
    const float bar_h  = 8.0f;

    // If there's partial text, show it above the bar
    const bool has_partial = !snapshot.stt_partial_text.empty();
    const float partial_area_h = has_partial ? (line_h + 10.0f) : 0.0f;

    const float pill_w = std::min(static_cast<float>(window_width) - 40.0f,
                                  std::max(bar_w + pad * 2.0f,
                                           has_partial
                                               ? static_cast<float>(text_renderer.MeasureTextWidth(snapshot.stt_partial_text)) + pad * 2.0f
                                               : 0.0f));
    const float pill_h = bar_h + line_h + 22.0f + partial_area_h;
    const float x      = (window_width - pill_w) * 0.5f;
    const float y      = window_height - pill_h - 36.0f;

    // State label and colour
    const char* state_text = "Mic standby";
    float sr = 0.50f, sg = 0.65f, sb = 0.55f;
    switch (snapshot.stt_state) {
        case 1: state_text = "Listening";  sr = 0.30f; sg = 0.88f; sb = 0.45f; break;
        case 2: state_text = "Processing"; sr = 0.92f; sg = 0.78f; sb = 0.20f; break;
        case 3: state_text = "Responding"; sr = 0.30f; sg = 0.62f; sb = 0.98f; break;
        default: break;
    }

    DrawFilledRect(x, y, pill_w, pill_h, 0.04f, 0.06f, 0.09f, 0.88f);

    // State label (top row)
    const float state_w = static_cast<float>(text_renderer.MeasureTextWidth(state_text));
    text_renderer.DrawText(x + (pill_w - state_w) * 0.5f, y + 8.0f, state_text,
                           sr, sg, sb, 1.0f);

    // Partial transcription text (below state label)
    if (has_partial) {
        text_renderer.DrawText(x + pad, y + 8.0f + line_h + 4.0f,
                               snapshot.stt_partial_text,
                               0.94f, 0.96f, 1.0f, 0.95f);
    }

    // Level bar (bottom row)
    const float bar_x = x + (pill_w - bar_w) * 0.5f;
    const float bar_y = y + pill_h - bar_h - 8.0f;

    // Track
    DrawFilledRect(bar_x, bar_y, bar_w, bar_h, 0.10f, 0.15f, 0.20f, 0.90f);

    // Fill — green → yellow → red
    const float fill = std::min(1.0f, snapshot.stt_mic_level * 8.0f);
    if (fill > 0.004f) {
        const float bar_r = std::min(1.0f, fill * 2.0f);
        const float bar_g = std::min(1.0f, 2.0f - fill * 2.0f);
        DrawFilledRect(bar_x, bar_y, bar_w * fill, bar_h,
                       bar_r * 0.50f + 0.08f, bar_g * 0.72f + 0.18f, 0.18f, 0.92f);
    }

    // Tick marks at 25%, 50%, 75%
    for (int t = 1; t < 4; ++t) {
        DrawFilledRect(bar_x + bar_w * (t * 0.25f), bar_y, 1.0f, bar_h,
                       0.28f, 0.36f, 0.46f, 0.65f);
    }
}

void AmadeusOverlay::Render(const AmadeusTextRenderer& text_renderer, int window_width, int window_height) {
    if (!text_renderer.IsReady()) {
        return;
    }

    const Snapshot snapshot = CaptureSnapshot();
    BeginOverlay(window_width, window_height);

    if (snapshot.settings_open) {
        DrawSettingsPanel(text_renderer, snapshot, window_width, window_height);
    } else {
        DrawSubtitleStage(text_renderer, snapshot, window_width, window_height);
        if (snapshot.app_mode != AppMode::SpeechToSpeech) {
            DrawInputBar(text_renderer, snapshot, window_width, window_height);
        } else if (snapshot.stt_enabled) {
            DrawSttMicIndicator(text_renderer, snapshot, window_width, window_height);
        }
    }

    DrawSettingsButton(text_renderer, window_width);

    // Loading toast: shown in the top-right (below the settings button) while the local
    // LLM preload thread is still running.
    if (snapshot.llm_loading) {
        const std::string msg = "Loading model...";
        const float tw = static_cast<float>(text_renderer.MeasureTextWidth(msg));
        const float lh = static_cast<float>(text_renderer.line_height());
        const float pw = tw + 24.0f;
        const float ph = lh + 14.0f;
        const float px = window_width - pw - 16.0f;
        const float py = 48.0f;  // just below the settings button
        DrawFilledRect(px, py, pw, ph, 0.06f, 0.16f, 0.22f, 0.88f);
        text_renderer.DrawText(px + 12.0f, py + 8.0f, msg, 0.55f, 0.80f, 0.95f, 1.0f);
    }

    // Thinking badge: amber indicator while the model is inside a <think> block.
    // Reserved for future animation hookup; currently just a visual state marker.
    if (snapshot.llm_thinking) {
        const std::string msg = "Thinking...";
        const float tw = static_cast<float>(text_renderer.MeasureTextWidth(msg));
        const float lh = static_cast<float>(text_renderer.line_height());
        const float pw = tw + 24.0f;
        const float ph = lh + 14.0f;
        const float px = window_width - pw - 16.0f;
        const float py = snapshot.llm_loading ? 96.0f : 48.0f;
        DrawFilledRect(px, py, pw, ph, 0.18f, 0.14f, 0.04f, 0.88f);
        text_renderer.DrawText(px + 12.0f, py + 8.0f, msg, 0.95f, 0.78f, 0.30f, 1.0f);
    }

    EndOverlay();
}
