pub fn generate_lod_slices(full_text: &str) -> Vec<String> {
    let targets = [usize::MAX, 800, 240, 80, 40, usize::MAX];
    let mut slices = Vec::with_capacity(targets.len());
    for &max_chars in &targets {
        if max_chars == usize::MAX || full_text.len() <= max_chars {
            slices.push(full_text.to_string());
        } else {
            // Find the nearest char boundary at or before max_chars
            let idx = if full_text.is_char_boundary(max_chars) {
                max_chars
            } else {
                full_text.floor_char_boundary(max_chars)
            };
            let truncated = &full_text[..idx];
            if let Some(last_period) = truncated.rfind('.') {
                if last_period > idx / 2 {
                    slices.push(full_text[..=last_period].to_string());
                    continue;
                }
            }
            if let Some(last_space) = truncated.rfind(' ') {
                slices.push(full_text[..last_space].to_string());
            } else {
                slices.push(truncated.to_string());
            }
        }
    }
    slices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_lod_slices_short_text() {
        let slices = generate_lod_slices("Hello");
        assert_eq!(slices.len(), 6);
        for s in &slices {
            assert_eq!(s, "Hello");
        }
    }

    #[test]
    fn generate_lod_slices_truncates_summary() {
        let long = "A. ".repeat(500);
        let text = &long[..long.len() - 2];
        let slices = generate_lod_slices(text);
        assert!(slices[0].len() > 800);
        assert!(slices[1].len() <= 800);
        assert!(slices[2].len() <= 240);
        assert!(slices[3].len() <= 80);
        assert!(slices[4].len() <= 40);
    }

    #[test]
    fn generate_lod_slices_sentence_boundary() {
        let text = "First sentence. Second sentence that is longer. Third sentence.";
        let slices = generate_lod_slices(text);
        assert!(slices[1].ends_with('.'));
    }
}
