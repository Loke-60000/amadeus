#include <EGL/egl.h>
#include <GLES3/gl3.h>
#include <android/native_window.h>

#include <CubismFramework.hpp>
#include <Rendering/OpenGL/CubismRenderer_OpenGLES2.hpp>

#include <atomic>
#include <exception>
#include <memory>
#include <stdexcept>
#include <string>
#include <cstdlib>
#include <ctime>

#include "CubismModelAndroid.hpp"
#include "LAppAllocator_Common.hpp"
#include "LAppDefine.hpp"
#include "LAppPal.hpp"

namespace {

using CubismOpenGlRenderer =
    Live2D::Cubism::Framework::Rendering::CubismRenderer_OpenGLES2;

static const char* kBgVertSrc = R"GLSL(
#version 300 es
out vec2 vUV;
void main() {
    vUV         = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
    gl_Position = vec4(vUV * 2.0 - 1.0, 0.0, 1.0);
}
)GLSL";

static const char* kBgFragSrc = R"GLSL(
#version 300 es
precision mediump float;
uniform float uTime;
uniform vec2  uResolution;
in  vec2 vUV;
out vec4 fragColor;

float hash(vec2 p) {
    p = fract(p * vec2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}
float hash1(float n) { return fract(sin(n) * 43758.5453123); }

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

vec3 renderBlockLayer(vec2 uv, float scale, float speedBase,
                      float alphaBase, float seed) {
    const float slotSize = 10.0;

    vec2  unitUV = uv * scale;
    float slotY  = floor(unitUV.y / slotSize);

    float colGroup = floor(unitUV.x / (slotSize * 2.0));
    float speedKey = slotY * 7.3 + colGroup * 13.1 + seed;

    float rowSpeed = (hash1(speedKey) * 0.8 + 0.2) * speedBase;
    float rowDir   = (hash1(speedKey * 1.7 + 1.0) > 0.5) ? 1.0 : -1.0;

    float animX  = unitUV.x + uTime * rowSpeed * rowDir;
    float slotXf = animX / slotSize;
    vec2  slotId = vec2(floor(slotXf), slotY);

    if (hash(slotId + seed) < 0.05) return vec3(0.0);

    float fadePhase = hash(slotId + seed + 77.3) * 6.28318;
    float fadeSpeed = hash1(slotId.x * 6.1 + slotId.y * 3.7 + seed) * 0.25 + 0.05;
    float chunkFade = smoothstep(0.15, 0.6, sin(uTime * fadeSpeed + fadePhase) * 0.5 + 0.5);
    if (chunkFade < 0.001) return vec3(0.0);

    float blockW  = floor(hash(slotId + seed +  3.7) * 9.0 + 1.5);
    float blockH  = floor(hash(slotId + seed +  8.1) * 9.0 + 1.5);

    float offsetX = floor(hash(slotId + seed + 15.3) * (slotSize - blockW + 1.0));
    float offsetY = floor(hash(slotId + seed + 22.9) * (slotSize - blockH + 1.0));

    float posX = fract(slotXf) * slotSize;
    float posY = fract(unitUV.y / slotSize) * slotSize;

    if (posY < offsetY || posY >= offsetY + blockH) return vec3(0.0);

    float rowIdx = floor(posY) - offsetY;
    float rowW   = floor(hash(vec2(slotId.x * 3.1 + rowIdx, slotId.y * 2.7) + seed * 5.1) * blockW) + 1.0;
    if (posX < offsetX || posX >= offsetX + rowW) return vec3(0.0);

    vec2 cellId   = vec2(floor(animX), floor(unitUV.y));
    vec2 cellFrac = vec2(fract(animX),  fract(unitUV.y));

    float zoneX    = floor((posX - offsetX) / 3.0);
    float zoneY    = floor((posY - offsetY) / 3.0);
    float zoneSeed = hash(vec2(zoneX + slotId.x * 5.0, zoneY + slotId.y * 5.0) + seed * 3.0);

    float digit, alphaScale;

    if (zoneSeed > 0.62) {
        vec2 mId   = cellId * 2.0 + floor(cellFrac * 2.0);
        vec2 mFrac = fract(cellFrac * 2.0);
        float ph   = floor(uTime * 0.4 + hash(mId + seed) * 6.0);
        digit      = drawDigit(mFrac, step(0.5, hash(mId + seed * 2.0 + ph)));
        alphaScale = 1.5;
    } else {
        float phase = floor(uTime * 0.25 + hash(cellId + seed) * 6.0);
        digit       = drawDigit(cellFrac, step(0.5, hash(cellId + seed * 2.0 + phase)));
        alphaScale  = (zoneSeed > 0.28) ? 1.0 : 0.35;
    }

    float flick = hash(cellId + seed * 3.0 + floor(uTime * 2.0) * 0.1) * 0.25 + 0.75;
    float pop   = step(0.92, hash(cellId + seed * 4.1)) * 0.6 + 1.0;

    return vec3(0.06, 0.48, 0.58) * digit * alphaBase * alphaScale * flick * pop * chunkFade;
}

vec3 renderParticles(vec2 uv, float aspect) {
    vec3 col = vec3(0.0);
    for (int i = 0; i < 12; i++) {
        float fi = float(i);
        float ox = hash1(fi * 3.7193);
        float oy = hash1(fi * 7.1341 + 1.0);
        float px = ox + 0.030 * sin(uTime * (0.20 + hash1(fi) * 0.15) + fi * 2.4);
        float py = oy + 0.030 * cos(uTime * (0.17 + hash1(fi + 5.0) * 0.13) + fi * 1.8);

        float pulse = 0.45 + 0.55 * sin(uTime * (0.9 + hash1(fi * 2.3) * 0.6) + fi * 3.14);

        vec2  diff = (uv - vec2(px, py)) * vec2(aspect, 1.0);
        float d    = length(diff);

        float core = 0.000018 / (d * d + 0.000010);
        float halo = 0.000180 / (d * d + 0.000350);
        col += vec3(0.30, 0.90, 1.00) * (core + halo) * pulse;
    }
    return col;
}

vec3 renderGrid(vec2 uv) {
    vec2 gv  = fract(uv * 4.0);
    vec2 id  = floor(uv * 4.0);
    vec3 col = vec3(0.0);

    float lx = smoothstep(0.016, 0.0, abs(gv.x - 0.5));
    float ly = smoothstep(0.016, 0.0, abs(gv.y - 0.5));
    col += vec3(0.02, 0.12, 0.16) * (lx + ly) * 0.28;

    if (hash(id) > 0.80) {
        float d     = length(abs(gv - 0.5));
        float pulse = sin(uTime * 1.4 + hash1(id.x * 10.0 + id.y) * 6.28) * 0.5 + 0.5;
        col += vec3(0.10, 0.60, 0.75) * (0.004 / (d * d + 0.006)) * pulse * 0.30;
    }
    return col;
}

void main() {
    vec2  uv     = vUV;
    float aspect = uResolution.x / uResolution.y;
    vec2  p      = (uv - 0.5) * vec2(aspect, 1.0);

    vec3 col = vec3(0.004, 0.014, 0.026);

    col += renderBlockLayer(p, 120.0, 5.0, 0.13, 11.0);
    col += renderBlockLayer(p,  70.0, 2.8, 0.22, 22.0);
    col += renderBlockLayer(p,  35.0, 1.2, 0.38, 33.0);

    col += renderGrid(p);
    col += renderParticles(uv, aspect);

    col *= vec3(0.82, 1.00, 1.03);

    float r         = length(uv - 0.5);
    float spotlight = exp(-r * r * 7.0);
    col *= mix(0.12, 1.0, spotlight);

    float vign = 1.0 - smoothstep(0.28, 0.90, r);
    col = mix(vec3(0.003, 0.010, 0.018), col, vign);

    fragColor = vec4(clamp(col, 0.0, 1.0), 1.0);
}
)GLSL";

std::string   g_last_error;
LAppAllocator_Common g_allocator;
Live2D::Cubism::Framework::CubismFramework::Option g_cubism_option;
std::unique_ptr<CubismModelAndroid> g_model;
bool          g_framework_started = false;
int           g_window_width      = 0;
int           g_window_height     = 0;

EGLDisplay    g_egl_display = EGL_NO_DISPLAY;
EGLContext    g_egl_context = EGL_NO_CONTEXT;
EGLSurface    g_egl_surface = EGL_NO_SURFACE;

std::atomic<float> g_lip_sync_value{0.0f};

struct BackgroundRenderer {
    GLuint program     = 0;
    GLuint vao         = 0;
    GLint  uTime       = -1;
    GLint  uResolution = -1;
    bool   ready       = false;
};

static BackgroundRenderer g_bg;
static struct timespec g_start_time = {};

static float GetElapsedSeconds() {
    struct timespec now = {};
    clock_gettime(CLOCK_MONOTONIC, &now);
    float secs = static_cast<float>(now.tv_sec - g_start_time.tv_sec);
    secs += static_cast<float>(now.tv_nsec - g_start_time.tv_nsec) * 1e-9f;
    return secs;
}

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
    clock_gettime(CLOCK_MONOTONIC, &g_start_time);
}

static void RenderBackground() {
    if (!g_bg.ready) return;

    GLint prevProg = 0;
    glGetIntegerv(GL_CURRENT_PROGRAM, &prevProg);

    glUseProgram(g_bg.program);
    if (g_bg.uTime >= 0)
        glUniform1f(g_bg.uTime, GetElapsedSeconds());
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

static bool InitEgl(ANativeWindow* window) {
    g_egl_display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (g_egl_display == EGL_NO_DISPLAY) return false;

    if (!eglInitialize(g_egl_display, nullptr, nullptr)) return false;

    const EGLint attribs[] = {
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT,
        EGL_SURFACE_TYPE,    EGL_WINDOW_BIT,
        EGL_BLUE_SIZE,       8,
        EGL_GREEN_SIZE,      8,
        EGL_RED_SIZE,        8,
        EGL_ALPHA_SIZE,      8,
        EGL_DEPTH_SIZE,      16,
        EGL_NONE
    };

    EGLConfig config;
    EGLint    num_configs;
    if (!eglChooseConfig(g_egl_display, attribs, &config, 1, &num_configs) || num_configs < 1)
        return false;

    g_egl_surface = eglCreateWindowSurface(g_egl_display, config, window, nullptr);
    if (g_egl_surface == EGL_NO_SURFACE) return false;

    const EGLint ctx_attribs[] = {
        EGL_CONTEXT_CLIENT_VERSION, 3,
        EGL_NONE
    };
    g_egl_context = eglCreateContext(g_egl_display, config, EGL_NO_CONTEXT, ctx_attribs);
    if (g_egl_context == EGL_NO_CONTEXT) return false;

    if (!eglMakeCurrent(g_egl_display, g_egl_surface, g_egl_surface, g_egl_context))
        return false;

    eglQuerySurface(g_egl_display, g_egl_surface, EGL_WIDTH,  &g_window_width);
    eglQuerySurface(g_egl_display, g_egl_surface, EGL_HEIGHT, &g_window_height);

    return true;
}

static void CleanupEgl() {
    if (g_egl_display != EGL_NO_DISPLAY) {
        eglMakeCurrent(g_egl_display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
        if (g_egl_context != EGL_NO_CONTEXT) { eglDestroyContext(g_egl_display, g_egl_context); g_egl_context = EGL_NO_CONTEXT; }
        if (g_egl_surface != EGL_NO_SURFACE) { eglDestroySurface(g_egl_display, g_egl_surface); g_egl_surface = EGL_NO_SURFACE; }
        eglTerminate(g_egl_display);
        g_egl_display = EGL_NO_DISPLAY;
    }
}

static void InitializeFramework() {
    if (g_framework_started) return;

    g_cubism_option.LogFunction          = LAppPal::PrintMessage;
    g_cubism_option.LoggingLevel         = Live2D::Cubism::Framework::CubismFramework::Option::LogLevel_Verbose;
    g_cubism_option.LoadFileFunction     = LAppPal::LoadFileAsBytes;
    g_cubism_option.ReleaseBytesFunction = LAppPal::ReleaseBytes;

    if (!Live2D::Cubism::Framework::CubismFramework::StartUp(&g_allocator, &g_cubism_option))
        throw std::runtime_error("failed to start the Cubism framework");

    Live2D::Cubism::Framework::CubismFramework::Initialize();
    g_framework_started = true;
}

static void LoadModel(const std::string& model_json_path) {
    std::string dir  = model_json_path.substr(0, model_json_path.find_last_of('/') + 1);
    std::string file = model_json_path.substr(dir.size());
    std::string name = dir.empty() ? file : dir.substr(0, dir.size() - 1);
    if (name.find('/') != std::string::npos)
        name = name.substr(name.find_last_of('/') + 1);

    g_model = std::make_unique<CubismModelAndroid>(dir, name);
    g_model->LoadAssets(file.c_str());
}

static void ConfigureGlState() {
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
}

static void RenderFrame() {
    LAppPal::UpdateTime();

    glClearColor(0.01f, 0.03f, 0.05f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
    ConfigureGlState();
    RenderBackground();

    if (g_model) {
        g_model->ModelOnUpdate(g_window_width, g_window_height);
    }

    eglSwapBuffers(g_egl_display, g_egl_surface);
}

static void CleanupAll() {
    if (g_model) {
        if (g_model->GetRenderer<CubismOpenGlRenderer>() != nullptr)
            g_model->DeleteRenderer();
        g_model.reset();
    }
    CleanupBackgroundRenderer();
    CleanupEgl();
    if (g_framework_started) {
        if (Live2D::Cubism::Framework::CubismFramework::IsInitialized())
            Live2D::Cubism::Framework::CubismFramework::Dispose();
        Live2D::Cubism::Framework::CubismFramework::CleanUp();
        g_framework_started = false;
    }
}

std::string DescribeCurrentException() {
    try { throw; }
    catch (const std::exception& e) { return e.what(); }
    catch (...) { return "unknown exception"; }
}

}  // namespace

extern "C" float amadeus_native_lip_sync_value() {
    return g_lip_sync_value.load(std::memory_order_relaxed);
}

extern "C" int amadeus_cubism_android_init(
    const char* model_json_path,
    void*       native_window)
{
    g_last_error.clear();
    try {
        if (!model_json_path || *model_json_path == '\0')
            throw std::runtime_error("missing model3.json path");

        auto* window = reinterpret_cast<ANativeWindow*>(native_window);
        if (!window)
            throw std::runtime_error("null ANativeWindow passed to cubism android bridge");

        InitializeFramework();

        if (!InitEgl(window))
            throw std::runtime_error("EGL initialization failed");

        glViewport(0, 0, g_window_width, g_window_height);
        InitBackgroundRenderer();
        LoadModel(std::string(model_json_path));

        return 0;
    }
    catch (...) {
        g_last_error = DescribeCurrentException();
        CleanupAll();
        return 1;
    }
}

extern "C" int amadeus_cubism_android_render_frame() {
    if (g_egl_display == EGL_NO_DISPLAY || g_egl_surface == EGL_NO_SURFACE)
        return 1;
    try {
        RenderFrame();
        return 0;
    }
    catch (...) {
        g_last_error = DescribeCurrentException();
        return 1;
    }
}

extern "C" void amadeus_cubism_android_destroy() {
    CleanupAll();
}

extern "C" const char* amadeus_cubism_android_last_error_message() {
    return g_last_error.c_str();
}

extern "C" void amadeus_cubism_android_set_lip_sync(float value) {
    g_lip_sync_value.store(value, std::memory_order_relaxed);
}

extern "C" void amadeus_cubism_android_set_expression(const char* name) {
    if (g_model && name) {
        g_model->SetExpression(name);
    }
}
