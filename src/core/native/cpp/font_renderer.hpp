#pragma once

#include <cstdint>
#include <memory>
#include <string>

class AmadeusTextRenderer {
public:
    AmadeusTextRenderer();
    ~AmadeusTextRenderer();

    AmadeusTextRenderer(const AmadeusTextRenderer&) = delete;
    AmadeusTextRenderer& operator=(const AmadeusTextRenderer&) = delete;

    bool Initialize(int pixel_size);
    void Shutdown();

    bool IsReady() const;
    int line_height() const;
    int baseline() const;
    int char_width() const;
    int MeasureTextWidth(const std::string& text) const;
    int CodepointAdvance(std::uint32_t codepoint) const;

    void DrawText(
        float x,
        float y,
        const std::string& text,
        float red,
        float green,
        float blue,
        float alpha) const;

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};