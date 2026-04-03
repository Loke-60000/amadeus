#include "amadeus_text_renderer.hpp"

#include <GL/glew.h>
#include <fontconfig/fontconfig.h>

#include <algorithm>
#include <cstdint>
#include <filesystem>
#include <string>
#include <unordered_map>
#include <vector>

#include <ft2build.h>
#include FT_FREETYPE_H

namespace {

constexpr unsigned char kAsciiPreloadFirstGlyph = 32;
constexpr unsigned char kAsciiPreloadLastGlyph = 126;
constexpr const char* kBundledCjkFontRelativePath = "fonts/NotoSansCJK-Regular.ttc";

struct GlyphTexture {
    GLuint texture = 0;
    int width = 0;
    int height = 0;
    int bearing_x = 0;
    int bearing_y = 0;
    int advance = 0;
};

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

std::string FindBundledFontPath() {
    const std::filesystem::path candidate = std::filesystem::path(kBundledCjkFontRelativePath);
    if (std::filesystem::exists(candidate)) {
        return candidate.string();
    }

    return std::string();
}

std::string FindMonospaceFontPath() {
    if (FcInit() == FcFalse) {
        return std::string();
    }

    FcPattern* pattern = FcPatternCreate();
    if (pattern == nullptr) {
        return std::string();
    }

    FcPatternAddString(pattern, FC_FAMILY, reinterpret_cast<const FcChar8*>("monospace"));
    FcPatternAddInteger(pattern, FC_WEIGHT, FC_WEIGHT_BOLD);
    FcPatternAddBool(pattern, FC_SCALABLE, FcTrue);
    FcConfigSubstitute(nullptr, pattern, FcMatchPattern);
    FcDefaultSubstitute(pattern);

    FcResult result = FcResultNoMatch;
    FcPattern* match = FcFontMatch(nullptr, pattern, &result);
    FcPatternDestroy(pattern);

    if (match == nullptr) {
        return std::string();
    }

    FcChar8* file = nullptr;
    std::string path;
    if (FcPatternGetString(match, FC_FILE, 0, &file) == FcResultMatch && file != nullptr) {
        path = reinterpret_cast<const char*>(file);
    }

    FcPatternDestroy(match);
    return path;
}

}  // namespace

struct AmadeusTextRenderer::Impl {
    FT_Library library = nullptr;
    FT_Face face = nullptr;
    std::unordered_map<std::uint32_t, GlyphTexture> glyphs;
    int line_height = 0;
    int baseline = 0;
    int char_width = 0;
    bool ready = false;
};

namespace {

void DeleteGlyphTexture(GlyphTexture* glyph) {
    if (glyph == nullptr) {
        return;
    }

    if (glyph->texture != 0) {
        glDeleteTextures(1, &glyph->texture);
        glyph->texture = 0;
    }
    glyph->width = 0;
    glyph->height = 0;
    glyph->bearing_x = 0;
    glyph->bearing_y = 0;
    glyph->advance = 0;
}

}  // namespace

AmadeusTextRenderer::AmadeusTextRenderer()
    : impl_(std::make_unique<Impl>()) {}

AmadeusTextRenderer::~AmadeusTextRenderer() {
    Shutdown();
}

bool AmadeusTextRenderer::Initialize(int pixel_size) {
    Shutdown();

    const std::string bundled_font_path = FindBundledFontPath();
    const std::string fallback_font_path = FindMonospaceFontPath();
    std::vector<std::string> font_candidates;
    if (!bundled_font_path.empty()) {
        font_candidates.push_back(bundled_font_path);
    }
    if (!fallback_font_path.empty() && fallback_font_path != bundled_font_path) {
        font_candidates.push_back(fallback_font_path);
    }

    if (font_candidates.empty()) {
        return false;
    }

    if (FT_Init_FreeType(&impl_->library) != 0) {
        return false;
    }

    bool face_loaded = false;
    for (const std::string& font_path : font_candidates) {
        if (FT_New_Face(impl_->library, font_path.c_str(), 0, &impl_->face) == 0) {
            face_loaded = true;
            break;
        }
    }

    if (!face_loaded) {
        Shutdown();
        return false;
    }

    if (FT_Set_Pixel_Sizes(impl_->face, 0, static_cast<FT_UInt>(pixel_size)) != 0) {
        Shutdown();
        return false;
    }

    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);

    const auto load_glyph = [this](std::uint32_t codepoint) -> GlyphTexture* {
        if (!impl_ || impl_->face == nullptr) {
            return nullptr;
        }

        if (auto existing = impl_->glyphs.find(codepoint); existing != impl_->glyphs.end()) {
            return &existing->second;
        }

        GlyphTexture glyph;
        if (FT_Load_Char(impl_->face, static_cast<FT_ULong>(codepoint), FT_LOAD_RENDER) == 0) {
            const FT_GlyphSlot slot = impl_->face->glyph;
            glyph.width = static_cast<int>(slot->bitmap.width);
            glyph.height = static_cast<int>(slot->bitmap.rows);
            glyph.bearing_x = slot->bitmap_left;
            glyph.bearing_y = slot->bitmap_top;
            glyph.advance = static_cast<int>(slot->advance.x >> 6);

            if (glyph.width > 0 && glyph.height > 0 && slot->bitmap.buffer != nullptr) {
                glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
                glGenTextures(1, &glyph.texture);
                glBindTexture(GL_TEXTURE_2D, glyph.texture);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
                glTexImage2D(
                    GL_TEXTURE_2D,
                    0,
                    GL_ALPHA,
                    glyph.width,
                    glyph.height,
                    0,
                    GL_ALPHA,
                    GL_UNSIGNED_BYTE,
                    slot->bitmap.buffer);
            }
        }

        const auto [inserted, _] = impl_->glyphs.emplace(codepoint, glyph);
        return &inserted->second;
    };

    int max_advance = 0;
    int total_advance = 0;
    int measured_glyphs = 0;
    for (unsigned char codepoint = kAsciiPreloadFirstGlyph;
         codepoint <= kAsciiPreloadLastGlyph;
         ++codepoint) {
        if (GlyphTexture* glyph = load_glyph(codepoint)) {
            max_advance = std::max(max_advance, glyph->advance);
            if (glyph->advance > 0) {
                total_advance += glyph->advance;
                ++measured_glyphs;
            }
        }
    }

    glBindTexture(GL_TEXTURE_2D, 0);

    const int font_height = static_cast<int>(impl_->face->size->metrics.height >> 6);
    const int average_advance = measured_glyphs > 0 ? total_advance / measured_glyphs : max_advance;

    impl_->line_height = std::max(pixel_size + 4, font_height + 2);
    impl_->baseline = std::max(
        pixel_size,
        static_cast<int>(impl_->face->size->metrics.ascender >> 6));
    impl_->char_width = std::max(
        pixel_size / 2,
        average_advance > 0 ? average_advance + 1 : max_advance + 1);
    impl_->ready = true;
    return true;
}

void AmadeusTextRenderer::Shutdown() {
    if (!impl_) {
        return;
    }

    for (auto& [_, glyph] : impl_->glyphs) {
        DeleteGlyphTexture(&glyph);
    }
    impl_->glyphs.clear();

    if (impl_->face != nullptr) {
        FT_Done_Face(impl_->face);
        impl_->face = nullptr;
    }
    if (impl_->library != nullptr) {
        FT_Done_FreeType(impl_->library);
        impl_->library = nullptr;
    }

    impl_->line_height = 0;
    impl_->baseline = 0;
    impl_->char_width = 0;
    impl_->ready = false;
}

bool AmadeusTextRenderer::IsReady() const {
    return impl_ && impl_->ready;
}

int AmadeusTextRenderer::line_height() const {
    return impl_ ? impl_->line_height : 0;
}

int AmadeusTextRenderer::baseline() const {
    return impl_ ? impl_->baseline : 0;
}

int AmadeusTextRenderer::char_width() const {
    return impl_ ? impl_->char_width : 0;
}

int AmadeusTextRenderer::CodepointAdvance(std::uint32_t codepoint) const {
    if (!impl_ || !impl_->ready || impl_->face == nullptr) {
        return 0;
    }

    const auto load_glyph = [this](std::uint32_t value) -> const GlyphTexture* {
        if (!impl_ || impl_->face == nullptr) {
            return nullptr;
        }

        if (auto existing = impl_->glyphs.find(value); existing != impl_->glyphs.end()) {
            return &existing->second;
        }

        GlyphTexture glyph;
        if (FT_Load_Char(impl_->face, static_cast<FT_ULong>(value), FT_LOAD_RENDER) == 0) {
            const FT_GlyphSlot slot = impl_->face->glyph;
            glyph.width = static_cast<int>(slot->bitmap.width);
            glyph.height = static_cast<int>(slot->bitmap.rows);
            glyph.bearing_x = slot->bitmap_left;
            glyph.bearing_y = slot->bitmap_top;
            glyph.advance = static_cast<int>(slot->advance.x >> 6);

            if (glyph.width > 0 && glyph.height > 0 && slot->bitmap.buffer != nullptr) {
                glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
                glGenTextures(1, &glyph.texture);
                glBindTexture(GL_TEXTURE_2D, glyph.texture);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
                glTexImage2D(
                    GL_TEXTURE_2D,
                    0,
                    GL_ALPHA,
                    glyph.width,
                    glyph.height,
                    0,
                    GL_ALPHA,
                    GL_UNSIGNED_BYTE,
                    slot->bitmap.buffer);
            }
        }

        const auto [inserted, _] = impl_->glyphs.emplace(value, glyph);
        return &inserted->second;
    };

    const GlyphTexture* glyph = load_glyph(codepoint);
    if (glyph == nullptr) {
        return impl_->char_width;
    }

    return glyph->advance > 0 ? glyph->advance : impl_->char_width;
}

int AmadeusTextRenderer::MeasureTextWidth(const std::string& text) const {
    if (text.empty()) {
        return 0;
    }

    int width = 0;
    std::size_t index = 0;
    std::uint32_t codepoint = 0;
    while (index < text.size()) {
        const std::size_t start = index;
        if (!DecodeUtf8Codepoint(text, &index, &codepoint)) {
            if (index == start) {
                ++index;
            }
            width += impl_ ? impl_->char_width : 0;
            continue;
        }

        width += CodepointAdvance(codepoint);
    }

    return width;
}

void AmadeusTextRenderer::DrawText(
    float x,
    float y,
    const std::string& text,
    float red,
    float green,
    float blue,
    float alpha) const {
    if (!impl_ || !impl_->ready || text.empty()) {
        return;
    }

    glEnable(GL_TEXTURE_2D);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);
    glColor4f(red, green, blue, alpha);

    const auto load_glyph = [this](std::uint32_t codepoint) -> const GlyphTexture* {
        if (!impl_ || impl_->face == nullptr) {
            return nullptr;
        }

        if (auto existing = impl_->glyphs.find(codepoint); existing != impl_->glyphs.end()) {
            return &existing->second;
        }

        GlyphTexture glyph;
        if (FT_Load_Char(impl_->face, static_cast<FT_ULong>(codepoint), FT_LOAD_RENDER) == 0) {
            const FT_GlyphSlot slot = impl_->face->glyph;
            glyph.width = static_cast<int>(slot->bitmap.width);
            glyph.height = static_cast<int>(slot->bitmap.rows);
            glyph.bearing_x = slot->bitmap_left;
            glyph.bearing_y = slot->bitmap_top;
            glyph.advance = static_cast<int>(slot->advance.x >> 6);

            if (glyph.width > 0 && glyph.height > 0 && slot->bitmap.buffer != nullptr) {
                glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
                glGenTextures(1, &glyph.texture);
                glBindTexture(GL_TEXTURE_2D, glyph.texture);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
                glTexImage2D(
                    GL_TEXTURE_2D,
                    0,
                    GL_ALPHA,
                    glyph.width,
                    glyph.height,
                    0,
                    GL_ALPHA,
                    GL_UNSIGNED_BYTE,
                    slot->bitmap.buffer);
            }
        }

        const auto [inserted, _] = impl_->glyphs.emplace(codepoint, glyph);
        return &inserted->second;
    };

    float cursor_x = x;
    const float baseline_y = y + static_cast<float>(impl_->baseline);
    std::size_t index = 0;
    while (index < text.size()) {
        std::uint32_t codepoint = 0;
        if (!DecodeUtf8Codepoint(text, &index, &codepoint)) {
            cursor_x += static_cast<float>(impl_->char_width);
            continue;
        }

        const GlyphTexture* glyph = load_glyph(codepoint);
        if (glyph != nullptr && glyph->texture != 0 && glyph->width > 0 && glyph->height > 0) {
            const float xpos = cursor_x + static_cast<float>(glyph->bearing_x);
            const float ypos = baseline_y - static_cast<float>(glyph->bearing_y);
            const float width = static_cast<float>(glyph->width);
            const float height = static_cast<float>(glyph->height);

            glBindTexture(GL_TEXTURE_2D, glyph->texture);
            glBegin(GL_QUADS);
            glTexCoord2f(0.0f, 0.0f);
            glVertex2f(xpos, ypos);
            glTexCoord2f(1.0f, 0.0f);
            glVertex2f(xpos + width, ypos);
            glTexCoord2f(1.0f, 1.0f);
            glVertex2f(xpos + width, ypos + height);
            glTexCoord2f(0.0f, 1.0f);
            glVertex2f(xpos, ypos + height);
            glEnd();
        }

        cursor_x += static_cast<float>(glyph != nullptr && glyph->advance > 0 ? glyph->advance : impl_->char_width);
    }

    glBindTexture(GL_TEXTURE_2D, 0);
    glDisable(GL_TEXTURE_2D);
}