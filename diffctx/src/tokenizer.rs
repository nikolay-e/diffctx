use once_cell::sync::Lazy;
use tiktoken_rs::CoreBPE;

static ENCODER: Lazy<CoreBPE> = Lazy::new(|| tiktoken_rs::o200k_base().unwrap());

pub fn count_tokens(text: &str) -> u32 {
    ENCODER.encode_ordinary(text).len() as u32
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
}
