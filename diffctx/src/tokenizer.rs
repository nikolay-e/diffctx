use once_cell::sync::Lazy;
use tiktoken_rs::CoreBPE;

static ENCODER: Lazy<CoreBPE> = Lazy::new(|| tiktoken_rs::o200k_base().unwrap());

pub fn count_tokens(text: &str) -> u32 {
    ENCODER.encode_with_special_tokens(text).len() as u32
}
