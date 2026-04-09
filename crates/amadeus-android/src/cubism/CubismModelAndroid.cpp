#include <GLES3/gl3.h>

#include <Utils/CubismString.hpp>
#include <Motion/CubismMotion.hpp>
#include <Physics/CubismPhysics.hpp>
#include <Effect/CubismEyeBlink.hpp>
#include <Effect/CubismBreath.hpp>
#include <CubismDefaultParameterId.hpp>
#include <Rendering/OpenGL/CubismRenderer_OpenGLES2.hpp>
#include <Motion/CubismMotionQueueEntry.hpp>
#include <Id/CubismIdManager.hpp>

#include "LAppPal.hpp"
#include "LAppDefine.hpp"
#include "LAppTextureManager_Common.hpp"
#include "LAppTextureManager_Android.hpp"

#include "CubismModelAndroid.hpp"
using namespace Live2D::Cubism::Framework;
using namespace DefaultParameterId;
using namespace LAppDefine;

extern "C" float amadeus_native_lip_sync_value();

static constexpr float kModelOffsetY = -0.3f;

CubismModelAndroid::CubismModelAndroid(const std::string& modelDir, const std::string& modelDirName)
    : LAppModel_Common()
    , _modelJson(nullptr)
    , _userTimeSeconds(0.0f)
    , _modelDir(modelDir)
    , _modelDirName(modelDirName)
    , _textureManager(new LAppTextureManager_Android())
{
    _idParamAngleX    = CubismFramework::GetIdManager()->GetId(ParamAngleX);
    _idParamAngleY    = CubismFramework::GetIdManager()->GetId(ParamAngleY);
    _idParamAngleZ    = CubismFramework::GetIdManager()->GetId(ParamAngleZ);
    _idParamBodyAngleX = CubismFramework::GetIdManager()->GetId(ParamBodyAngleX);
    _idParamEyeBallX  = CubismFramework::GetIdManager()->GetId(ParamEyeBallX);
    _idParamEyeBallY  = CubismFramework::GetIdManager()->GetId(ParamEyeBallY);
}

CubismModelAndroid::~CubismModelAndroid()
{
    ReleaseModelSetting();
    delete _textureManager;
}

void CubismModelAndroid::LoadAssets(const Csm::csmChar* fileName)
{
    csmSizeInt size;
    const csmString path = csmString(_modelDir.c_str()) + fileName;

    csmByte* buffer = CreateBuffer(path.GetRawString(), &size);
    _modelJson = new CubismModelSettingJson(buffer, size);
    DeleteBuffer(buffer, path.GetRawString());

    SetupModel();
}

void CubismModelAndroid::SetupModel()
{
    _updating    = true;
    _initialized = false;

    csmByte* buffer;
    csmSizeInt size;

    if (strcmp(_modelJson->GetModelFileName(), "")) {
        csmString path = csmString(_modelDir.c_str()) + _modelJson->GetModelFileName();
        buffer = CreateBuffer(path.GetRawString(), &size);
        LoadModel(buffer, size);
        DeleteBuffer(buffer, path.GetRawString());
    }

    if (_modelJson->GetExpressionCount() > 0) {
        for (csmInt32 i = 0; i < _modelJson->GetExpressionCount(); i++) {
            csmString name = _modelJson->GetExpressionName(i);
            csmString path = csmString(_modelDir.c_str()) + _modelJson->GetExpressionFileName(i);
            buffer = CreateBuffer(path.GetRawString(), &size);
            ACubismMotion* motion = LoadExpression(buffer, size, name.GetRawString());
            if (motion) {
                if (_expressions[name]) {
                    ACubismMotion::Delete(_expressions[name]);
                    _expressions[name] = nullptr;
                }
                _expressions[name] = motion;
            }
            DeleteBuffer(buffer, path.GetRawString());
        }
    }

    if (strcmp(_modelJson->GetPoseFileName(), "")) {
        csmString path = csmString(_modelDir.c_str()) + _modelJson->GetPoseFileName();
        buffer = CreateBuffer(path.GetRawString(), &size);
        LoadPose(buffer, size);
        DeleteBuffer(buffer, path.GetRawString());
    }

    if (strcmp(_modelJson->GetPhysicsFileName(), "")) {
        csmString path = csmString(_modelDir.c_str()) + _modelJson->GetPhysicsFileName();
        buffer = CreateBuffer(path.GetRawString(), &size);
        LoadPhysics(buffer, size);
        DeleteBuffer(buffer, path.GetRawString());
    }

    if (_modelJson->GetEyeBlinkParameterCount() > 0) {
        _eyeBlink = CubismEyeBlink::Create(_modelJson);
    }

    {
        _breath = CubismBreath::Create();
        csmVector<CubismBreath::BreathParameterData> params;
        params.PushBack(CubismBreath::BreathParameterData(CubismFramework::GetIdManager()->GetId(ParamAngleX),     0.0f, 15.0f,  6.5345f, 0.5f));
        params.PushBack(CubismBreath::BreathParameterData(CubismFramework::GetIdManager()->GetId(ParamAngleY),     0.0f,  8.0f,  3.5345f, 0.5f));
        params.PushBack(CubismBreath::BreathParameterData(CubismFramework::GetIdManager()->GetId(ParamAngleZ),     0.0f, 10.0f,  5.5345f, 0.5f));
        params.PushBack(CubismBreath::BreathParameterData(CubismFramework::GetIdManager()->GetId(ParamBodyAngleX), 0.0f,  4.0f, 15.5345f, 0.5f));
        params.PushBack(CubismBreath::BreathParameterData(CubismFramework::GetIdManager()->GetId(ParamBreath),     0.5f,  0.5f,  3.2345f, 0.5f));
        _breath->SetParameters(params);
    }

    for (csmInt32 i = 0; i < _modelJson->GetLipSyncParameterCount(); ++i) {
        _lipSyncIds.PushBack(_modelJson->GetLipSyncParameterId(i));
    }

    if (strcmp(_modelJson->GetUserDataFile(), "")) {
        csmString path = csmString(_modelDir.c_str()) + _modelJson->GetUserDataFile();
        buffer = CreateBuffer(path.GetRawString(), &size);
        LoadUserData(buffer, size);
        DeleteBuffer(buffer, path.GetRawString());
    }

    const csmInt32 eyeBlinkIdCount = _modelJson->GetEyeBlinkParameterCount();
    for (csmInt32 i = 0; i < eyeBlinkIdCount; ++i) {
        _eyeBlinkIds.PushBack(_modelJson->GetEyeBlinkParameterId(i));
    }

    PreloadMotionGroup(LAppDefine::MotionGroupIdle);

    SetupTextures();
    CreateRenderer();

    _updating    = false;
    _initialized = true;
}

void CubismModelAndroid::SetupTextures()
{
    for (csmInt32 i = 0; i < _modelJson->GetTextureCount(); i++) {
        if (!strcmp(_modelJson->GetTextureFileName(i), "")) continue;

        csmString texPath = csmString(_modelDir.c_str()) + _modelJson->GetTextureFileName(i);
        LAppTextureManager_Common::TextureInfo* tex =
            static_cast<LAppTextureManager_Android*>(_textureManager)->CreateTextureFromPngFile(texPath.GetRawString());
        GetRenderer<Rendering::CubismRenderer_OpenGLES2>()->BindTexture(i, tex->id);
    }
    GetRenderer<Rendering::CubismRenderer_OpenGLES2>()->IsPremultipliedAlpha(false);
}

Csm::CubismMotionQueueEntryHandle CubismModelAndroid::StartMotion(
    const Csm::csmChar* group, Csm::csmInt32 no, Csm::csmInt32 priority)
{
    if (!_modelJson->GetMotionCount(group))
        return Csm::InvalidMotionQueueEntryHandleValue;

    if (priority == LAppDefine::PriorityForce) {
        _motionManager->SetReservePriority(priority);
    } else if (!_motionManager->ReserveMotion(priority)) {
        return Csm::InvalidMotionQueueEntryHandleValue;
    }

    const Csm::csmString fileName = _modelJson->GetMotionFileName(group, no);
    csmString name = Utils::CubismString::GetFormatedString("%s_%d", group, no);
    CubismMotion* motion = static_cast<CubismMotion*>(_motions[name.GetRawString()]);
    csmBool autoDelete = false;

    if (!motion) {
        csmString path = csmString(_modelDir.c_str()) + fileName;
        csmByte* buffer;
        csmSizeInt size;
        buffer = CreateBuffer(path.GetRawString(), &size);
        motion = static_cast<CubismMotion*>(
            LoadMotion(buffer, size, nullptr, nullptr, nullptr, _modelJson, group, no));
        if (motion) autoDelete = true;
        DeleteBuffer(buffer, path.GetRawString());
    }

    return _motionManager->StartMotionPriority(motion, autoDelete, priority);
}

void CubismModelAndroid::PreloadMotionGroup(const Csm::csmChar* group)
{
    const csmInt32 count = _modelJson->GetMotionCount(group);
    for (csmInt32 i = 0; i < count; i++) {
        csmString name = Utils::CubismString::GetFormatedString("%s_%d", group, i);
        csmString path = csmString(_modelDir.c_str()) + _modelJson->GetMotionFileName(group, i);
        csmByte* buffer;
        csmSizeInt size;
        buffer = CreateBuffer(path.GetRawString(), &size);
        CubismMotion* motion = static_cast<CubismMotion*>(
            LoadMotion(buffer, size, name.GetRawString(), nullptr, nullptr, _modelJson, group, i));
        if (motion) {
            if (_motions[name.GetRawString()]) {
                ACubismMotion::Delete(_motions[name.GetRawString()]);
            }
            _motions[name.GetRawString()] = motion;
        }
        DeleteBuffer(buffer, path.GetRawString());
    }
}

void CubismModelAndroid::ModelParamUpdate()
{
    const Csm::csmFloat32 dt = LAppPal::GetDeltaTime();
    _userTimeSeconds += dt;

    _dragManager->Update(dt);
    _dragX = _dragManager->GetX();
    _dragY = _dragManager->GetY();

    Csm::csmBool motionUpdated = false;
    _model->LoadParameters();

    if (_motionManager->IsFinished()) {
        StartMotion(LAppDefine::MotionGroupIdle, 0, LAppDefine::PriorityIdle);
    } else {
        motionUpdated = _motionManager->UpdateMotion(_model, dt);
    }

    _model->SaveParameters();

    if (_expressionManager) {
        _expressionManager->UpdateMotion(_model, dt);
    }

    if (!motionUpdated && _eyeBlink) {
        _eyeBlink->UpdateParameters(_model, dt);
    }

    if (_breath) {
        _breath->UpdateParameters(_model, dt);
    }

    _model->AddParameterValue(_idParamAngleX,     _dragX * 30.0f);
    _model->AddParameterValue(_idParamAngleY,     _dragY * 30.0f);
    _model->AddParameterValue(_idParamAngleZ,     _dragX * _dragY * -30.0f);
    _model->AddParameterValue(_idParamBodyAngleX, _dragX * 10.0f);
    _model->AddParameterValue(_idParamEyeBallX,   _dragX);
    _model->AddParameterValue(_idParamEyeBallY,   _dragY);

    {
        const float lipVal = amadeus_native_lip_sync_value();
        for (csmUint32 i = 0; i < _lipSyncIds.GetSize(); ++i) {
            _model->AddParameterValue(_lipSyncIds[i], lipVal, 0.8f);
        }
    }

    if (_physics) _physics->Evaluate(_model, dt);
    if (_pose)    _pose->UpdateParameters(_model, dt);

    _model->Update();
}

void CubismModelAndroid::Draw(Csm::CubismMatrix44& matrix)
{
    if (!_model) return;

    matrix.MultiplyByMatrix(_modelMatrix);
    GetRenderer<Csm::Rendering::CubismRenderer_OpenGLES2>()->SetMvpMatrix(&matrix);
    GetRenderer<Csm::Rendering::CubismRenderer_OpenGLES2>()->DrawModel();
}

void CubismModelAndroid::ModelOnUpdate(int width, int height)
{
    Csm::CubismMatrix44 projection;
    projection.LoadIdentity();

    if (_model->GetCanvasWidth() > 1.0f && width < height) {
        GetModelMatrix()->SetWidth(2.0f);
        projection.Scale(1.0f, static_cast<float>(width) / static_cast<float>(height));
    } else {
        projection.Scale(static_cast<float>(height) / static_cast<float>(width), 1.0f);
    }

    ModelParamUpdate();
    Draw(projection);
}

void CubismModelAndroid::SetExpression(const char* name)
{
    if (!name || !_modelJson) return;
    ACubismMotion* motion = _expressions[name];
    if (motion && _expressionManager) {
        _expressionManager->StartMotionPriority(motion, false, LAppDefine::PriorityForce);
    }
}

void CubismModelAndroid::ReleaseModelSetting()
{
    for (auto it = _motions.Begin(); it != _motions.End(); ++it) {
        ACubismMotion::Delete(it->Second);
    }
    _motions.Clear();

    for (auto it = _expressions.Begin(); it != _expressions.End(); ++it) {
        ACubismMotion::Delete(it->Second);
    }
    _expressions.Clear();

    delete _modelJson;
    _modelJson = nullptr;
}
