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

void AmadeusOverlay::Render(const AmadeusTextRenderer& text_renderer, int window_width, int window_height) {
    if (!text_renderer.IsReady()) {
        return;
    }

    const Snapshot snapshot = CaptureSnapshot();
    BeginOverlay(window_width, window_height);
    DrawSubtitleStage(text_renderer, snapshot, window_width, window_height);
    DrawInputBar(text_renderer, snapshot, window_width, window_height);
    EndOverlay();
}
