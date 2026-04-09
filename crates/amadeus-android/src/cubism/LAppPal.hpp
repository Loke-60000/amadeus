#pragma once

#include <CubismFramework.hpp>
#include <cstdlib>
#include <string>

class LAppPal
{
public:
    static Csm::csmByte* LoadFileAsBytes(const std::string filePath, Csm::csmSizeInt* outSize);
    static void ReleaseBytes(Csm::csmByte* byteData);
    static Csm::csmFloat32 GetDeltaTime();
    static void UpdateTime();
    static void SetDeltaTime(Csm::csmFloat32 deltaSeconds);
    static void PrintLog(const Csm::csmChar* format, ...);
    static void PrintLogLn(const Csm::csmChar* format, ...);
    static void PrintMessage(const Csm::csmChar* message);
    static void PrintMessageLn(const Csm::csmChar* message);

private:
    static double s_currentFrame;
    static double s_lastFrame;
    static double s_deltaTime;
};
