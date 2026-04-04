#include "bridge.h"

#include <climits>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "llama.h"

extern "C" {

void amadeus_llama_backend_init(void) {
    llama_backend_init();
}

void amadeus_llama_backend_free(void) {
    llama_backend_free();
}

AmadeusLlamaModel amadeus_llama_load_model(const char* path, int n_gpu_layers) {
    llama_model_params params = llama_model_default_params();
    params.n_gpu_layers = n_gpu_layers;
    return static_cast<AmadeusLlamaModel>(llama_model_load_from_file(path, params));
}

void amadeus_llama_free_model(AmadeusLlamaModel model) {
    if (model) {
        llama_model_free(static_cast<llama_model*>(model));
    }
}

AmadeusLlamaContext amadeus_llama_new_context(AmadeusLlamaModel model, uint32_t n_ctx, int n_threads) {
    llama_context_params params = llama_context_default_params();
    params.n_ctx = (n_ctx > 0) ? n_ctx : 4096;
    if (n_threads > 0) {
        params.n_threads = n_threads;
        params.n_threads_batch = n_threads;
    }
    params.flash_attn = true;
    return static_cast<AmadeusLlamaContext>(
        llama_init_from_model(static_cast<llama_model*>(model), params));
}

void amadeus_llama_free_context(AmadeusLlamaContext ctx) {
    if (ctx) {
        llama_free(static_cast<llama_context*>(ctx));
    }
}

int32_t amadeus_llama_apply_chat_template(
    AmadeusLlamaModel model,
    const char* system_prompt,
    const char* user_message,
    char* buf,
    int32_t buf_size)
{
    llama_chat_message messages[2];
    int n_messages = 0;

    if (system_prompt && system_prompt[0] != '\0') {
        messages[n_messages++] = { "system", system_prompt };
    }
    messages[n_messages++] = { "user", user_message };

    return llama_chat_apply_template(
        static_cast<llama_model*>(model),
        nullptr,
        messages,
        n_messages,
        /*add_ass=*/true,
        buf,
        buf_size);
}

int amadeus_llama_generate(
    AmadeusLlamaContext ctx_opaque,
    AmadeusLlamaModel model_opaque,
    const char* prompt,
    int max_tokens,
    float temperature,
    float top_p,
    AmadeusLlamaTokenCb callback,
    void* user_data)
{
    auto* ctx   = static_cast<llama_context*>(ctx_opaque);
    auto* model = static_cast<llama_model*>(model_opaque);
    const llama_vocab* vocab = llama_model_get_vocab(model);

    // Tokenize prompt
    const int n_prompt_max = 8192;
    std::vector<llama_token> tokens(n_prompt_max);
    int n_tokens = llama_tokenize(
        vocab,
        prompt, (int32_t)strlen(prompt),
        tokens.data(), n_prompt_max,
        /*add_special=*/true,
        /*parse_special=*/true);
    if (n_tokens < 0) {
        // Buffer was too small — try again with exact size
        tokens.resize(-n_tokens);
        n_tokens = llama_tokenize(
            vocab,
            prompt, (int32_t)strlen(prompt),
            tokens.data(), (int32_t)tokens.size(),
            /*add_special=*/true,
            /*parse_special=*/true);
        if (n_tokens < 0) return -1;
    }
    tokens.resize(n_tokens);

    // Clear the KV cache so we start fresh
    llama_kv_self_clear(ctx);

    // Build sampler chain
    llama_sampler_chain_params sparams = llama_sampler_chain_default_params();
    llama_sampler* smpl = llama_sampler_chain_init(sparams);
    llama_sampler_chain_add(smpl, llama_sampler_init_top_p(top_p, 1));
    llama_sampler_chain_add(smpl, llama_sampler_init_temp(temperature));
    llama_sampler_chain_add(smpl, llama_sampler_init_dist(LLAMA_DEFAULT_SEED));

    // Feed the prompt as a single batch
    llama_batch batch = llama_batch_get_one(tokens.data(), (int32_t)tokens.size());
    if (llama_decode(ctx, batch) != 0) {
        llama_sampler_free(smpl);
        return -2;
    }

    // Generate tokens
    char piece_buf[256];
    int generated = 0;
    bool aborted = false;

    while (generated < max_tokens) {
        llama_token id = llama_sampler_sample(smpl, ctx, -1);
        llama_sampler_accept(smpl, id);

        if (llama_vocab_is_eog(vocab, id)) break;

        int piece_len = llama_token_to_piece(
            vocab, id,
            piece_buf, (int32_t)sizeof(piece_buf),
            /*lstrip=*/0,
            /*special=*/false);
        if (piece_len < 0) break;

        if (callback && callback(piece_buf, (size_t)piece_len, user_data) != 0) {
            aborted = true;
            break;
        }

        // Decode next token
        llama_batch next_batch = llama_batch_get_one(&id, 1);
        if (llama_decode(ctx, next_batch) != 0) break;

        ++generated;
    }

    llama_sampler_free(smpl);
    return aborted ? 1 : 0;
}

} // extern "C"
