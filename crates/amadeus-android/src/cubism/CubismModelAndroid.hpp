#pragma once

#include <string>
#include <atomic>

#include <CubismFramework.hpp>
#include <CubismModelSettingJson.hpp>

#include "LAppTextureManager_Common.hpp"
#include "LAppModel_Common.hpp"

class CubismModelAndroid : public LAppModel_Common
{
public:
    CubismModelAndroid(const std::string& modelDir, const std::string& modelDirName);
    ~CubismModelAndroid();

    void LoadAssets(const Csm::csmChar* fileName);
    void ModelOnUpdate(int width, int height);
    void SetExpression(const char* name);

private:
    void SetupModel();
    void SetupTextures();
    void ModelParamUpdate();
    void Draw(Csm::CubismMatrix44& matrix);

    Csm::CubismMotionQueueEntryHandle StartMotion(
        const Csm::csmChar* group,
        Csm::csmInt32 no,
        Csm::csmInt32 priority);

    void ReleaseModelSetting();
    void PreloadMotionGroup(const Csm::csmChar* group);

    std::string _modelDir;
    std::string _modelDirName;

    Csm::csmFloat32                _userTimeSeconds;
    Csm::CubismModelSettingJson*   _modelJson;

    Csm::csmVector<Csm::CubismIdHandle> _eyeBlinkIds;
    Csm::csmVector<Csm::CubismIdHandle> _lipSyncIds;
    Csm::csmMap<Csm::csmString, Csm::ACubismMotion*> _motions;
    Csm::csmMap<Csm::csmString, Csm::ACubismMotion*> _expressions;

    LAppTextureManager_Common* _textureManager;

    const Csm::CubismId* _idParamAngleX;
    const Csm::CubismId* _idParamAngleY;
    const Csm::CubismId* _idParamAngleZ;
    const Csm::CubismId* _idParamBodyAngleX;
    const Csm::CubismId* _idParamEyeBallX;
    const Csm::CubismId* _idParamEyeBallY;
};
