#include <aaudio/AAudio.h>
#include <android/log.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

#define LOG_TAG "amadeus-audio"
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)

typedef struct {
    const float* samples;
    int32_t      total_frames;
    int32_t      cursor;
    int32_t      channels;
} PlaybackState;

static aaudio_data_callback_result_t data_callback(
        AAudioStream* stream,
        void*         user_data,
        void*         audio_data,
        int32_t       num_frames)
{
    PlaybackState* state = (PlaybackState*)user_data;
    float* out = (float*)audio_data;
    int32_t frames_remaining = state->total_frames - state->cursor;
    int32_t frames_to_copy   = frames_remaining < num_frames ? frames_remaining : num_frames;

    if (frames_to_copy > 0) {
        memcpy(out,
               state->samples + (state->cursor * state->channels),
               (size_t)(frames_to_copy * state->channels) * sizeof(float));
        state->cursor += frames_to_copy;
    }

    int32_t silence_frames = num_frames - frames_to_copy;
    if (silence_frames > 0) {
        memset(out + frames_to_copy * state->channels, 0,
               (size_t)(silence_frames * state->channels) * sizeof(float));
    }

    return (state->cursor >= state->total_frames)
        ? AAUDIO_CALLBACK_RESULT_STOP
        : AAUDIO_CALLBACK_RESULT_CONTINUE;
}

int amadeus_aaudio_play_pcm_f32(
        const float* samples,
        int32_t      num_frames,
        int32_t      sample_rate,
        int32_t      channels)
{
    if (!samples || num_frames <= 0 || sample_rate <= 0 || channels <= 0)
        return -1;

    PlaybackState state = {
        .samples      = samples,
        .total_frames = num_frames,
        .cursor       = 0,
        .channels     = channels,
    };

    AAudioStreamBuilder* builder = NULL;
    AAudioStream*        stream  = NULL;
    aaudio_result_t      result;

    result = AAudio_createStreamBuilder(&builder);
    if (result != AAUDIO_OK) { LOGE("createStreamBuilder: %s", AAudio_convertResultToText(result)); return -1; }

    AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_FLOAT);
    AAudioStreamBuilder_setSampleRate(builder, sample_rate);
    AAudioStreamBuilder_setChannelCount(builder, channels);
    AAudioStreamBuilder_setPerformanceMode(builder, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
#if __ANDROID_API__ >= 28
    AAudioStreamBuilder_setUsage(builder, AAUDIO_USAGE_MEDIA);
#endif
    AAudioStreamBuilder_setDataCallback(builder, data_callback, &state);

    result = AAudioStreamBuilder_openStream(builder, &stream);
    AAudioStreamBuilder_delete(builder);
    if (result != AAUDIO_OK) { LOGE("openStream: %s", AAudio_convertResultToText(result)); return -1; }

    result = AAudioStream_requestStart(stream);
    if (result != AAUDIO_OK) {
        LOGE("requestStart: %s", AAudio_convertResultToText(result));
        AAudioStream_close(stream);
        return -1;
    }

    while (state.cursor < state.total_frames) {
        aaudio_stream_state_t current_state = AAudioStream_getState(stream);
        if (current_state == AAUDIO_STREAM_STATE_STOPPED ||
            current_state == AAUDIO_STREAM_STATE_DISCONNECTED) {
            break;
        }
        aaudio_stream_state_t next_state = AAUDIO_STREAM_STATE_UNINITIALIZED;
        AAudioStream_waitForStateChange(stream, current_state, &next_state, 8000000LL);
    }

    AAudioStream_requestStop(stream);
    AAudioStream_close(stream);
    return 0;
}
