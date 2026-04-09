#pragma once

#include "LAppTextureManager_Common.hpp"

class LAppTextureManager_Android : public LAppTextureManager_Common
{
public:
    LAppTextureManager_Android();
    ~LAppTextureManager_Android();

    TextureInfo* CreateTextureFromPngFile(std::string fileName);
    void         ReleaseTextures();
    void         ReleaseTexture(Csm::csmUint32 textureId);
    void         ReleaseTexture(std::string fileName);
};
