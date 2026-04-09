#include "LAppPal.hpp"
#include <android/log.h>
#include <sys/stat.h>
#include <cstdio>
#include <cstdlib>
#include <cstdarg>
#include <ctime>
#include <fstream>
#include <Model/CubismMoc.hpp>

using namespace Csm;
using std::string;

double LAppPal::s_currentFrame = 0.0;
double LAppPal::s_lastFrame    = 0.0;
double LAppPal::s_deltaTime    = 0.0;

csmByte* LAppPal::LoadFileAsBytes(const string filePath, csmSizeInt* outSize)
{
    const char* path = filePath.c_str();
    struct stat statBuf;
    if (stat(path, &statBuf) != 0 || statBuf.st_size == 0)
        return nullptr;

    int size = static_cast<int>(statBuf.st_size);
    std::fstream file(path, std::ios::in | std::ios::binary);
    if (!file.is_open()) return nullptr;

    char* buf = new char[size];
    file.read(buf, size);
    file.close();

    *outSize = size;
    return reinterpret_cast<csmByte*>(buf);
}

void LAppPal::ReleaseBytes(csmByte* byteData)
{
    delete[] byteData;
}

csmFloat32 LAppPal::GetDeltaTime()
{
    return static_cast<csmFloat32>(s_deltaTime);
}

void LAppPal::UpdateTime()
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    s_currentFrame = static_cast<double>(ts.tv_sec) + static_cast<double>(ts.tv_nsec) * 1e-9;
    s_deltaTime    = s_currentFrame - s_lastFrame;
    s_lastFrame    = s_currentFrame;
}

void LAppPal::SetDeltaTime(csmFloat32 deltaSeconds)
{
    s_deltaTime = static_cast<double>(deltaSeconds);
}

void LAppPal::PrintLog(const csmChar* format, ...)
{
    va_list args;
    va_start(args, format);
    __android_log_vprint(ANDROID_LOG_DEBUG, "amadeus-cubism", format, args);
    va_end(args);
}

void LAppPal::PrintLogLn(const csmChar* format, ...)
{
    va_list args;
    va_start(args, format);
    __android_log_vprint(ANDROID_LOG_DEBUG, "amadeus-cubism", format, args);
    va_end(args);
}

void LAppPal::PrintMessage(const csmChar* message)
{
    __android_log_print(ANDROID_LOG_DEBUG, "amadeus-cubism", "%s", message);
}

void LAppPal::PrintMessageLn(const csmChar* message)
{
    __android_log_print(ANDROID_LOG_DEBUG, "amadeus-cubism", "%s", message);
}
