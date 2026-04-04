#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void* AmadeusLlamaModel;
typedef void* AmadeusLlamaContext;

// Called for each decoded token piece during generation.
// Return non-zero to abort generation.
typedef int (*AmadeusLlamaTokenCb)(const char* piece, size_t piece_len, void* user_data);

// Initialize the llama backend (call once at startup).
void amadeus_llama_backend_init(void);
void amadeus_llama_backend_free(void);

// Load/free a model from a GGUF file.
// n_gpu_layers: number of layers to offload to GPU (0 = CPU only, INT_MAX = all).
AmadeusLlamaModel amadeus_llama_load_model(const char* path, int n_gpu_layers);
void amadeus_llama_free_model(AmadeusLlamaModel model);

// Create/free a context for a loaded model.
// n_ctx: KV cache size in tokens (0 = use model default).
// n_threads: CPU thread count (0 = auto).
AmadeusLlamaContext amadeus_llama_new_context(AmadeusLlamaModel model, uint32_t n_ctx, int n_threads);
void amadeus_llama_free_context(AmadeusLlamaContext ctx);

// Apply the model's built-in chat template to produce a formatted prompt.
// Returns the number of bytes written to buf (excluding null terminator),
// or a negative error code on failure.
// If buf is NULL / buf_size is 0 the function returns the required buffer size.
int32_t amadeus_llama_apply_chat_template(
    AmadeusLlamaModel model,
    const char* system_prompt,
    const char* user_message,
    char* buf,
    int32_t buf_size);

// Run inference on an already-formatted prompt string.
// Calls callback for each decoded token piece until EOS or max_tokens is reached.
// Returns 0 on success, negative on error.
int amadeus_llama_generate(
    AmadeusLlamaContext ctx,
    AmadeusLlamaModel model,
    const char* prompt,
    int max_tokens,
    float temperature,
    float top_p,
    AmadeusLlamaTokenCb callback,
    void* user_data);

#ifdef __cplusplus
}
#endif
