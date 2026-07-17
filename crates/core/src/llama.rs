//! In-process llama.cpp engine (feature `llama`). Loads a GGUF model once and
//! serializes generation requests through a mutex — on consumer hardware one
//! generation at a time is the right behavior anyway.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use crate::engine::{ChatMessage, Engine, TokenSink};

/// llama.cpp's backend may only be initialized once per process; model
/// switching creates new engines, so the backend lives in a global.
static BACKEND: std::sync::OnceLock<LlamaBackend> = std::sync::OnceLock::new();

fn backend() -> Result<&'static LlamaBackend> {
    if BACKEND.get().is_none() {
        let b = LlamaBackend::init().context("initializing llama backend")?;
        let _ = BACKEND.set(b);
    }
    Ok(BACKEND.get().expect("backend initialized"))
}

pub struct LlamaEngine {
    backend: &'static LlamaBackend,
    model: LlamaModel,
    model_path: PathBuf,
    lock: Mutex<()>,
    n_ctx: u32,
}

impl LlamaEngine {
    pub fn load(model_path: &Path, n_ctx: u32) -> Result<LlamaEngine> {
        let backend = backend()?;
        // Offload everything to GPU when one exists (Metal/Vulkan/CUDA build);
        // llama.cpp silently falls back to CPU otherwise. iOS is the exception:
        // its Metal backend crashes llama.cpp — the simulator stalls, and on a
        // real device ggml_backend_metal_buffer allocation segfaults (SIGSEGV in
        // ggml_metal_buffer_is_shared while building the KV cache). So stay on
        // CPU for all iOS; the Accelerate framework keeps it reasonably fast.
        let n_gpu_layers: u32 = if cfg!(target_os = "ios") { 0 } else { 1_000_000 };
        let params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);
        let model = LlamaModel::load_from_file(backend, model_path, &params)
            .with_context(|| format!("loading model {}", model_path.display()))?;
        // iOS apps get a tight memory budget; the default 8k KV cache + compute
        // buffers fail to allocate on a 3 GB device (new_context errors out, so
        // chat fails with "creating llama context"). Cap the context on iOS so
        // inference fits in memory. Desktop keeps the full configured window.
        let n_ctx = if cfg!(target_os = "ios") { n_ctx.min(2048) } else { n_ctx };
        Ok(LlamaEngine {
            backend,
            model,
            model_path: model_path.to_path_buf(),
            lock: Mutex::new(()),
            n_ctx,
        })
    }

    fn render_prompt(&self, messages: &[ChatMessage]) -> Result<String> {
        let chat: Vec<LlamaChatMessage> = messages
            .iter()
            .map(|m| LlamaChatMessage::new(m.role.clone(), m.content.clone()))
            .collect::<Result<_, _>>()?;
        // Some models ship Jinja templates too complex for llama.cpp's
        // template engine (Gemma 4 uses macros); fall back to rendering the
        // conversation manually in the model family's native format.
        if let Ok(tmpl) = self.model.chat_template(None) {
            if let Ok(p) = self.model.apply_chat_template(&tmpl, &chat, true) {
                return Ok(p);
            }
        }
        Ok(self.render_manual(messages))
    }

    fn render_manual(&self, messages: &[ChatMessage]) -> String {
        let gemma = self
            .model_path
            .file_name()
            .map(|f| f.to_string_lossy().to_lowercase().contains("gemma"))
            .unwrap_or(false);
        let mut out = String::new();
        if gemma {
            // Gemma has no system role: fold it into the first user turn.
            let mut pending_system = String::new();
            for m in messages {
                match m.role.as_str() {
                    "system" => pending_system = m.content.clone(),
                    "assistant" => {
                        out.push_str("<start_of_turn>model\n");
                        out.push_str(&m.content);
                        out.push_str("<end_of_turn>\n");
                    }
                    _ => {
                        out.push_str("<start_of_turn>user\n");
                        if !pending_system.is_empty() {
                            out.push_str(&pending_system);
                            out.push_str("\n\n");
                            pending_system.clear();
                        }
                        out.push_str(&m.content);
                        out.push_str("<end_of_turn>\n");
                    }
                }
            }
            out.push_str("<start_of_turn>model\n");
        } else {
            // ChatML: the most widely understood generic format.
            for m in messages {
                out.push_str(&format!("<|im_start|>{}\n{}<|im_end|>\n", m.role, m.content));
            }
            out.push_str("<|im_start|>assistant\n");
        }
        out
    }
}

impl Engine for LlamaEngine {
    fn name(&self) -> String {
        self.model_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "gguf model".into())
    }

    fn can_plan(&self) -> bool {
        true
    }

    fn generate(
        &self,
        messages: &[ChatMessage],
        sink: TokenSink,
        max_new_tokens: usize,
    ) -> Result<String> {
        let _guard = self.lock.lock().unwrap();
        let prompt = self.render_prompt(messages)?;

        // A smaller batch trims the compute-buffer allocation on memory-
        // constrained iOS devices; desktop keeps the larger, faster batch.
        // This same value MUST size the prompt-feeding loop below: decoding a
        // batch larger than the context's n_batch trips a GGML assert that
        // aborts the whole process (a hard crash on device, not a Rust error).
        let n_batch: usize = if cfg!(target_os = "ios") { 256 } else { 512 };
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(self.n_ctx))
            .with_n_batch(n_batch as u32);
        let mut ctx = self
            .model
            .new_context(self.backend, ctx_params)
            .context("creating llama context")?;

        let mut tokens = self.model.str_to_token(&prompt, AddBos::Always)?;
        // If the prompt is too large for the context window, drop from the
        // middle (oldest conversation) — the head carries the system rules and
        // sources, the tail carries the question itself.
        let budget = self.n_ctx as usize - max_new_tokens.min(self.n_ctx as usize / 4) - 8;
        crate::engine::keep_head_tail(&mut tokens, budget);

        // Feed the prompt in n_batch-sized pieces (same n_batch the context was
        // built with — see the crash note above).
        let mut batch = LlamaBatch::new(n_batch, 1);
        let last_idx = tokens.len() - 1;
        let mut pos = 0usize;
        while pos < tokens.len() {
            batch.clear();
            let end = (pos + n_batch).min(tokens.len());
            for (j, tok) in tokens[pos..end].iter().enumerate() {
                let i = pos + j;
                batch.add(*tok, i as i32, &[0], i == last_idx)?;
            }
            ctx.decode(&mut batch).context("prompt decode failed")?;
            pos = end;
        }

        let mut sampler = LlamaSampler::chain_simple([
            // Small models loop badly without a repetition penalty.
            LlamaSampler::penalties(256, 1.15, 0.05, 0.0),
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.9, 1),
            LlamaSampler::temp(0.3),
            LlamaSampler::dist(42),
        ]);

        let mut out = String::new();
        let mut n_cur = tokens.len();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        for _ in 0..max_new_tokens {
            // The KV cache only holds n_ctx positions. Once generation fills the
            // window, stop cleanly with what we have rather than decoding past
            // it — writing beyond n_ctx errors out as "decode failed" and throws
            // away the whole answer mid-stream. On a small context (mobile caps
            // at 2048) a rambly model can reach this before emitting an EOG.
            if n_cur >= self.n_ctx as usize {
                break;
            }
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if self.model.is_eog_token(token) {
                break;
            }
            let bytes = self.model.token_to_piece_bytes(token, 64, false, None)?;
            let mut piece = String::with_capacity(bytes.len() + 4);
            let _ = decoder.decode_to_string(&bytes, &mut piece, false);
            if !piece.is_empty() {
                out.push_str(&piece);
                if !sink(&piece) {
                    break;
                }
            }
            batch.clear();
            batch.add(token, n_cur as i32, &[0], true)?;
            n_cur += 1;
            ctx.decode(&mut batch).context("decode failed")?;
        }
        Ok(out)
    }
}
