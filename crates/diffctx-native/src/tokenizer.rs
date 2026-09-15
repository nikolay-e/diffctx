use once_cell::sync::Lazy;
use schemars::JsonSchema;
use serde::Serialize;
use tiktoken_rs::CoreBPE;

#[derive(Debug, thiserror::Error)]
pub enum TokenizerError {
    #[error("failed to load o200k_base BPE tables: {0}")]
    EncoderInit(String),
}

/// The accounting unit every budget, envelope charge and `token_count` is
/// expressed in. A budget of `N` means `N` tokens under THIS tokenizer; no
/// model-specific tokenizer is emulated, and the id travels in provenance so
/// a consumer counting against a different model knows what it is converting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenizerId {
    O200kBase,
}

impl TokenizerId {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenizerId::O200kBase => "o200k_base",
        }
    }
}

pub trait TokenCounter: Send + Sync {
    fn id(&self) -> TokenizerId;
    fn count(&self, text: &str) -> u32;
}

/// `DIFFCTX_TOKEN_SAFETY_FACTOR`: a multiplier a consumer applies when the
/// model it feeds tokenizes denser than o200k_base. 1.0 (the default) leaves
/// every count exact; anything else scales every count and therefore every
/// budget decision, and is recorded in provenance. Never defaulted to a
/// model-specific guess: the right margin is the consumer's measurement.
pub fn safety_factor() -> f64 {
    *SAFETY_FACTOR
}

static SAFETY_FACTOR: Lazy<f64> = Lazy::new(|| {
    std::env::var("DIFFCTX_TOKEN_SAFETY_FACTOR")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 1.0)
        .unwrap_or(1.0)
});

/// The process-wide accounting counter: o200k_base scaled by the safety
/// factor. Everything that charges a budget goes through it.
pub struct AccountingCounter;

impl TokenCounter for AccountingCounter {
    fn id(&self) -> TokenizerId {
        TokenizerId::O200kBase
    }

    fn count(&self, text: &str) -> u32 {
        scale(count_raw(text))
    }
}

pub static ACCOUNTING: AccountingCounter = AccountingCounter;

// `Lazy<Result<...>>` keeps the static initialization fallible without
// crashing the host process (PyO3 surface) on sandboxed / proxy-blocked
// environments where tiktoken-rs may fail to materialize the BPE tables.
// `try_count_tokens` is the boundary-safe variant that returns the error;
// `count_tokens` retains the infallible signature used by ~5 internal
// hot-path call sites and degrades to a conservative byte-length estimate
// rather than aborting the entire process.
static ENCODER: Lazy<Result<CoreBPE, TokenizerError>> =
    Lazy::new(|| tiktoken_rs::o200k_base().map_err(|e| TokenizerError::EncoderInit(e.to_string())));

fn scale(raw: u32) -> u32 {
    let factor = safety_factor();
    if factor == 1.0 {
        raw
    } else {
        (f64::from(raw) * factor).ceil() as u32
    }
}

pub fn try_count_tokens(text: &str) -> Result<u32, TokenizerError> {
    try_count_raw(text).map(scale)
}

fn try_count_raw(text: &str) -> Result<u32, TokenizerError> {
    // Why `encode_ordinary` (not `encode_with_special_tokens`):
    //
    // 1. Budget contract (R2-T1 regression): `encode_with_special_tokens`
    //    collapses literal `<|endoftext|>`-style sequences into a single
    //    token, breaking byte-accurate accounting against the user budget.
    //
    // 2. Prompt-injection safety: a diff is user input. Treating literal
    //    `<|...|>` sequences as opaque text prevents them from being
    //    interpreted as model control tokens downstream.
    match &*ENCODER {
        Ok(enc) => Ok(enc.encode_ordinary(text).len() as u32),
        Err(e) => Err(TokenizerError::EncoderInit(e.to_string())),
    }
}

fn count_raw(text: &str) -> u32 {
    // Infallible variant for internal hot-path call sites. On encoder-init
    // failure, fall back to a conservative byte-length estimate (4 bytes
    // per token heuristic) so the pipeline degrades gracefully instead of
    // aborting the host process.
    match try_count_raw(text) {
        Ok(n) => n,
        Err(_) if text.is_empty() => 0,
        Err(_) => ((text.len() as u32) / 4).max(1),
    }
}

pub fn count_tokens(text: &str) -> u32 {
    ACCOUNTING.count(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_token_literals_are_not_collapsed() {
        // R2-T1 regression: `encode_with_special_tokens` would collapse
        // a literal `<|endoftext|>` to a single token, escaping the budget
        // contract. `encode_ordinary` treats it as plain text.
        let n = count_tokens("literal <|endoftext|> in code");
        assert!(
            n > 1,
            "tokenizer must not collapse special-token literals; got {n} tokens"
        );
    }

    #[test]
    fn the_accounting_counter_names_its_tokenizer() {
        assert_eq!(ACCOUNTING.id().as_str(), "o200k_base");
        assert_eq!(ACCOUNTING.count("hello world"), count_raw("hello world"));
    }
}
