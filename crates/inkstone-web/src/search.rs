use inkstone_core::ids::DocId;

#[derive(Debug, Clone)]
pub struct SearchDocument {
    pub path: String,
    pub file_name: String,
    pub doc_id: Option<DocId>,
    pub title: String,
    pub blocks: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub path: String,
    pub file_name: String,
    pub doc_id: Option<DocId>,
    pub title: String,
    pub snippet: Option<String>,
    pub score: u32,
}

pub fn rank_results(docs: &[SearchDocument], query: &str) -> Vec<SearchResult> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    for doc in docs {
        if let Some(score) = score_doc(doc, &q) {
            let snippet = generate_snippet(doc, &q);
            results.push(SearchResult {
                path: doc.path.clone(),
                file_name: doc.file_name.clone(),
                doc_id: doc.doc_id,
                title: doc.title.clone(),
                snippet,
                score,
            });
        }
    }

    // Sort:
    // 1. Score descending
    // 2. Path ascending
    results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));

    results
}

fn score_doc(doc: &SearchDocument, q: &str) -> Option<u32> {
    let fn_lower = doc.file_name.to_lowercase();
    let path_lower = doc.path.to_lowercase();
    let title_lower = doc.title.to_lowercase();

    if fn_lower == q {
        return Some(1000);
    }
    if fn_lower.starts_with(q) {
        return Some(800);
    }
    if fn_lower.contains(q) {
        return Some(600);
    }
    if path_lower.contains(q) {
        return Some(400);
    }
    if title_lower.contains(q) {
        return Some(200);
    }
    for block in &doc.blocks {
        if block.to_lowercase().contains(q) {
            return Some(100);
        }
    }
    None
}

pub fn find_case_insensitive_range(text: &str, lower_query: &str) -> Option<(usize, usize)> {
    find_case_insensitive_ranges(text, lower_query)
        .into_iter()
        .next()
}

pub fn find_case_insensitive_ranges(text: &str, lower_query: &str) -> Vec<(usize, usize)> {
    if lower_query.is_empty() {
        return Vec::new();
    }
    let mut lower_text = String::with_capacity(text.len());
    let mut offsets = Vec::new();

    for (orig_idx, ch) in text.char_indices() {
        let start_len = lower_text.len();
        lower_text.extend(ch.to_lowercase());
        let end_len = lower_text.len();
        let orig_end = text[orig_idx..]
            .chars()
            .next()
            .map(|c| orig_idx + c.len_utf8())
            .unwrap_or(text.len());
        offsets.push((start_len, end_len, orig_idx, orig_end));
    }
    let mut result = Vec::new();
    let mut lower_offset = 0;
    while let Some(found) = lower_text[lower_offset..].find(lower_query) {
        let start = lower_offset + found;
        let end = start + lower_query.len();
        let first = offsets
            .iter()
            .find(|(a, b, _, _)| *a <= start && start < *b);
        let last = offsets.iter().find(|(a, b, _, _)| *a < end && end <= *b);
        if let (Some((_, _, original_start, _)), Some((_, _, _, original_end))) = (first, last) {
            result.push((*original_start, *original_end));
        }
        lower_offset = end;
    }
    result
}

fn generate_snippet(doc: &SearchDocument, q: &str) -> Option<String> {
    for block in &doc.blocks {
        if let Some((match_start, match_end)) = find_case_insensitive_range(block, q) {
            let char_start = block[..match_start].chars().count();
            let query_char_len = block[match_start..match_end].chars().count();
            let total_chars = block.chars().count();
            let context: usize = 30;

            // Window in char space.
            let window_start = char_start.saturating_sub(context);
            let window_end = (char_start + query_char_len + context).min(total_chars);

            // Convert back to byte offsets using char_indices on the *original* string.
            let mut byte_start = block.len();
            let mut byte_end = block.len();
            for (byte_off, _ch) in block.char_indices() {
                let ci = block[..byte_off].chars().count();
                if ci == window_start {
                    byte_start = byte_off;
                }
                if ci == window_end {
                    byte_end = byte_off;
                    break;
                }
            }

            let mut snippet = String::new();
            if window_start > 0 {
                snippet.push_str("...");
            }
            snippet.push_str(&block[byte_start..byte_end]);
            if window_end < total_chars {
                snippet.push_str("...");
            }
            return Some(snippet);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_scoring_and_ranking() {
        let docs = vec![
            SearchDocument {
                path: "Projects/Inkstone/Roadmap.md".to_string(),
                file_name: "Roadmap.md".to_string(),
                doc_id: None,
                title: "Inkstone Project Roadmap".to_string(),
                blocks: vec!["This contains some text.".to_string()],
            },
            SearchDocument {
                path: "Drafts/Roadmap.md".to_string(),
                file_name: "Roadmap.md".to_string(),
                doc_id: None,
                title: "Draft".to_string(),
                blocks: vec!["Other stuff.".to_string()],
            },
            SearchDocument {
                path: "Notes.md".to_string(),
                file_name: "Notes.md".to_string(),
                doc_id: None,
                title: "General Notes".to_string(),
                blocks: vec!["Roadmap is here inside content.".to_string()],
            },
        ];

        let results = rank_results(&docs, "roadmap");
        assert_eq!(results.len(), 3);
        // Alphabetically sorted for equal scores: Drafts/Roadmap.md then Projects/Inkstone/Roadmap.md
        // "roadmap" starts-with matches "Roadmap.md" → score 800
        assert_eq!(results[0].path, "Drafts/Roadmap.md");
        assert_eq!(results[0].score, 800);
        assert_eq!(results[1].path, "Projects/Inkstone/Roadmap.md");
        assert_eq!(results[1].score, 800);
        assert_eq!(results[2].path, "Notes.md");
        assert_eq!(results[2].score, 100);
    }

    #[test]
    fn test_snippet_generation() {
        let doc = SearchDocument {
            path: "Ideas.md".to_string(),
            file_name: "Ideas.md".to_string(),
            doc_id: None,
            title: "".to_string(),
            blocks: vec!["The quick brown fox jumps over the lazy dog repeatedly.".to_string()],
        };

        let snippet = generate_snippet(&doc, "lazy");
        assert!(snippet.is_some());
        let s = snippet.unwrap();
        assert!(s.contains("lazy"));
        assert!(s.contains("fox"));
    }

    #[test]
    fn test_snippet_unicode_accented() {
        let doc = SearchDocument {
            path: "Français.md".to_string(),
            file_name: "Français.md".to_string(),
            doc_id: None,
            title: "".to_string(),
            blocks: vec!["Le résumé complet de l'été passé en Île-de-France.".to_string()],
        };
        let snippet = generate_snippet(&doc, "résumé");
        assert!(snippet.is_some(), "should find accented word");
        let s = snippet.unwrap();
        assert!(s.contains("résumé"), "snippet should contain match");
    }

    #[test]
    fn test_snippet_emoji() {
        let doc = SearchDocument {
            path: "Emoji.md".to_string(),
            file_name: "Emoji.md".to_string(),
            doc_id: None,
            title: "".to_string(),
            blocks: vec!["Hello 🌍 this is a test with emoji 🚀 and more text.".to_string()],
        };
        let snippet = generate_snippet(&doc, "emoji");
        assert!(snippet.is_some(), "should find word near emoji");
        let s = snippet.unwrap();
        assert!(s.contains("emoji"), "snippet should contain matched word");
    }

    #[test]
    fn test_empty_query_returns_nothing() {
        let docs = vec![SearchDocument {
            path: "A.md".to_string(),
            file_name: "A.md".to_string(),
            doc_id: None,
            title: "Title".to_string(),
            blocks: vec!["content".to_string()],
        }];
        let results = rank_results(&docs, "");
        assert!(results.is_empty(), "empty query must return empty results");
    }

    #[test]
    fn test_case_insensitive_path_search() {
        let docs = vec![SearchDocument {
            path: "Notes/MyProject.md".to_string(),
            file_name: "MyProject.md".to_string(),
            doc_id: None,
            title: "".to_string(),
            blocks: vec![],
        }];
        let results = rank_results(&docs, "MYPROJECT");
        assert_eq!(results.len(), 1);
        assert!(
            results[0].score >= 600,
            "should match file name case-insensitively"
        );
    }

    #[test]
    fn test_title_search() {
        let docs = vec![SearchDocument {
            path: "Untitled.md".to_string(),
            file_name: "Untitled.md".to_string(),
            doc_id: None,
            title: "Annual Review".to_string(),
            blocks: vec![],
        }];
        let results = rank_results(&docs, "annual");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].score, 200);
    }

    #[test]
    fn test_content_search() {
        let docs = vec![SearchDocument {
            path: "Misc.md".to_string(),
            file_name: "Misc.md".to_string(),
            doc_id: None,
            title: "".to_string(),
            blocks: vec!["This note discusses cryptography at length.".to_string()],
        }];
        let results = rank_results(&docs, "cryptography");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].score, 100);
    }

    #[test]
    fn test_deterministic_tie_ordering() {
        let docs = vec![
            SearchDocument {
                path: "Z/Note.md".to_string(),
                file_name: "Note.md".to_string(),
                doc_id: None,
                title: "".to_string(),
                blocks: vec![],
            },
            SearchDocument {
                path: "A/Note.md".to_string(),
                file_name: "Note.md".to_string(),
                doc_id: None,
                title: "".to_string(),
                blocks: vec![],
            },
        ];
        let results = rank_results(&docs, "note");
        assert_eq!(results.len(), 2);
        // Same score; A/Note.md sorts before Z/Note.md
        assert_eq!(results[0].path, "A/Note.md");
        assert_eq!(results[1].path, "Z/Note.md");
    }

    #[test]
    fn test_find_case_insensitive_range() {
        let text = "İstanbul 🌍 Roadmap";
        let query = "roadmap";
        let res = find_case_insensitive_range(text, query);
        assert!(res.is_some());
        let (start, end) = res.unwrap();
        assert_eq!(&text[start..end], "Roadmap");
    }

    #[test]
    fn test_repeated_unicode_safe_ranges() {
        let ranges = find_case_insensitive_ranges("İstanbul 🌍 Roadmap roadmap", "roadmap");
        assert_eq!(ranges.len(), 2);
        assert_eq!(
            &"İstanbul 🌍 Roadmap roadmap"[ranges[0].0..ranges[0].1],
            "Roadmap"
        );
        assert_eq!(
            &"İstanbul 🌍 Roadmap roadmap"[ranges[1].0..ranges[1].1],
            "roadmap"
        );
    }
}
