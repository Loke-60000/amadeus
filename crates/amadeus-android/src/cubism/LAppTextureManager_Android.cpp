#include "LAppTextureManager_Android.hpp"
#include <GLES3/gl3.h>
#define STBI_NO_STDIO
#define STBI_ONLY_PNG
#define STB_IMAGE_IMPLEMENTATION
#include "stb_image.h"
#include "LAppPal.hpp"

LAppTextureManager_Android::LAppTextureManager_Android() : LAppTextureManager_Common() {}

LAppTextureManager_Android::~LAppTextureManager_Android()
{
    ReleaseTextures();
}

LAppTextureManager_Android::TextureInfo*
LAppTextureManager_Android::CreateTextureFromPngFile(std::string fileName)
{
    for (Csm::csmUint32 i = 0; i < _texturesInfo.GetSize(); i++) {
        if (_texturesInfo[i]->fileName == fileName)
            return _texturesInfo[i];
    }

    Csm::csmSizeInt size;
    unsigned char* address = LAppPal::LoadFileAsBytes(fileName, &size);
    if (!address) return nullptr;

    int width, height, channels;
    unsigned char* png = stbi_load_from_memory(
        address, static_cast<int>(size), &width, &height, &channels, STBI_rgb_alpha);
    LAppPal::ReleaseBytes(address);
    if (!png) return nullptr;

    GLuint textureId;
    glGenTextures(1, &textureId);
    glBindTexture(GL_TEXTURE_2D, textureId);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, width, height, 0, GL_RGBA, GL_UNSIGNED_BYTE, png);
    glGenerateMipmap(GL_TEXTURE_2D);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR_MIPMAP_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glBindTexture(GL_TEXTURE_2D, 0);
    stbi_image_free(png);

    TextureInfo* info = new TextureInfo();
    info->fileName = fileName;
    info->width    = width;
    info->height   = height;
    info->id       = textureId;
    _texturesInfo.PushBack(info);
    return info;
}

void LAppTextureManager_Android::ReleaseTextures()
{
    for (Csm::csmUint32 i = 0; i < _texturesInfo.GetSize(); i++) {
        glDeleteTextures(1, &(_texturesInfo[i]->id));
    }
    ReleaseTexturesInfo();
}

void LAppTextureManager_Android::ReleaseTexture(Csm::csmUint32 textureId)
{
    for (Csm::csmUint32 i = 0; i < _texturesInfo.GetSize(); i++) {
        if (_texturesInfo[i]->id != textureId) continue;
        glDeleteTextures(1, &(_texturesInfo[i]->id));
        delete _texturesInfo[i];
        _texturesInfo.Remove(i);
        break;
    }
}

void LAppTextureManager_Android::ReleaseTexture(std::string fileName)
{
    for (Csm::csmUint32 i = 0; i < _texturesInfo.GetSize(); i++) {
        if (_texturesInfo[i]->fileName != fileName) continue;
        glDeleteTextures(1, &(_texturesInfo[i]->id));
        delete _texturesInfo[i];
        _texturesInfo.Remove(i);
        break;
    }
}
