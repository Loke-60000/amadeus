#include <GL/glew.h>
#include <GLFW/glfw3.h>

#include <CubismFramework.hpp>
#include <Rendering/OpenGL/CubismRenderer_OpenGLES2.hpp>

#include <exception>
#include <filesystem>
#include <memory>
#include <stdexcept>
#include <string>
#include <cstdlib>

#include "overlay.hpp"
#include "font_renderer.hpp"
#include "CubismUserModelExtend.hpp"
#include "LAppAllocator_Common.hpp"
#include "LAppDefine.hpp"
#include "LAppPal.hpp"
#include "MouseActionManager.hpp"
#include "stb_image.h"

namespace {

using CubismOpenGlRenderer =
    Live2D::Cubism::Framework::Rendering::CubismRenderer_OpenGLES2;

constexpr int kOverlayFontPixelSize = 28;

// ── Digital data shader — adapted from Shadertoy conventions ─────────────────

static const char* kBgVertSrc = R"GLSL(
#version 130
out vec2 vUV;
void main() {
    vUV         = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
    gl_Position = vec4(vUV * 2.0 - 1.0, 0.0, 1.0);
}
)GLSL";

static const char* kBgFragSrc = R"GLSL(
#version 130
uniform float uTime;
uniform vec2  uResolution;
in  vec2 vUV;
out vec4 fragColor;

// ── hash helpers ─────────────────────────────────────────────
float hash(vec2 p) {
    p = fract(p * vec2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}
float hash1(float n) { return fract(sin(n) * 43758.5453123); }

// ── "The choice of Steins;Gate" → 200 bits ───────────────────
const int kBits = 200;
const int kPhrase[25] = int[25](
    0x54,0x68,0x65,0x20,0x63,
    0x68,0x6F,0x69,0x63,0x65,
    0x20,0x6F,0x66,0x20,0x53,
    0x74,0x65,0x69,0x6E,0x73,
    0x3B,0x47,0x61,0x74,0x65
);
float phraseBit(float rawPos) {
    int p   = int(mod(rawPos, float(kBits)));
    int b   = p / 8;
    int bit = 7 - (p - b * 8);
    return float((kPhrase[b] >> bit) & 1);
}

// ── blocky digit ─────────────────────────────────────────────
float drawDigit(vec2 uv, float val) {
    if (uv.x < 0.15 || uv.x > 0.85 || uv.y < 0.08 || uv.y > 0.92) return 0.0;
    float d = 0.0;
    if (val < 0.5) {
        if ((uv.x > 0.15 && uv.x < 0.38) || (uv.x > 0.62 && uv.x < 0.85)) d = 1.0;
        if ((uv.y > 0.08 && uv.y < 0.26) || (uv.y > 0.74 && uv.y < 0.92)) d = 1.0;
    } else {
        if (uv.x > 0.40 && uv.x < 0.60) d = 1.0;
        if (uv.x > 0.25 && uv.x < 0.48 && uv.y > 0.74 && uv.y < 0.92) d = 1.0;
        if (uv.x > 0.22 && uv.x < 0.78 && uv.y > 0.08 && uv.y < 0.26) d = 1.0;
    }
    return d;
}

// ── binary layer ─────────────────────────────────────────────
// depthScale: 0=far(dim,small,fast), 1=near(brighter,large,slow)
vec3 renderBinaryLayer(vec2 uv, float scale, float speedBase,
                        float alphaBase, float seed) {
    vec2 scaledUV = uv * scale;
    vec2 gridId   = floor(scaledUV);

    float rowSpeed = (hash1(gridId.y + seed) * 0.5 + 0.5) * speedBase;
    float rowDir   = (hash1(gridId.y + seed + 10.0) > 0.5) ? 1.0 : -1.0;

    vec2 aUV   = scaledUV + vec2(uTime * rowSpeed * rowDir, 0.0);
    vec2 aId   = floor(aUV);
    vec2 aFrac = fract(aUV);

    if (hash(aId + seed * 1.5) > 0.58) return vec3(0.0);

    float rawIdx = aId.x + aId.y * 7.0 + seed * 31.0
                   + floor(uTime * 0.20 + hash(aId) * 8.0);
    float bitVal = phraseBit(rawIdx);
    float digit  = drawDigit(aFrac, bitVal);
    float flick  = hash(aId + floor(uTime * 2.5) * 0.1) * 0.25 + 0.75;

    // Rare bright cell for depth pop — kept subtle
    float pop = step(0.92, hash(aId + seed * 3.1)) * 0.8 + 1.0;

    return vec3(0.06, 0.48, 0.58) * digit * alphaBase * flick * pop;
}

// ── grid ─────────────────────────────────────────────────────
vec3 renderGrid(vec2 uv) {
    vec2 gv  = fract(uv * 4.0);
    vec2 id  = floor(uv * 4.0);
    vec3 col = vec3(0.0);

    float lx = smoothstep(0.016, 0.0, abs(gv.x - 0.5));
    float ly = smoothstep(0.016, 0.0, abs(gv.y - 0.5));
    // Grid lines much dimmer — they fade into background
    col += vec3(0.02, 0.12, 0.16) * (lx + ly) * 0.28;

    // Only ~20% of intersections get a node glow, and it's subtle
    if (hash(id) > 0.80) {
        float d     = length(abs(gv - 0.5));
        float pulse = sin(uTime * 1.4 + hash1(id.x * 10.0 + id.y) * 6.28) * 0.5 + 0.5;
        col += vec3(0.10, 0.60, 0.75) * (0.004 / (d * d + 0.006)) * pulse * 0.30;
    }
    return col;
}

// ── particles ────────────────────────────────────────────────
// Fixes: aspect-correct distance (rounds), much smaller bloom.
vec3 renderParticles(vec2 uv, float aspect) {
    vec3 col = vec3(0.0);
    for (int i = 0; i < 12; i++) {
        float fi = float(i);
        float ox = hash1(fi * 3.7193);
        float oy = hash1(fi * 7.1341 + 1.0);
        float px = ox + 0.030 * sin(uTime * (0.20 + hash1(fi) * 0.15) + fi * 2.4);
        float py = oy + 0.030 * cos(uTime * (0.17 + hash1(fi + 5.0) * 0.13) + fi * 1.8);

        float pulse = 0.45 + 0.55 * sin(uTime * (0.9 + hash1(fi * 2.3) * 0.6) + fi * 3.14);

        // Aspect-corrected → perfectly round
        vec2 diff = (uv - vec2(px, py)) * vec2(aspect, 1.0);
        float d   = length(diff);

        // Tight core + narrow halo — no giant blob
        float core = 0.000018 / (d * d + 0.000010);
        float halo = 0.000180 / (d * d + 0.000350);
        col += vec3(0.30, 0.90, 1.00) * (core + halo) * pulse;
    }
    return col;
}

void main() {
    vec2  uv     = vUV;
    float aspect = uResolution.x / uResolution.y;
    vec2  p      = (uv - 0.5) * vec2(aspect, 1.0);

    // Very dark base — lets the model stand out clearly
    vec3 col = vec3(0.004, 0.014, 0.026);

    // Depth layers: far (dim, dense, fast) → near (brighter, sparse, slow)
    col += renderBinaryLayer(p, 52.0, 5.0, 0.13, 11.0);  // far
    col += renderBinaryLayer(p, 24.0, 2.8, 0.22, 22.0);  // mid
    col += renderBinaryLayer(p, 11.0, 1.2, 0.34, 33.0);  // near

    col += renderGrid(p);
    col += renderParticles(uv, aspect);

    // Subtle teal push (keep it muted)
    col *= vec3(0.82, 1.00, 1.03);

    // ── lighting: bright center, dark surround ────────────────
    // Gaussian spotlight peaks at center, falls off quickly.
    float r        = length(uv - 0.5);
    float spotlight = exp(-r * r * 7.0);  // tight Gaussian
    // Center gets ~full brightness, ring around it is crushed to ~12%
    col *= mix(0.12, 1.0, spotlight);

    // Secondary hard vignette crushes corners to near-black
    float vign = 1.0 - smoothstep(0.28, 0.90, r);
    col = mix(vec3(0.003, 0.010, 0.018), col, vign);

    fragColor = vec4(clamp(col, 0.0, 1.0), 1.0);
}
)GLSL";

// ── BackgroundRenderer state ─────────────────────────────────────────────────

std::string g_last_error;
LAppAllocator_Common g_allocator;
Live2D::Cubism::Framework::CubismFramework::Option g_cubism_option;
GLFWwindow* g_window = nullptr;
std::unique_ptr<CubismUserModelExtend> g_model;
std::unique_ptr<AmadeusOverlay> g_overlay;
AmadeusTextRenderer g_text_renderer;
bool g_glfw_initialized = false;
bool g_framework_started = false;
int g_window_width = LAppDefine::RenderTargetWidth;
int g_window_height = LAppDefine::RenderTargetHeight;

// ── BackgroundRenderer ───────────────────────────────────────────────────────

struct BackgroundRenderer {
    GLuint program     = 0;
    GLuint vao         = 0;
    GLint  uTime       = -1;
    GLint  uResolution = -1;
    bool   ready       = false;
};

static BackgroundRenderer g_bg;

static GLuint CompileShader(GLenum type, const char* src) {
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &src, nullptr);
    glCompileShader(s);
    GLint ok = 0;
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        glDeleteShader(s);
        return 0;
    }
    return s;
}

static void InitBackgroundRenderer() {
    GLuint vs = CompileShader(GL_VERTEX_SHADER,   kBgVertSrc);
    GLuint fs = CompileShader(GL_FRAGMENT_SHADER, kBgFragSrc);
    if (!vs || !fs) {
        if (vs) glDeleteShader(vs);
        if (fs) glDeleteShader(fs);
        return;
    }
    GLuint prog = glCreateProgram();
    glAttachShader(prog, vs);
    glAttachShader(prog, fs);
    glLinkProgram(prog);
    glDeleteShader(vs);
    glDeleteShader(fs);
    GLint ok = 0;
    glGetProgramiv(prog, GL_LINK_STATUS, &ok);
    if (!ok) {
        glDeleteProgram(prog);
        return;
    }
    g_bg.program     = prog;
    g_bg.uTime       = glGetUniformLocation(prog, "uTime");
    g_bg.uResolution = glGetUniformLocation(prog, "uResolution");
    glGenVertexArrays(1, &g_bg.vao);
    g_bg.ready = true;
}

static void RenderBackground() {
    if (!g_bg.ready) return;

    GLint prevProg = 0;
    glGetIntegerv(GL_CURRENT_PROGRAM, &prevProg);

    glUseProgram(g_bg.program);
    if (g_bg.uTime >= 0)
        glUniform1f(g_bg.uTime, static_cast<float>(glfwGetTime()));
    if (g_bg.uResolution >= 0)
        glUniform2f(g_bg.uResolution,
                    static_cast<float>(g_window_width),
                    static_cast<float>(g_window_height));

    glBindVertexArray(g_bg.vao);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    glBindVertexArray(0);

    glUseProgram(static_cast<GLuint>(prevProg));
}

static void CleanupBackgroundRenderer() {
    if (g_bg.program) { glDeleteProgram(g_bg.program); g_bg.program = 0; }
    if (g_bg.vao)     { glDeleteVertexArrays(1, &g_bg.vao); g_bg.vao = 0; }
    g_bg.ready = false;
}

// ─────────────────────────────────────────────────────────────────────────────

std::string EnsureTrailingSlash(std::string path)
{
    if (!path.empty() && path.back() != '/')
    {
        path.push_back('/');
    }

    return path;
}

std::string DescribeCurrentException()
{
    try
    {
        throw;
    }
    catch (const std::exception& error)
    {
        return error.what();
    }
    catch (...)
    {
        return "unknown exception";
    }
}

void OnKeyCallback(GLFWwindow* window, int key, int scancode, int action, int mods)
{
    (void)scancode;

    if (g_overlay)
    {
        g_overlay->HandleKey(window, key, action, mods);
    }
}

void OnCharCallback(GLFWwindow* window, unsigned int codepoint)
{
    (void)window;

    if (g_overlay)
    {
        g_overlay->HandleChar(codepoint);
    }
}

void CleanupModel()
{
    if (!g_model)
    {
        return;
    }

    if (g_model->GetRenderer<CubismOpenGlRenderer>() != nullptr)
    {
        g_model->DeleteRenderer();
    }

    g_model.reset();
}

void CleanupWindow()
{
    MouseActionManager::ReleaseInstance();

    if (g_window != nullptr)
    {
        CleanupBackgroundRenderer();
        if (g_overlay) {
            g_overlay->Shutdown();
        }
        g_text_renderer.Shutdown();
        glfwDestroyWindow(g_window);
        g_window = nullptr;
    }

    if (g_glfw_initialized)
    {
        glfwTerminate();
        g_glfw_initialized = false;
    }
}

void CleanupFramework()
{
    if (!g_framework_started)
    {
        return;
    }

    if (Live2D::Cubism::Framework::CubismFramework::IsInitialized())
    {
        Live2D::Cubism::Framework::CubismFramework::Dispose();
    }

    Live2D::Cubism::Framework::CubismFramework::CleanUp();
    g_framework_started = false;
}

void CleanupAll()
{
    CleanupModel();
    CleanupWindow();
    CleanupFramework();
}

void InitializeFramework()
{
    if (g_framework_started)
    {
        return;
    }

    g_cubism_option.LogFunction = LAppPal::PrintMessage;
    g_cubism_option.LoggingLevel =
        Live2D::Cubism::Framework::CubismFramework::Option::LogLevel_Verbose;
    g_cubism_option.LoadFileFunction = LAppPal::LoadFileAsBytes;
    g_cubism_option.ReleaseBytesFunction = LAppPal::ReleaseBytes;

    if (!Live2D::Cubism::Framework::CubismFramework::StartUp(
            &g_allocator,
            &g_cubism_option))
    {
        throw std::runtime_error("failed to start the Cubism framework");
    }

    Live2D::Cubism::Framework::CubismFramework::Initialize();
    g_framework_started = true;
}

void ConfigureOpenGlState()
{
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
}

void InitializeWindow(const char* window_title)
{
    const char* session_type = std::getenv("XDG_SESSION_TYPE");
    const bool is_wayland_session =
        session_type != nullptr && std::string(session_type) == "wayland";
    const bool has_x11_display = std::getenv("DISPLAY") != nullptr;

    if (is_wayland_session && has_x11_display)
    {
        glfwInitHint(GLFW_PLATFORM, GLFW_PLATFORM_X11);
    }

    if (glfwInit() == GLFW_FALSE)
    {
        throw std::runtime_error("failed to initialize GLFW");
    }
    g_glfw_initialized = true;

    glfwWindowHint(GLFW_CLIENT_API, GLFW_OPENGL_API);

    g_window = glfwCreateWindow(
        LAppDefine::RenderTargetWidth,
        LAppDefine::RenderTargetHeight,
        window_title,
        NULL,
        NULL);
    if (g_window == nullptr)
    {
        throw std::runtime_error("failed to create the native Cubism window");
    }

    {
        Csm::csmSizeInt icon_file_size = 0;
        Csm::csmByte* icon_file_data = LAppPal::LoadFileAsBytes("logo.png", &icon_file_size);
        if (icon_file_data != nullptr)
        {
            int w, h, channels;
            unsigned char* pixels = stbi_load_from_memory(
                icon_file_data, icon_file_size, &w, &h, &channels, STBI_rgb_alpha);
            LAppPal::ReleaseBytes(icon_file_data);
            if (pixels != nullptr)
            {
                GLFWimage icon{w, h, pixels};
                glfwSetWindowIcon(g_window, 1, &icon);
                stbi_image_free(pixels);
            }
        }
    }

    glfwMakeContextCurrent(g_window);
    glfwSwapInterval(1);

    glewExperimental = GL_TRUE;
    const GLenum glew_status = glewInit();
    if (glew_status != GLEW_OK)
    {
        throw std::runtime_error(
            "failed to initialize GLEW: " +
            std::string(reinterpret_cast<const char*>(glewGetErrorString(glew_status))));
    }

    glGetError();
    ConfigureOpenGlState();

    glfwSetMouseButtonCallback(g_window, EventHandler::OnMouseCallBack);
    glfwSetCursorPosCallback(g_window, EventHandler::OnMouseCallBack);
    glfwSetKeyCallback(g_window, OnKeyCallback);
    glfwSetCharCallback(g_window, OnCharCallback);

    glfwGetWindowSize(g_window, &g_window_width, &g_window_height);
    glViewport(0, 0, g_window_width, g_window_height);

    MouseActionManager::GetInstance()->Initialize(g_window_width, g_window_height);
    g_overlay = std::make_unique<AmadeusOverlay>();
    if (!g_text_renderer.Initialize(kOverlayFontPixelSize)) {
        throw std::runtime_error("failed to initialize the scalable native overlay text renderer");
    }
    g_overlay->Initialize();
    InitBackgroundRenderer();
}

void LoadModel(const std::filesystem::path& model_json_path)
{
    if (!std::filesystem::exists(model_json_path))
    {
        throw std::runtime_error(
            "model file does not exist: " + model_json_path.string());
    }

    const std::filesystem::path model_directory = model_json_path.parent_path();
    const std::string model_directory_name =
        model_directory.filename().string().empty()
            ? model_json_path.stem().string()
            : model_directory.filename().string();
    const std::string current_model_directory =
        EnsureTrailingSlash(model_directory.string());
    const std::string model_file_name = model_json_path.filename().string();

    g_model = std::make_unique<CubismUserModelExtend>(
        model_directory_name,
        current_model_directory);
    g_model->LoadAssets(model_file_name.c_str());

    MouseActionManager::GetInstance()->SetUserModel(g_model.get());
}

void UpdateViewportIfNeeded()
{
    int width = 0;
    int height = 0;
    glfwGetWindowSize(g_window, &width, &height);

    if ((g_window_width != width || g_window_height != height) && width > 0 && height > 0)
    {
        MouseActionManager::GetInstance()->ViewInitialize(width, height);
        g_window_width = width;
        g_window_height = height;
        glViewport(0, 0, width, height);
        return;
    }

    glViewport(0, 0, g_window_width, g_window_height);
}

int RunEventLoop()
{
    while (glfwWindowShouldClose(g_window) == GLFW_FALSE)
    {
        UpdateViewportIfNeeded();
        LAppPal::UpdateTime();
        if (g_overlay) {
            g_overlay->Update();
        }

        glClearColor(0.01f, 0.03f, 0.05f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
        glClearDepth(1.0);
        ConfigureOpenGlState();
        RenderBackground();

        g_model->ModelOnUpdate(g_window);
        if (g_overlay) {
            g_overlay->Render(g_text_renderer, g_window_width, g_window_height);
        }

        glfwSwapBuffers(g_window);
        glfwPollEvents();
    }

    return 0;
}

}  // namespace

extern "C" int amadeus_cubism_viewer_run(
    const char* model_json_path,
    const char* window_title)
{
    g_last_error.clear();

    try
    {
        if (model_json_path == nullptr || *model_json_path == '\0')
        {
            throw std::runtime_error("missing model3.json path for the native Cubism viewer");
        }

        const char* resolved_window_title =
            (window_title != nullptr && *window_title != '\0')
                ? window_title
                : "Amadeus Live2D";

        InitializeFramework();
        InitializeWindow(resolved_window_title);
        LoadModel(std::filesystem::path(model_json_path));

        const int exit_code = RunEventLoop();
        CleanupAll();
        return exit_code;
    }
    catch (...)
    {
        g_last_error = DescribeCurrentException();
        CleanupAll();
        return 1;
    }
}

extern "C" const char* amadeus_cubism_viewer_last_error_message()
{
    return g_last_error.c_str();
}