#[cfg(not(feature = "local-llm"))]
use anyhow::bail;
#[cfg(feature = "local-llm")]
use anyhow::Context;
use anyhow::Result;
#[cfg(feature = "local-llm")]
use std::sync::atomic::{AtomicBool, Ordering};

/// `true` while the local LLM is inside a `<think>…</think>` block.
/// Read by the overlay to drive future animation states.
#[cfg(feature = "local-llm")]
static LLAMA_IS_THINKING: AtomicBool = AtomicBool::new(false);

/// Returns `true` while the model is inside a `<think>…</think>` block.
#[cfg(feature = "local-llm")]
pub fn is_thinking() -> bool {
    LLAMA_IS_THINKING.load(Ordering::Relaxed)
}

/// Holds the loaded model handle across `LlamaCppClient` instances so the model
/// stays in VRAM even when config is reloaded. Cleared when the user switches to
/// a cloud provider via `release_persistent_handle()`.
#[cfg(feature = "local-llm")]
static PERSISTENT_HANDLE: std::sync::Mutex<Option<std::sync::Arc<ffi::LlmHandle>>> =
    std::sync::Mutex::new(None);

/// Return the already-loaded handle if present, otherwise load from disk and cache it.
#[cfg(feature = "local-llm")]
fn get_or_load_handle(
    path: &std::path::Path,
    n_gpu_layers: i32,
) -> anyhow::Result<std::sync::Arc<ffi::LlmHandle>> {
    let mut guard = PERSISTENT_HANDLE.lock().unwrap();
    if let Some(h) = guard.as_ref() {
        return Ok(h.clone());
    }
    let handle = std::sync::Arc::new(ffi::LlmHandle::load(path, n_gpu_layers)?);
    *guard = Some(handle.clone());
    Ok(handle)
}

/// Returns `true` when the model has been loaded into VRAM and is ready for inference.
/// Unlike `cached_client`, this survives across `reload_config()` calls.
#[cfg(feature = "local-llm")]
pub fn is_handle_loaded() -> bool {
    PERSISTENT_HANDLE.lock().map(|g| g.is_some()).unwrap_or(false)
}

#[cfg(not(feature = "local-llm"))]
pub fn is_handle_loaded() -> bool { false }

/// Drop the persistent model handle.  If the current `LlamaCppClient` (if any) also
/// drops its `Arc`, the model is freed from VRAM.  Call this when the user switches
/// to a cloud-based provider.
#[cfg(feature = "local-llm")]
pub fn release_persistent_handle() {
    if let Ok(mut guard) = PERSISTENT_HANDLE.lock() {
        *guard = None;
    }
}

/// No-op shim so callers compile without the feature flag.
#[cfg(not(feature = "local-llm"))]
pub fn release_persistent_handle() {}

use crate::{
    config::AgentRuntimeConfig,
    llm::common::{ModelClient, ModelTurn, TextStreamSink},
    session::SessionMessage,
    tools::ToolDefinition,
};

/// Local inference client backed by llama.cpp.
///
/// Requires the `local-llm` Cargo feature to do real work.  Without it, construction
/// succeeds at the type level but `new()` returns an error at runtime so the rest of
/// the codebase compiles unconditionally.
pub struct LlamaCppClient {
    #[cfg(feature = "local-llm")]
    inner: LocalInner,
    /// Model path kept for display / error messages in the no-feature path.
    #[cfg(not(feature = "local-llm"))]
    #[allow(dead_code)]
    model_path: String,
}

// ── feature = "local-llm" ────────────────────────────────────────────────────

#[cfg(feature = "local-llm")]
mod ffi {
    use std::ffi::{c_char, c_float, c_int, c_void, CStr, CString};
    use std::path::Path;

    use anyhow::{bail, Context, Result};

    unsafe extern "C" {
        /// Load a GGUF model from `path`.  Returns a heap-allocated handle or null on failure.
        fn amadeus_llm_load(path: *const c_char, n_gpu_layers: c_int) -> *mut c_void;
        /// Run inference.  Calls `callback(token, user_data)` for every generated token.
        /// Returns 0 on success.
        fn amadeus_llm_generate(
            handle: *mut c_void,
            prompt: *const c_char,
            max_tokens: c_int,
            temperature: c_float,
            callback: Option<unsafe extern "C" fn(*const c_char, *mut c_void)>,
            user_data: *mut c_void,
        ) -> c_int;
        /// Free the handle returned by `amadeus_llm_load`.
        fn amadeus_llm_free(handle: *mut c_void);
    }

    pub struct LlmHandle(*mut c_void);

    unsafe impl Send for LlmHandle {}
    unsafe impl Sync for LlmHandle {}

    impl Drop for LlmHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { amadeus_llm_free(self.0) };
            }
        }
    }

    impl LlmHandle {
        pub fn load(path: &Path, n_gpu_layers: i32) -> Result<Self> {
            let c_path = CString::new(path.to_string_lossy().as_ref())
                .context("model path contains a null byte")?;
            let handle = unsafe { amadeus_llm_load(c_path.as_ptr(), n_gpu_layers) };
            if handle.is_null() {
                bail!("llama.cpp failed to load model at {}", path.display());
            }
            Ok(Self(handle))
        }

        pub fn generate(
            &self,
            prompt: &str,
            max_tokens: i32,
            temperature: f32,
            mut on_token: impl FnMut(&str),
        ) -> Result<()> {
            struct CallbackState<'a> {
                on_token: &'a mut dyn FnMut(&str),
                /// Accumulates raw bytes across callbacks to handle partial UTF-8 sequences.
                utf8_buf: Vec<u8>,
            }

            unsafe extern "C" fn token_cb(token: *const c_char, user_data: *mut c_void) {
                let state = unsafe { &mut *(user_data as *mut CallbackState) };
                // llama_token_to_piece returns raw bytes; multi-byte UTF-8 codepoints can be
                // split across consecutive token callbacks.  Accumulate bytes and flush only
                // complete UTF-8 text to avoid "invalid utf-8" panics downstream.
                let bytes = unsafe { CStr::from_ptr(token) }.to_bytes();
                state.utf8_buf.extend_from_slice(bytes);
                loop {
                    match std::str::from_utf8(&state.utf8_buf) {
                        Ok(s) => {
                            let owned = s.to_string();
                            state.utf8_buf.clear();
                            (state.on_token)(&owned);
                            break;
                        }
                        Err(e) => {
                            let valid_up_to = e.valid_up_to();
                            if valid_up_to > 0 {
                                // Emit the valid prefix and keep the tail for the next round.
                                let valid = std::str::from_utf8(&state.utf8_buf[..valid_up_to])
                                    .expect("just validated");
                                let owned = valid.to_string();
                                state.utf8_buf.drain(..valid_up_to);
                                (state.on_token)(&owned);
                                // Continue loop — there may be more valid text after the tail.
                            } else if e.error_len().is_some() {
                                // Definitively invalid byte (not an incomplete sequence) — drop it.
                                state.utf8_buf.remove(0);
                            } else {
                                // Incomplete multi-byte sequence at the start — wait for more bytes.
                                break;
                            }
                        }
                    }
                }
            }

            let c_prompt = CString::new(prompt).context("prompt contains a null byte")?;
            let mut state = CallbackState {
                on_token: &mut on_token,
                utf8_buf: Vec::new(),
            };
            let rc = unsafe {
                amadeus_llm_generate(
                    self.0,
                    c_prompt.as_ptr(),
                    max_tokens,
                    temperature,
                    Some(token_cb),
                    &raw mut state as *mut c_void,
                )
            };
            if rc != 0 {
                bail!("amadeus_llm_generate returned error code {rc}");
            }
            // Flush any trailing bytes as lossy UTF-8 (should be empty in practice).
            if !state.utf8_buf.is_empty() {
                let tail = String::from_utf8_lossy(&state.utf8_buf).into_owned();
                (on_token)(&tail);
            }
            Ok(())
        }
    }
}

/// Wraps a token callback and strips `<think>…</think>` blocks from the stream.
///
/// - Tokens inside a thinking block are **not** forwarded to `emit`.
/// - `LLAMA_IS_THINKING` is set/cleared as tags are encountered.
/// - Both `<think>` and `</think>` may be split across multiple token callbacks;
///   the internal buffer handles this correctly.
#[cfg(feature = "local-llm")]
struct ThinkFilter {
    in_think: bool,
    /// Bytes that have arrived but not yet been classified (partial-tag guard).
    buf: String,
}

/// Walk backwards from `pos` to the nearest UTF-8 char boundary in `s`.
#[cfg(feature = "local-llm")]
fn floor_char_boundary(s: &str, mut pos: usize) -> usize {
    pos = pos.min(s.len());
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

#[cfg(feature = "local-llm")]
impl ThinkFilter {
    fn new() -> Self {
        Self { in_think: false, buf: String::new() }
    }

    /// Push a newly-decoded UTF-8 chunk through the filter; calls `emit` for visible text.
    fn push(&mut self, text: &str, emit: &mut dyn FnMut(&str)) {
        self.buf.push_str(text);
        self.drain(emit);
    }

    /// Called once after the last token; flushes anything still buffered.
    fn finish(self, emit: &mut dyn FnMut(&str)) {
        if !self.in_think && !self.buf.is_empty() {
            emit(&self.buf);
        }
        LLAMA_IS_THINKING.store(false, Ordering::Relaxed);
    }

    fn drain(&mut self, emit: &mut dyn FnMut(&str)) {
        loop {
            if self.in_think {
                match self.buf.find("</think>") {
                    Some(pos) => {
                        self.buf.drain(..pos + "</think>".len());
                        self.in_think = false;
                        LLAMA_IS_THINKING.store(false, Ordering::Relaxed);
                        // continue — there may be visible text after the close tag
                    }
                    None => {
                        // Keep the last 7 bytes: a partial `</think>` might span callbacks.
                        // Walk back to a char boundary so drain() doesn't panic on multi-byte chars.
                        let keep = ("</think>".len() - 1).min(self.buf.len());
                        let drain_to = floor_char_boundary(&self.buf, self.buf.len() - keep);
                        self.buf.drain(..drain_to);
                        break;
                    }
                }
            } else {
                match self.buf.find("<think>") {
                    Some(pos) => {
                        if pos > 0 {
                            let visible = self.buf[..pos].to_string();
                            emit(&visible);
                        }
                        self.buf.drain(..pos + "<think>".len());
                        self.in_think = true;
                        LLAMA_IS_THINKING.store(true, Ordering::Relaxed);
                        // continue — consume the thinking block
                    }
                    None => {
                        // Keep the last 6 bytes: a partial `<think>` might span callbacks.
                        // Walk back to a char boundary so the slice and drain() don't panic.
                        let keep = ("<think>".len() - 1).min(self.buf.len());
                        let drain_to = floor_char_boundary(&self.buf, self.buf.len() - keep);
                        if drain_to > 0 {
                            let visible = self.buf[..drain_to].to_string();
                            emit(&visible);
                            self.buf.drain(..drain_to);
                        }
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(feature = "local-llm")]
struct LocalInner {
    handle: std::sync::Arc<ffi::LlmHandle>,
    max_tokens: i32,
    temperature: f32,
}

#[cfg(feature = "local-llm")]
impl LocalInner {
    fn new(config: &AgentRuntimeConfig) -> Result<Self> {
        use std::env;

        let model_path = config
            .services
            .local_llm_model_path
            .as_deref()
            .context("local_llm_model_path is not set; add it to .amadeus/config.json")?;

        if !model_path.exists() {
            download_gguf(model_path)?;
        }

        // Default: offload all layers when a GPU backend (Vulkan/CUDA) is compiled in.
        // Override with AMADEUS_LLM_GPU_LAYERS=0 to force CPU inference.
        let gpu_default: i32 = if cfg!(feature = "llm-vulkan") || cfg!(feature = "llm-cuda") {
            999 // llama.cpp clamps this to the actual layer count
        } else {
            0
        };
        let n_gpu_layers: i32 = env::var("AMADEUS_LLM_GPU_LAYERS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(gpu_default);

        let handle = get_or_load_handle(model_path, n_gpu_layers)?;

        Ok(Self {
            handle,
            max_tokens: config.max_output_tokens as i32,
            temperature: config.temperature,
        })
    }
}

// ── GGUF auto-download ────────────────────────────────────────────────────────

/// Download the Qwen3-4B Q8_0 GGUF from HuggingFace if the file is not present.
///
/// Streams the response body to disk so the file is written atomically (to a
/// `.part` temp file, then renamed) — avoids leaving a partial GGUF behind on
/// interrupt.
#[cfg(feature = "local-llm")]
fn download_gguf(dest: &std::path::Path) -> Result<()> {
    use std::io::{Read, Write};

    const HF_URL: &str =
        "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q8_0.gguf";

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create model directory {}", parent.display()))?;
    }

    let part_path = dest.with_extension("gguf.part");

    eprintln!(
        "[amadeus] GGUF model not found at {}.\n[amadeus] Downloading from HuggingFace (≈4 GB) — this may take a while…",
        dest.display()
    );

    let mut response =
        reqwest::blocking::get(HF_URL).context("failed to start GGUF download from HuggingFace")?;

    if !response.status().is_success() {
        anyhow::bail!(
            "HuggingFace returned HTTP {} for {}",
            response.status(),
            HF_URL
        );
    }

    let total = response.content_length();

    let mut file = std::fs::File::create(&part_path)
        .with_context(|| format!("failed to create temp file {}", part_path.display()))?;

    let mut downloaded: u64 = 0;
    let mut buf = vec![0u8; 1024 * 256]; // 256 KiB chunks
    loop {
        let n = response
            .read(&mut buf)
            .context("error reading download stream")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .with_context(|| format!("error writing to {}", part_path.display()))?;
        downloaded += n as u64;
        if let Some(total) = total {
            let pct = downloaded * 100 / total;
            eprint!(
                "\r[amadeus] {pct}% ({:.1} / {:.1} GiB)",
                mb(downloaded),
                mb(total)
            );
        } else {
            eprint!(
                "\r[amadeus] {:.1} MiB downloaded",
                downloaded as f64 / 1_048_576.0
            );
        }
    }
    eprintln!(); // newline after progress

    // Atomic rename — only visible after fully written.
    std::fs::rename(&part_path, dest).with_context(|| {
        format!(
            "failed to rename {} → {}",
            part_path.display(),
            dest.display()
        )
    })?;

    eprintln!("[amadeus] Download complete: {}", dest.display());
    Ok(())
}

#[cfg(feature = "local-llm")]
fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

// ── public constructor ────────────────────────────────────────────────────────

impl LlamaCppClient {
    pub fn new(config: &AgentRuntimeConfig) -> Result<Self> {
        #[cfg(feature = "local-llm")]
        {
            Ok(Self {
                inner: LocalInner::new(config)?,
            })
        }
        #[cfg(not(feature = "local-llm"))]
        {
            let path = config
                .services
                .local_llm_model_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<unset>".to_string());
            bail!(
                "local LLM is disabled at compile time (model path: {path}); \
                 rebuild with `cargo build --features local-llm`"
            );
        }
    }
}

// ── ModelClient ───────────────────────────────────────────────────────────────

impl ModelClient for LlamaCppClient {
    fn complete(
        &self,
        system_prompt: &str,
        messages: &[SessionMessage],
        _tools: &[ToolDefinition],
    ) -> Result<ModelTurn> {
        #[cfg(feature = "local-llm")]
        {
            let prompt = build_chatml_prompt(system_prompt, messages);
            let mut visible = String::new();
            let mut filter = ThinkFilter::new();
            self.inner.handle.generate(
                &prompt,
                self.inner.max_tokens,
                self.inner.temperature,
                |t| filter.push(t, &mut |s| visible.push_str(s)),
            )?;
            filter.finish(&mut |s| visible.push_str(s));
            let text = strip_end_token(visible.trim()).to_string();
            Ok(ModelTurn {
                assistant_text: text,
                tool_calls: Vec::new(),
            })
        }
        #[cfg(not(feature = "local-llm"))]
        {
            let _ = (system_prompt, messages);
            bail!("local-llm feature is not compiled in");
        }
    }

    fn complete_streaming(
        &self,
        system_prompt: &str,
        messages: &[SessionMessage],
        _tools: &[ToolDefinition],
        stream: &mut dyn TextStreamSink,
    ) -> Result<ModelTurn> {
        #[cfg(feature = "local-llm")]
        {
            let prompt = build_chatml_prompt(system_prompt, messages);
            let mut visible = String::new();
            let mut filter = ThinkFilter::new();
            self.inner.handle.generate(
                &prompt,
                self.inner.max_tokens,
                self.inner.temperature,
                |token| {
                    filter.push(token, &mut |s| {
                        visible.push_str(s);
                        // Ignore stream errors mid-generation.
                        let _ = stream.on_text_delta(s);
                    });
                },
            )?;
            filter.finish(&mut |s| {
                visible.push_str(s);
                let _ = stream.on_text_delta(s);
            });
            let text = strip_end_token(visible.trim()).to_string();
            Ok(ModelTurn {
                assistant_text: text,
                tool_calls: Vec::new(),
            })
        }
        #[cfg(not(feature = "local-llm"))]
        {
            let _ = (system_prompt, messages, stream);
            bail!("local-llm feature is not compiled in");
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a Qwen3 ChatML prompt string from the session history.
/// Tool messages are rendered as plain user turns so the model can read results.
/// Tool calls are skipped (no tool support for local inference).
#[cfg(feature = "local-llm")]
fn build_chatml_prompt(system_prompt: &str, messages: &[SessionMessage]) -> String {
    use crate::session::SessionRole;

    let mut out = String::new();
    out.push_str("<|im_start|>system\n");
    out.push_str(system_prompt);
    out.push_str("<|im_end|>\n");

    for msg in messages {
        match msg.role {
            SessionRole::User => {
                out.push_str("<|im_start|>user\n");
                out.push_str(&msg.content);
                out.push_str("<|im_end|>\n");
            }
            SessionRole::Assistant => {
                if !msg.content.trim().is_empty() {
                    out.push_str("<|im_start|>assistant\n");
                    out.push_str(&msg.content);
                    out.push_str("<|im_end|>\n");
                }
            }
            SessionRole::Tool => {
                // Surface tool results as a user turn so the model sees them.
                out.push_str("<|im_start|>user\n[tool result] ");
                out.push_str(&msg.content);
                out.push_str("<|im_end|>\n");
            }
        }
    }

    // Open the assistant turn; llama.cpp fills the rest.
    out.push_str("<|im_start|>assistant\n");
    out
}

/// Strip the trailing `<|im_end|>` token that llama.cpp may or may not emit.
#[cfg(feature = "local-llm")]
fn strip_end_token(text: &str) -> &str {
    text.trim_end_matches("<|im_end|>").trim_end()
}
