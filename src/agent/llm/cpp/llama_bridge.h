#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct AmadeusLlmSession AmadeusLlmSession;

/* Called for each generated token piece (UTF-8, null-terminated). */
typedef void (*amadeus_token_callback)(const char *token, void *user_data);

/*
 * Load a GGUF model from `path`.
 * `n_gpu_layers` controls how many transformer layers are offloaded to GPU.
 * Pass 0 for CPU-only inference.
 * Returns NULL on failure.
 */
AmadeusLlmSession *amadeus_llm_load(const char *path, int n_gpu_layers);

/*
 * Run inference on `prompt` (pre-formatted ChatML string).
 * Calls `callback(token, user_data)` for each generated token piece.
 * Returns 0 on success, -1 on error.
 */
int amadeus_llm_generate(
    AmadeusLlmSession *session,
    const char        *prompt,
    int                max_tokens,
    float              temperature,
    amadeus_token_callback callback,
    void              *user_data);

/* Free all resources associated with the session. */
void amadeus_llm_free(AmadeusLlmSession *session);

#ifdef __cplusplus
}
#endif
