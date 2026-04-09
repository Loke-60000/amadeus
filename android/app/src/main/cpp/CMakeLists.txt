cmake_minimum_required(VERSION 3.22.1)
project(amadeus_android)

set(WORKSPACE_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../../../../")
set(CRATE_ROOT     "${WORKSPACE_ROOT}/crates/amadeus-android")
set(CLIENT_CUBISM  "${WORKSPACE_ROOT}/crates/amadeus-client/src/cubism")
set(ANDROID_CUBISM "${CRATE_ROOT}/src/cubism")
set(FRAMEWORK_SRC  "${WORKSPACE_ROOT}/third_party/CubismNativeFramework/src")

set(CUBISM_CORE_DIR "$ENV{AMADEUS_CUBISM_CORE_DIR}"
    CACHE PATH "Path to Cubism Core (Core/ from SDK download)")

if(NOT CUBISM_CORE_DIR)
    if(EXISTS "${WORKSPACE_ROOT}/android/cubism-core/lib/android")
        set(CUBISM_CORE_DIR "${WORKSPACE_ROOT}/android/cubism-core")
    else()
        set(CUBISM_CORE_DIR "${WORKSPACE_ROOT}/ressource/CubismSdkForNative-5-r.4.1/Core")
    endif()
endif()

set(CORE_LIB "${CUBISM_CORE_DIR}/lib/android/${ANDROID_ABI}/libLive2DCubismCore.a")
if(NOT EXISTS "${CORE_LIB}")
    message(FATAL_ERROR
        "Cubism Core not found at ${CORE_LIB}. "
        "Set AMADEUS_CUBISM_CORE_DIR to the Core/ directory from the Live2D Native SDK download.")
endif()

file(GLOB_RECURSE FRAMEWORK_SOURCES
    "${FRAMEWORK_SRC}/*.cpp"
)
# Remove platform-specific renderers that don't compile on Android
list(FILTER FRAMEWORK_SOURCES EXCLUDE REGEX ".*/Rendering/D3D[0-9]+/.*")
list(FILTER FRAMEWORK_SOURCES EXCLUDE REGEX ".*/Rendering/Metal/.*")
list(FILTER FRAMEWORK_SOURCES EXCLUDE REGEX ".*/Rendering/Vulkan/.*")

set(SHARED_CUBISM_SOURCES
    "${CLIENT_CUBISM}/CubismSampleViewMatrix_Common.cpp"
    "${CLIENT_CUBISM}/LAppAllocator_Common.cpp"
    "${CLIENT_CUBISM}/LAppDefine.cpp"
    "${CLIENT_CUBISM}/LAppModel_Common.cpp"
    "${CLIENT_CUBISM}/LAppTextureManager_Common.cpp"
    "${CLIENT_CUBISM}/MouseActionManager_Common.cpp"
    "${CLIENT_CUBISM}/TouchManager_Common.cpp"
    # CubismUserModelExtend.cpp is desktop-only (GLEW/GLFW); Android uses CubismModelAndroid.cpp
)

set(ANDROID_SOURCES
    "${ANDROID_CUBISM}/CubismModelAndroid.cpp"
    "${ANDROID_CUBISM}/LAppPal.cpp"
    "${ANDROID_CUBISM}/LAppTextureManager_Android.cpp"
    "${ANDROID_CUBISM}/cubism_bridge_android.cpp"
    "${ANDROID_CUBISM}/aaudio_player.c"
)

add_library(amadeus_android SHARED
    ${FRAMEWORK_SOURCES}
    ${SHARED_CUBISM_SOURCES}
    ${ANDROID_SOURCES}
)

target_include_directories(amadeus_android PRIVATE
    "${ANDROID_CUBISM}"
    "${CLIENT_CUBISM}"
    "${FRAMEWORK_SRC}"
    "${CUBISM_CORE_DIR}/include"
)

target_compile_definitions(amadeus_android PRIVATE
    CSM_TARGET_ANDROID_ES2
)

# C++ flags for all C++ sources
target_compile_options(amadeus_android PRIVATE
    $<$<COMPILE_LANGUAGE:CXX>:-std=c++17>
    -Wno-deprecated-declarations
    -Wno-missing-field-initializers
    -Wno-unused-parameter
)

add_library(Live2DCubismCore STATIC IMPORTED)
set_target_properties(Live2DCubismCore PROPERTIES
    IMPORTED_LOCATION "${CORE_LIB}"
)

find_library(EGL_LIB EGL)
find_library(GLES3_LIB GLESv3)
find_library(ANDROID_LIB android)
find_library(LOG_LIB log)
find_library(AAUDIO_LIB aaudio)

target_link_libraries(amadeus_android
    Live2DCubismCore
    ${EGL_LIB}
    ${GLES3_LIB}
    ${ANDROID_LIB}
    ${LOG_LIB}
    ${AAUDIO_LIB}
)
