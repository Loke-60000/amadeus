#include "llama_bridge.h"
#include "llama.h"

#include <algorithm>
#include <cstdlib>
#include <cstring>
#include <thread>
#include <vector>

struct AmadeusLlmSession {
    llama_model   *model;
    llama_context *ctx;
};

static const llama_vocab *session_vocab(const AmadeusLlmSession *s) {
    return llama_model_get_vocab(s->model);
}

// Half of logical CPUs ≈ physical cores; clamped to [4, 32].
static int detect_n_threads() {
    const unsigned int hw = std::thread::hardware_concurrency();
    const int n = hw > 0 ? static_cast<int>(hw / 2) : 4;
    return std::max(4, std::min(n, 32));
}

AmadeusLlmSession *amadeus_llm_load(const char *path, int n_gpu_layers) {
    llama_backend_init();

    llama_model_params mparams = llama_model_default_params();
    mparams.n_gpu_layers = n_gpu_layers;

    llama_model *model = llama_model_load_from_file(path, mparams);
    if (!model) {
        llama_backend_free();
        return nullptr;
    }

    const int n_threads = detect_n_threads();

    llama_context_params cparams = llama_context_default_params();
    cparams.n_ctx           = 16384; // 16k context; KV cache ~2.25 GiB at fp16
    cparams.n_batch         = 2048;  // prefill chunk; must be <= n_ctx
    cparams.n_ubatch        = 512;
    cparams.n_threads       = n_threads;
    cparams.n_threads_batch = n_threads;
    cparams.no_perf         = true;

    llama_context *ctx = llama_init_from_model(model, cparams);
    if (!ctx) {
        llama_model_free(model);
        llama_backend_free();
        return nullptr;
    }

    return new AmadeusLlmSession{model, ctx};
}

int amadeus_llm_generate(
    AmadeusLlmSession     *session,
    const char            *prompt,
    int                    max_tokens,
    float                  temperature,
    amadeus_token_callback callback,
    void                  *user_data)
{
    if (!session || !prompt || !callback) return -1;

    const llama_vocab *vocab = session_vocab(session);
    llama_context     *ctx   = session->ctx;

    // Tokenize — first call returns negative count when the output buffer is too small.
    int n_prompt = -llama_tokenize(
        vocab, prompt, (int)strlen(prompt),
        nullptr, 0, /*add_special=*/true, /*parse_special=*/true);
    if (n_prompt <= 0) return -1;

    std::vector<llama_token> tokens((size_t)n_prompt);
    if (llama_tokenize(
            vocab, prompt, (int)strlen(prompt),
            tokens.data(), n_prompt,
            /*add_special=*/true, /*parse_special=*/true) < 0)
        return -1;

    // Clamp prompt to leave room for generation.  Keep the most-recent tokens
    // so the model always sees the end of the conversation.
    const int n_ctx   = (int)llama_n_ctx(ctx);
    const int gen_cap = std::max(max_tokens, 128);
    const int budget  = n_ctx - gen_cap;
    if (budget > 0 && (int)tokens.size() > budget) {
        tokens.erase(tokens.begin(), tokens.begin() + ((int)tokens.size() - budget));
    }
    n_prompt = (int)tokens.size();

    // Clear KV cache from any prior run so positions start at 0.
    llama_memory_clear(llama_get_memory(ctx), /*data=*/false);

    // Prefill in chunks of n_batch — avoids GGML_ASSERT(n_tokens_all <= n_batch).
    const int n_batch = (int)llama_n_batch(ctx);
    int n_consumed = 0;
    while (n_consumed < n_prompt) {
        const int chunk = std::min(n_batch, n_prompt - n_consumed);
        llama_batch batch = llama_batch_get_one(tokens.data() + n_consumed, chunk);
        if (llama_decode(ctx, batch) != 0) return -1;
        n_consumed += chunk;
    }

    // Sampler chain: top-k → top-p → temperature → stochastic dist.
    llama_sampler_chain_params sparams = llama_sampler_chain_default_params();
    sparams.no_perf = true;
    llama_sampler *smpl = llama_sampler_chain_init(sparams);
    llama_sampler_chain_add(smpl, llama_sampler_init_top_k(40));
    llama_sampler_chain_add(smpl, llama_sampler_init_top_p(0.9f, 1));
    llama_sampler_chain_add(smpl, llama_sampler_init_temp(temperature > 0.0f ? temperature : 0.7f));
    llama_sampler_chain_add(smpl, llama_sampler_init_dist(LLAMA_DEFAULT_SEED));

    char piece[256];
    int  n_generated = 0;

    while (n_generated < max_tokens) {
        llama_token id = llama_sampler_sample(smpl, ctx, -1);
        llama_sampler_accept(smpl, id);

        if (llama_vocab_is_eog(vocab, id)) break;

        int n = llama_token_to_piece(vocab, id, piece, (int)sizeof(piece) - 1,
                                     /*lstrip=*/0, /*special=*/false);
        if (n < 0) break;
        piece[n] = '\0';
        callback(piece, user_data);

        ++n_generated;

        llama_batch next = llama_batch_get_one(&id, 1);
        if (llama_decode(ctx, next) != 0) break;
    }

    llama_sampler_free(smpl);
    return 0;
}

void amadeus_llm_free(AmadeusLlmSession *session) {
    if (!session) return;
    llama_free(session->ctx);
    llama_model_free(session->model);
    llama_backend_free();
    delete session;
}
