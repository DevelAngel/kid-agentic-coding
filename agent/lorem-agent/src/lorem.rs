//! Generates Lorem Ipsum placeholder text for the fake ACP agent.

const WORDS: &[&str] = &[
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "enim",
    "ad",
    "minim",
    "veniam",
    "quis",
    "nostrud",
    "exercitation",
    "ullamco",
    "laboris",
    "nisi",
    "aliquip",
    "ex",
    "ea",
    "commodo",
    "consequat",
];

/// Deterministically picks `count` words from the fixed pool, cycling
/// through it starting at `seed`. Deterministic on purpose: assertions in
/// tests must not flake.
pub fn generate(seed: usize, count: usize) -> String {
    (0..count)
        .map(|i| WORDS[(seed + i) % WORDS.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{WORDS, generate};

    #[test]
    fn generates_requested_word_count() {
        let text = generate(0, 5);
        assert_eq!(text.split(' ').count(), 5);
    }

    #[test]
    fn is_deterministic_for_the_same_seed() {
        assert_eq!(generate(3, 8), generate(3, 8));
    }

    #[test]
    fn wraps_around_the_word_pool() {
        let text = generate(WORDS.len() - 1, 2);
        assert_eq!(text, format!("{} {}", WORDS[WORDS.len() - 1], WORDS[0]));
    }
}
