#pragma once

#include <string>
#include <vector>

#include "font_renderer.hpp"

struct GLFWwindow;

class BootSequence {
public:
    BootSequence(
        GLFWwindow* window,
        int window_width,
        int window_height);

    ~BootSequence() = default;

    // Returns false if the window was closed mid-sequence
    bool Run();

private:
    bool RunTerminalPhase();
    bool RunModelLoadingPhase();
    bool RunLogoPhase();

    void BeginDraw() const;
    void EndDraw() const;
    // box_w/box_h define the fixed pixel rect all frames share; the texture is
    // letterboxed inside it and the whole rect is centered on screen.
    void DrawImageTexture(unsigned int tex, int img_w, int img_h,
                          float box_w, float box_h) const;
    bool SwapAndPoll();

    GLFWwindow*         window_;
    AmadeusTextRenderer term_renderer_;  // monospace, owned by this sequence
    int                 window_width_;
    int                 window_height_;

    // Terminal lines — edit this array to change the boot text
    static const std::vector<std::string> kTerminalLines;
};
