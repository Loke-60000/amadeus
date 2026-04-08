#include "boot_sequence.hpp"

#include <GL/glew.h>
#include <GLFW/glfw3.h>

#include <algorithm>
#include <cstdlib>
#include <cmath>
#include <fstream>
#include <string>
#include <vector>

#include "font_renderer.hpp"
#include "stb_image.h"

extern "C" unsigned int amadeus_native_boot_audio_play(const char* path, unsigned int fallback_ms);

extern "C" {
    // GGUF (LLM) model download
    int  amadeus_native_gguf_model_exists(int type_index);
    void amadeus_native_gguf_download_start(int type_index);
    int  amadeus_native_gguf_download_status();
    int  amadeus_native_gguf_download_progress();

    // STT model download
    int  amadeus_native_stt_model_exists();
    void amadeus_native_stt_download_start();
    int  amadeus_native_stt_download_status();
    int  amadeus_native_stt_download_progress();

    // TTS model cache check
    int  amadeus_native_tts_model_cached();

    // Post-boot service init
    void amadeus_native_init_services();
}

// ─── Boot terminal lines ──────────────────────────────────────────────────────
// Edit this array to change what appears during the terminal phase.

const std::vector<std::string> BootSequence::kTerminalLines = {
    "amadeus System ver.1.09.2 re.v2123",
    "",
    "",
    "  >>Initialize System ... OK",
    "  >>Detecting boot device ... OK",
    "  >>:Loading kerner ... OK",
    "  >>Detecting OS control device ... OK",
    "  Booting...",
    "  >>Processor 0 is activated ... OK",
    "  >>Processor 1 is activated ... OK",
    "  >>Processor 2 is activated ... OK",
    "  >>Processor 3 is activated ... OK",
    "  >>Memory Initialization 0/32767MBytes",
    "",
    "INIT: Kernel version 2.04 booting...",
    "",
    "Mounting proc at /proc ... [OK]",
    "Mounting sysfs at /sts ... [OK]",
    "Initializing network ...",
    "Setting up localhost ... [OK]",
    "Setting up inet1 ... [OK]",
    "Setting up route ... [OK]",
    "Accessing croute ... [OK]",
    "Starting system log at /log/sys ... [OK]",
    "Cleaning /var/lock ... [OK]",
    "Cleaning /tmp ... [OK]",
    "Initializing init.rc ... [OK]",
    
    };

// ─── Constants ────────────────────────────────────────────────────────────────

namespace {

// Time between each character reveal
constexpr double kCharIntervalSec    = 0.008;
// Pause after a non-empty line finishes printing
constexpr double kLineEndPauseSec    = 0.05;
// Pause for empty separator lines
constexpr double kEmptyLinePauseSec  = 0.02;
// Hold time after all lines are printed before switching to the logo phase
constexpr double kTerminalExitHoldSec = 0.3;

// Terminal text layout
constexpr float kPadX = 52.0f;
constexpr float kPadY = 58.0f;
constexpr float kLineSpacingMul = 1.55f;  // multiplier over line_height()

// Red phosphor color for normal lines
constexpr float kGreenR = 0.95f;
constexpr float kGreenG = 0.08f;
constexpr float kGreenB = 0.08f;

// Dimmer tint for "[OK]" status lines
constexpr float kDimR = 0.62f;
constexpr float kDimG = 0.05f;
constexpr float kDimB = 0.05f;

constexpr float kFullAlpha = 1.0f;
constexpr float kCursorAlpha = 0.85f;

// Memory counter special line — prefix that triggers the animated counter
constexpr const char* kMemoryCounterPrefix = "  >>Memory Initialization ";
constexpr int         kMemoryCounterMax    = 32767;
constexpr double      kMemoryCounterSec    = 1.4;  // how long 0→max takes

// ── Tuning knobs ─────────────────────────────────────────────────────────────
// Change these two values to adjust the look without touching anything else.
constexpr int   kTerminalFontSize = 30;    // pixel size of the terminal monospace font
constexpr float kLogoScale        = 1.0f; // logo as a fraction of the smaller screen dimension

// Logo frame animation — fallback playback time (overridden by actual audio duration) and final hold
constexpr unsigned int kFramePlaybackFallbackMs = 2727;
constexpr double       kFinalHoldMs             = 4000.0;

// ── UTF-8 helpers ─────────────────────────────────────────────────────────────

std::size_t CodepointCount(const std::string& s)
{
    std::size_t n = 0;
    for (unsigned char c : s) {
        if ((c & 0xC0u) != 0x80u) ++n;
    }
    return n;
}

// Returns the byte-length of the first `n` codepoints in `s`.
std::size_t CodepointByteLen(const std::string& s, std::size_t n)
{
    std::size_t idx   = 0;
    std::size_t count = 0;
    while (idx < s.size() && count < n) {
        const unsigned char c = static_cast<unsigned char>(s[idx]);
        if      ((c & 0x80u) == 0u)   idx += 1;
        else if ((c & 0xE0u) == 0xC0u) idx += 2;
        else if ((c & 0xF0u) == 0xE0u) idx += 3;
        else                            idx += 4;
        ++count;
    }
    return idx;
}

bool IsDimLine(const std::string& s)
{
    return s.find("[OK]")   != std::string::npos
        || s.find("[WARN]") != std::string::npos
        || s.find("[ERR]")  != std::string::npos;
}

}  // namespace

// ─── BootSequence ─────────────────────────────────────────────────────────────

BootSequence::BootSequence(
    GLFWwindow* window,
    int window_width,
    int window_height)
    : window_(window)
    , window_width_(window_width)
    , window_height_(window_height)
{
    // 20px monospace — large enough to read, small enough to fit many lines.
    // Falls back silently if no monospace font is found on the system.
    term_renderer_.InitializeMonospace(kTerminalFontSize);
}

bool BootSequence::Run()
{
    if (!RunTerminalPhase())      return false;
    if (!RunModelLoadingPhase())  return false;
    if (!RunLogoPhase())          return false;
    return true;
}

// ─── GL helpers ───────────────────────────────────────────────────────────────

void BootSequence::BeginDraw() const
{
    glUseProgram(0);
    glBindBuffer(GL_ARRAY_BUFFER, 0);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
    glPushAttrib(GL_ALL_ATTRIB_BITS);
    glMatrixMode(GL_PROJECTION);
    glPushMatrix();
    glLoadIdentity();
    glOrtho(0.0, static_cast<double>(window_width_),
            static_cast<double>(window_height_), 0.0, -1.0, 1.0);
    glMatrixMode(GL_MODELVIEW);
    glPushMatrix();
    glLoadIdentity();
    glDisable(GL_DEPTH_TEST);
    glDisable(GL_CULL_FACE);
    glDisable(GL_SCISSOR_TEST);
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
}

void BootSequence::EndDraw() const
{
    glMatrixMode(GL_MODELVIEW);
    glPopMatrix();
    glMatrixMode(GL_PROJECTION);
    glPopMatrix();
    glPopAttrib();
}

bool BootSequence::SwapAndPoll()
{
    glfwSwapBuffers(window_);
    glfwPollEvents();

    // Refresh cached window size in case of resize
    int w = 0, h = 0;
    glfwGetWindowSize(window_, &w, &h);
    if (w > 0 && h > 0) {
        window_width_  = w;
        window_height_ = h;
        glViewport(0, 0, w, h);
    }

    return glfwWindowShouldClose(window_) == GLFW_FALSE;
}

void BootSequence::DrawImageTexture(unsigned int tex, int img_w, int img_h,
                                    float box_w, float box_h) const
{
    // Letterbox this texture inside the caller-supplied box, then center on screen.
    const float scale  = std::min(box_w / static_cast<float>(img_w),
                                  box_h / static_cast<float>(img_h));
    const float draw_w = static_cast<float>(img_w) * scale;
    const float draw_h = static_cast<float>(img_h) * scale;
    const float ox = (static_cast<float>(window_width_)  - draw_w) * 0.5f;
    const float oy = (static_cast<float>(window_height_) - draw_h) * 0.5f;

    glBindTexture(GL_TEXTURE_2D, static_cast<GLuint>(tex));
    glEnable(GL_TEXTURE_2D);
    glColor4f(1.0f, 1.0f, 1.0f, 1.0f);
    glBegin(GL_QUADS);
    glTexCoord2f(0.0f, 0.0f); glVertex2f(ox,          oy);
    glTexCoord2f(1.0f, 0.0f); glVertex2f(ox + draw_w, oy);
    glTexCoord2f(1.0f, 1.0f); glVertex2f(ox + draw_w, oy + draw_h);
    glTexCoord2f(0.0f, 1.0f); glVertex2f(ox,          oy + draw_h);
    glEnd();
    glDisable(GL_TEXTURE_2D);
    glBindTexture(GL_TEXTURE_2D, 0);
}

// ─── Phase 1: terminal typewriter ─────────────────────────────────────────────

bool BootSequence::RunTerminalPhase()
{
    const int line_h = std::max(1,
        static_cast<int>(static_cast<float>(term_renderer_.line_height()) * kLineSpacingMul));

    const int max_visible = std::max(1,
        (window_height_ - static_cast<int>(kPadY * 2.0f)) / line_h);

    struct PrintedLine { std::string text; bool dim; };
    std::vector<PrintedLine> printed;

    std::size_t line_idx    = 0;
    std::size_t char_idx    = 0;
    bool        line_done   = false;
    double      last_tick   = glfwGetTime();
    double      line_end_at = -1.0;

    // Memory counter state — active only while processing the counter line
    bool   mem_counter_active = false;
    double mem_counter_start  = 0.0;
    int    mem_counter_value  = 0;
    std::string mem_current_display;   // rebuilt each tick

    const auto is_mem_line = [](const std::string& s) -> bool {
        return s.rfind(kMemoryCounterPrefix, 0) == 0;
    };

    // ── draw all committed + current-in-progress lines ───────────────────────
    const float cell_w = static_cast<float>(term_renderer_.char_width());

    const auto draw_line = [&](float y, const std::string& text, bool dim) {
        if (dim)
            term_renderer_.DrawTextFixed(kPadX, y, text, cell_w, kDimR, kDimG, kDimB, kFullAlpha);
        else
            term_renderer_.DrawTextFixed(kPadX, y, text, cell_w, kGreenR, kGreenG, kGreenB, kFullAlpha);
    };

    const auto draw_frame = [&]() {
        glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        BeginDraw();

        const int row_offset = std::max(0, static_cast<int>(printed.size()) - max_visible + 1);

        for (int i = row_offset; i < static_cast<int>(printed.size()); ++i) {
            const float y = kPadY + static_cast<float>((i - row_offset) * line_h);
            const auto& pl = printed[i];
            draw_line(y, pl.text, pl.dim);
        }

        if (line_idx < kTerminalLines.size()) {
            const int   cur_row = static_cast<int>(printed.size()) - row_offset;
            const float y       = kPadY + static_cast<float>(cur_row * line_h);

            if (mem_counter_active) {
                draw_line(y, mem_current_display, false);
            } else {
                const std::string& src     = kTerminalLines[line_idx];
                const std::size_t  blen    = line_done ? src.size()
                                                       : CodepointByteLen(src, char_idx);
                const std::string  partial(src, 0, blen);

                if (!partial.empty())
                    draw_line(y, partial, IsDimLine(src));

                if (!line_done) {
                    // Cursor at fixed cell position
                    const std::size_t col   = CodepointCount(partial);
                    const float       cx    = kPadX + static_cast<float>(col) * cell_w;
                    const bool        blink = (static_cast<int>(glfwGetTime() * 2.0) & 1) == 0;
                    if (blink)
                        term_renderer_.DrawTextFixed(cx, y, "_", cell_w,
                                                     kGreenR, kGreenG, kGreenB, kCursorAlpha);
                }
            }
        }

        EndDraw();
    };

    // ── main loop ─────────────────────────────────────────────────────────────
    while (glfwWindowShouldClose(window_) == GLFW_FALSE) {
        const double now = glfwGetTime();

        if (mem_counter_active) {
            // Animate counter 0 → kMemoryCounterMax over kMemoryCounterSec
            const double t = std::min(1.0, (now - mem_counter_start) / kMemoryCounterSec);
            mem_counter_value   = static_cast<int>(t * kMemoryCounterMax);
            mem_current_display = std::string(kMemoryCounterPrefix)
                                + std::to_string(mem_counter_value)
                                + "/" + std::to_string(kMemoryCounterMax) + "MBytes";

            if (t >= 1.0) {
                // Counter finished — commit the completed line
                mem_counter_active = false;
                printed.push_back({mem_current_display, false});
                ++line_idx;
                char_idx    = 0;
                line_done   = false;
                last_tick   = now;
                line_end_at = -1.0;

                if (line_idx >= kTerminalLines.size()) goto all_done;
            }
        } else if (!line_done) {
            const std::string& src   = kTerminalLines[line_idx];
            const std::size_t  total = CodepointCount(src);

            if (total == 0) {
                line_done   = true;
                line_end_at = now + kEmptyLinePauseSec;
            } else if (is_mem_line(src)) {
                // Switch to counter mode immediately (no typewriter for this line)
                mem_counter_active = true;
                mem_counter_start  = now;
                mem_counter_value  = 0;
            } else if (now - last_tick >= kCharIntervalSec) {
                ++char_idx;
                last_tick = now;
                if (char_idx >= total) {
                    line_done   = true;
                    line_end_at = now + kLineEndPauseSec;
                }
            }
        } else if (now >= line_end_at) {
            const std::string& src = kTerminalLines[line_idx];
            printed.push_back({src, IsDimLine(src)});
            ++line_idx;
            char_idx    = 0;
            line_done   = false;
            last_tick   = now;

            if (line_idx >= kTerminalLines.size()) goto all_done;
        }

        draw_frame();
        if (!SwapAndPoll()) return false;
        continue;

    all_done:
        draw_frame();
        if (!SwapAndPoll()) return false;
        {
            const double hold_until = glfwGetTime() + kTerminalExitHoldSec;
            while (glfwGetTime() < hold_until) {
                if (glfwWindowShouldClose(window_)) return false;
                draw_frame();
                if (!SwapAndPoll()) return false;
            }
        }
        return true;
    }

    return false;
}

// ─── Phase 1.5: model loading bars ───────────────────────────────────────────

bool BootSequence::RunModelLoadingPhase()
{
    // If all models are already present, skip the loading bars entirely
    // and proceed directly to the logo phase.
    const bool llm_downloading = amadeus_native_gguf_download_status() == 1;
    const bool stt_downloading = amadeus_native_stt_download_status() == 1;
    if (!llm_downloading && !stt_downloading) {
        amadeus_native_init_services();
        return glfwWindowShouldClose(window_) == GLFW_FALSE;
    }

    // ── At least one model is downloading — show the loading bars ────────────
    constexpr int   kBarCols  = 24;     // filled cells in a complete bar
    constexpr float kBarScale = 1.0f;   // reserved for future per-row scaling

    const int line_h = std::max(1,
        static_cast<int>(static_cast<float>(term_renderer_.line_height()) * kLineSpacingMul));
    const float cell_w = static_cast<float>(term_renderer_.char_width());

    // Helper: draw one bar row.
    //   label     — left-aligned name, padded to 14 chars
    //   progress  — 0..100
    //   status    — 0=idle 1=downloading 2=done 3=error
    const auto draw_row = [&](float y, const char* label, int progress, int status) {
        char buf[128];

        // Determine filled bar width and right-side text
        int filled = 0;
        const char* right_text = "";
        char right_buf[32] = {};
        if (status == 2) {
            filled = kBarCols;
            right_text = "Ready";
        } else if (status == 3) {
            filled = 0;
            right_text = "Error";
        } else if (status == 1) {
            filled = (int)(progress * kBarCols / 100.0f);
            std::snprintf(right_buf, sizeof(right_buf), "%3d%%", progress);
            right_text = right_buf;
        } else {
            right_text = "Waiting";
        }

        // Build bar string: [████░░░░░░░░] or [████████████]
        char bar[kBarCols + 3 + 1];  // '[' + cells + ']' + '\0'
        bar[0] = '[';
        for (int i = 0; i < kBarCols; ++i) {
            // Use UTF-8 block character: █ (U+2588) = 0xE2 0x96 0x88
            // Use ░ (U+2591) = 0xE2 0x96 0x91 for empty
            if (i < filled)
                bar[i + 1] = '\x01';  // placeholder, replaced below
            else
                bar[i + 1] = '\x02';
        }
        bar[kBarCols + 1] = ']';
        bar[kBarCols + 2] = '\0';

        // Build display string with UTF-8 blocks
        std::string row_str;
        row_str.reserve(128);
        // Label, left-padded to 16 chars
        row_str += "  ";
        row_str += label;
        // Pad to 16
        size_t label_len = std::char_traits<char>::length(label);
        for (size_t i = label_len; i < 16; ++i) row_str += ' ';
        // Bar
        row_str += '[';
        for (int i = 0; i < kBarCols; ++i) {
            if (i < filled) {
                // U+2588 FULL BLOCK
                row_str += "\xe2\x96\x88";
            } else {
                // U+2591 LIGHT SHADE
                row_str += "\xe2\x96\x91";
            }
        }
        row_str += ']';
        row_str += "  ";
        row_str += right_text;

        float r = kGreenR, g = kGreenG, b = kGreenB;
        if (status == 2) { r = kDimR; g = kDimG; b = kDimB; }   // dim when done
        if (status == 3) { r = 0.9f; g = 0.5f; b = 0.05f; }    // orange-ish for error

        term_renderer_.DrawTextFixed(kPadX, y, row_str, cell_w, r, g, b, kFullAlpha);
    };

    const auto draw_frame = [&]() {
        glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        BeginDraw();

        float y = kPadY;

        // Header
        term_renderer_.DrawTextFixed(kPadX, y, "  Loading AI models...", cell_w,
                                      kGreenR, kGreenG, kGreenB, kFullAlpha);
        y += static_cast<float>(line_h) * 1.5f;

        // LLM row (Amadeus built-in)
        draw_row(y, "LLM (Amadeus)",
                 amadeus_native_gguf_download_progress(),
                 amadeus_native_gguf_download_status());
        y += static_cast<float>(line_h) * 1.4f;

        // STT row (Whisper)
        draw_row(y, "STT (Whisper)",
                 amadeus_native_stt_download_progress(),
                 amadeus_native_stt_download_status());
        y += static_cast<float>(line_h) * 1.4f;

        // TTS row — lazy download, show cache status only
        {
            int tts_cached = amadeus_native_tts_model_cached();
            int tts_status = tts_cached ? 2 : 0;   // 2=Ready  0=Will download on first use
            draw_row(y, "TTS (Voice)",
                     tts_cached ? 100 : 0,
                     tts_status);
        }

        EndDraw();
    };

    // Poll until LLM and STT are either done (2) or errored (3), or already present.
    // TTS is lazy so we don't block on it.
    const auto both_ready = [&]() -> bool {
        int llm = amadeus_native_gguf_download_status();
        int stt = amadeus_native_stt_download_status();
        // status 0 means model was already present (we set it to 2 in preflight if present)
        // so only 1 (downloading) keeps us waiting.
        return llm != 1 && stt != 1;
    };

    while (glfwWindowShouldClose(window_) == GLFW_FALSE) {
        draw_frame();
        if (!SwapAndPoll()) return false;
        if (both_ready()) break;
    }

    if (glfwWindowShouldClose(window_)) return false;

    // Draw one final frame with completed state, then call into Rust to init services.
    draw_frame();
    if (!SwapAndPoll()) return false;

    // Final render showing "Ready" on all rows, hold briefly so the user sees it.
    {
        const double hold_until = glfwGetTime() + 0.6;
        while (glfwGetTime() < hold_until) {
            if (glfwWindowShouldClose(window_)) return false;
            draw_frame();
            if (!SwapAndPoll()) return false;
        }
    }

    // Initialize Rust services (TTS, STT, agent) now that models are present.
    amadeus_native_init_services();

    return glfwWindowShouldClose(window_) == GLFW_FALSE;
}

// ─── Phase 2: boot logo frames ────────────────────────────────────────────────

bool BootSequence::RunLogoPhase()
{
    const char* assets_env = std::getenv("AMADEUS_ASSETS_DIR");
    if (assets_env == nullptr || assets_env[0] == '\0')
        return true;  // no assets dir — skip gracefully

    const std::string boot_dir = std::string(assets_env) + "/boot/";

    struct FrameTex {
        GLuint tex = 0;
        int    w   = 0;
        int    h   = 0;
    };

    // RAII cleanup of all uploaded textures
    struct FrameSet {
        std::vector<FrameTex> frames;
        ~FrameSet() {
            for (auto& f : frames) {
                if (f.tex != 0) glDeleteTextures(1, &f.tex);
            }
        }
    } frame_set;

    // Load numbered frames: 1.png, 2.png, … until file is missing.
    // Use stbi_load_from_memory because the stb_image TU in this project
    // is compiled with STBI_NO_STDIO, so the path-based stbi_load isn't available.
    constexpr int kMaxFrames = 39;
    for (int i = 1; i <= kMaxFrames; ++i) {
        const std::string path = boot_dir + std::to_string(i) + ".png";
        std::ifstream file(path, std::ios::binary | std::ios::ate);
        if (!file.is_open()) break;
        const auto file_size = file.tellg();
        file.seekg(0);
        std::vector<unsigned char> buf(static_cast<std::size_t>(file_size));
        file.read(reinterpret_cast<char*>(buf.data()), file_size);
        file.close();

        int w = 0, h = 0, channels = 0;
        unsigned char* pixels = stbi_load_from_memory(
            buf.data(), static_cast<int>(buf.size()), &w, &h, &channels, STBI_rgb_alpha);
        if (pixels == nullptr) break;

        FrameTex f;
        f.w = w;
        f.h = h;
        glGenTextures(1, &f.tex);
        glBindTexture(GL_TEXTURE_2D, f.tex);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
        glBindTexture(GL_TEXTURE_2D, 0);
        stbi_image_free(pixels);

        frame_set.frames.push_back(f);
    }

    const auto& frames = frame_set.frames;
    if (frames.empty()) return true;

    // Play the boot audio and get its actual duration for frame sync.
    const std::string audio_path = boot_dir + "boot.mp3";
    const unsigned int audio_ms  = amadeus_native_boot_audio_play(
        audio_path.c_str(), kFramePlaybackFallbackMs);

    const std::size_t n = frames.size();
    const double frame_dur = (static_cast<double>(audio_ms) / 1000.0) / static_cast<double>(n);

    // Fixed bounding box derived from the first frame so every frame renders
    // at the same physical size regardless of individual pixel dimensions.
    // kLogoScale is defined at the top of this file
    const float ref_scale = std::min(
        static_cast<float>(window_width_)  / static_cast<float>(frames[0].w),
        static_cast<float>(window_height_) / static_cast<float>(frames[0].h)) * kLogoScale;
    const float box_w = static_cast<float>(frames[0].w) * ref_scale;
    const float box_h = static_cast<float>(frames[0].h) * ref_scale;

    const auto render = [&](std::size_t idx) -> bool {
        if (glfwWindowShouldClose(window_)) return false;
        glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        BeginDraw();
        DrawImageTexture(frames[idx].tex, frames[idx].w, frames[idx].h, box_w, box_h);
        EndDraw();
        return SwapAndPoll();
    };

    // ── Animate all frames ────────────────────────────────────────────────────
    double frame_start = glfwGetTime();
    std::size_t cur = 0;

    while (cur < n - 1) {
        if (!render(cur)) return false;
        if (glfwGetTime() - frame_start >= frame_dur) {
            frame_start = glfwGetTime();
            ++cur;
        }
    }

    // ── Hold the last frame ───────────────────────────────────────────────────
    const double hold_until = glfwGetTime() + (kFinalHoldMs / 1000.0);
    while (glfwGetTime() < hold_until) {
        if (!render(n - 1)) return false;
    }

    return glfwWindowShouldClose(window_) == GLFW_FALSE;
}
