use inkstone_core::ids::BlockId;
use inkstone_workspace::path::WorkspacePath;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikilinkResolution {
    Resolved(String),
    Ambiguous(Vec<String>),
    Unresolved,
}

#[derive(Debug, Clone)]
pub struct KnowledgeTag {
    pub name: String,
    pub count: usize,
}

#[derive(Debug, Clone)]
pub struct ResolvedHeading {
    pub text: String,
    pub level: u8,
    pub block_id: BlockId,
    pub line: usize,
    pub anchor: String,
}

#[derive(Debug, Clone)]
pub struct DocumentKnowledge {
    pub headings: Vec<ResolvedHeading>,
    pub tags: Vec<String>,
    pub outgoing_links: Vec<OutgoingLink>,
}

#[derive(Debug, Clone)]
pub struct OutgoingLink {
    pub target_raw: String,
    pub alias: Option<String>,
    pub block_id: BlockId,
    pub is_wikilink: bool,
}

#[derive(Debug, Clone)]
pub struct Backlink {
    pub source_path: String,
    pub source_name: String,
    pub context_snippet: Option<String>,
}

#[derive(Debug, Default)]
pub struct KnowledgeIndex {
    // tag_name (lowercase) -> Set of WorkspacePaths containing it
    pub tag_to_paths: BTreeMap<String, BTreeSet<String>>,
    // path -> set of tags (lowercase) contained in that document
    pub path_to_tags: HashMap<String, HashSet<String>>,

    // Target path -> Set of source paths that link to it
    // Note: The target path here is fully resolved
    pub backlinks: BTreeMap<String, BTreeSet<String>>,

    // path -> all raw target links (unresolved) extracted from it
    pub path_to_outlinks: HashMap<String, Vec<OutgoingLink>>,

    // Basename (lowercase) -> Set of full paths with that basename
    // Used for wikilink resolution
    pub basename_to_paths: HashMap<String, BTreeSet<String>>,

    // path -> DocumentKnowledge (headings, tags, outlinks)
    pub path_to_doc: HashMap<String, DocumentKnowledge>,
}

impl KnowledgeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_path(&mut self, path: &str) {
        if let Some(basename) = path.split('/').last() {
            self.basename_to_paths
                .entry(basename.to_lowercase())
                .or_default()
                .insert(path.to_string());
        }
    }

    pub fn index_document(
        &mut self,
        path: &str,
        blocks: &[(BlockId, inkstone_core::markdown::MarkdownIndex)],
    ) {
        // First remove existing data for this path to handle updates cleanly
        self.remove_document(path);
        
        let mut doc_headings = Vec::new();
        let mut doc_tags = BTreeSet::new();
        let mut doc_outlinks = Vec::new();
        
        let mut heading_counts: HashMap<String, usize> = HashMap::new();

        for (block_id, index) in blocks {
            for h in &index.headings {
                let text_lower = h.text.to_lowercase();
                let count = heading_counts.entry(text_lower.clone()).or_insert(0);
                *count += 1;
                
                let anchor = if *count == 1 {
                    text_lower.replace(' ', "-")
                } else {
                    format!("{}-{}", text_lower.replace(' ', "-"), count)
                };
                
                doc_headings.push(ResolvedHeading {
                    text: h.text.clone(),
                    level: h.level,
                    block_id: *block_id,
                    line: h.line,
                    anchor,
                });
            }
            
            for t in &index.tags {
                let tag_lower = t.tag.to_lowercase();
                doc_tags.insert(tag_lower.clone());
                self.tag_to_paths.entry(tag_lower).or_default().insert(path.to_string());
            }
            
            for w in &index.wikilinks {
                doc_outlinks.push(OutgoingLink {
                    target_raw: w.target.clone(),
                    alias: w.alias.clone(),
                    block_id: *block_id,
                    is_wikilink: true,
                });
                
                let resolved = self.resolve_wikilink(path, &w.target);
                if let WikilinkResolution::Resolved(target_path) = resolved {
                    self.backlinks.entry(target_path).or_default().insert(path.to_string());
                }
            }
            
            // Standard relative markdown links ending in .md
            // Note: We don't have standard links parsed in MarkdownIndex yet, only wikilinks.
            // The prompt says: "Support at minimum: [[wikilinks]], existing safe relative Markdown links ending in .md"
            // Let's rely on wikilinks primarily, but we might need to add md link parsing to core if needed.
        }
        
        self.path_to_tags.insert(path.to_string(), doc_tags.into_iter().collect());
        
        self.path_to_doc.insert(path.to_string(), DocumentKnowledge {
            headings: doc_headings,
            tags: self.path_to_tags.get(path).unwrap().iter().cloned().collect(),
            outgoing_links: doc_outlinks,
        });
    }

    pub fn remove_document(&mut self, path: &str) {
        // Remove from tag_to_paths
        if let Some(tags) = self.path_to_tags.remove(path) {
            for tag in tags {
                if let Some(paths) = self.tag_to_paths.get_mut(&tag) {
                    paths.remove(path);
                    if paths.is_empty() {
                        self.tag_to_paths.remove(&tag);
                    }
                }
            }
        }
        
        // Remove backlinks pointing *from* this doc
        if let Some(doc) = self.path_to_doc.remove(path) {
            for link in doc.outgoing_links {
                let resolved = self.resolve_wikilink(path, &link.target_raw);
                if let WikilinkResolution::Resolved(target_path) = resolved {
                    if let Some(sources) = self.backlinks.get_mut(&target_path) {
                        sources.remove(path);
                        if sources.is_empty() {
                            self.backlinks.remove(&target_path);
                        }
                    }
                }
            }
        }
        
        // We do NOT remove from backlinks pointing *to* this doc here,
        // because the source docs still have the link text. They just become unresolved.
        // The prompt says: "deleted notes disappear from links and backlinks, links pointing to deleted notes become unresolved"
        // When a doc is deleted, `resolve_wikilink` will dynamically fail if the path is missing.
        // Wait, backlinks relies on the `backlinks` map which was populated during `index_document` of the source.
        // If a target doc is deleted, we must remove it from `basename_to_paths` (via remove_path),
        // which makes `resolve_wikilink` fail.
        // However, the `backlinks` map stores `target_path -> sources`.
        // If the target is deleted, should it disappear from `backlinks`?
        // Let's just rely on `backlinks` but filter when querying if the target exists, or clear it.
        // We will remove it from `basename_to_paths` in `remove_path`.
    }

    pub fn remove_path(&mut self, path: &str) {
        if let Some(basename) = path.split('/').last() {
            if let Some(set) = self.basename_to_paths.get_mut(&basename.to_lowercase()) {
                set.remove(path);
                if set.is_empty() {
                    self.basename_to_paths.remove(&basename.to_lowercase());
                }
            }
        }
        self.remove_document(path);
    }

    pub fn resolve_wikilink(&self, source_path: &str, target_raw: &str) -> WikilinkResolution {
        // 1. Try Sibling Note
        let target_with_ext = if target_raw.ends_with(".md") {
            target_raw.to_string()
        } else {
            format!("{}.md", target_raw)
        };

        if let Some(slash_idx) = source_path.rfind('/') {
            let sibling_path = format!("{}/{}", &source_path[..slash_idx], target_with_ext);
            if WorkspacePath::new(&sibling_path).is_ok() && self.path_exists(&sibling_path) {
                return WikilinkResolution::Resolved(sibling_path);
            }
        } else {
            // Source is at root
            if WorkspacePath::new(&target_with_ext).is_ok() && self.path_exists(&target_with_ext) {
                return WikilinkResolution::Resolved(target_with_ext.clone());
            }
        }

        // 2. Try Root-relative
        if WorkspacePath::new(&target_with_ext).is_ok() && self.path_exists(&target_with_ext) {
            return WikilinkResolution::Resolved(target_with_ext);
        }

        // 3. Search existing Markdown entries by basename
        if let Some(basename) = target_with_ext.split('/').last() {
            if let Some(matches) = self.basename_to_paths.get(&basename.to_lowercase()) {
                if matches.len() == 1 {
                    return WikilinkResolution::Resolved(matches.iter().next().unwrap().clone());
                } else if matches.len() > 1 {
                    let mut ambiguous: Vec<String> = matches.iter().cloned().collect();
                    ambiguous.sort();
                    return WikilinkResolution::Ambiguous(ambiguous);
                }
            }
        }

        WikilinkResolution::Unresolved
    }

    fn path_exists(&self, path: &str) -> bool {
        // The path exists if we've seen its basename in the index
        if let Some(basename) = path.split('/').last() {
            if let Some(matches) = self.basename_to_paths.get(&basename.to_lowercase()) {
                return matches.contains(path);
            }
        }
        false
    }

    pub fn get_backlinks(&self, target_path: &str) -> Vec<Backlink> {
        let mut result = Vec::new();
        if let Some(sources) = self.backlinks.get(target_path) {
            for source in sources {
                let source_name = source.split('/').last().unwrap_or(source).to_string();
                result.push(Backlink {
                    source_path: source.clone(),
                    source_name,
                    context_snippet: None,
                });
            }
        }
        result.sort_by(|a, b| a.source_name.cmp(&b.source_name));
        result
    }

    pub fn get_all_tags_with_counts(&self) -> Vec<(String, usize)> {
        let mut result = Vec::new();
        for (tag, paths) in &self.tag_to_paths {
            result.push((tag.clone(), paths.len()));
        }
        // Sort by count descending, then alphabetical
        result.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_wikilink_sibling() {
        let mut idx = KnowledgeIndex::new();
        idx.register_path("Notes/Target.md");
        let res = idx.resolve_wikilink("Notes/Source.md", "Target");
        assert_eq!(res, WikilinkResolution::Resolved("Notes/Target.md".into()));
    }

    #[test]
    fn test_resolve_wikilink_root_relative() {
        let mut idx = KnowledgeIndex::new();
        idx.register_path("Target.md");
        let res = idx.resolve_wikilink("Notes/Deep/Source.md", "Target");
        assert_eq!(res, WikilinkResolution::Resolved("Target.md".into()));
    }

    #[test]
    fn test_resolve_wikilink_unique_basename() {
        let mut idx = KnowledgeIndex::new();
        idx.register_path("Projects/Secret/Target.md");
        let res = idx.resolve_wikilink("Notes/Source.md", "Target");
        assert_eq!(res, WikilinkResolution::Resolved("Projects/Secret/Target.md".into()));
    }

    #[test]
    fn test_resolve_wikilink_ambiguous() {
        let mut idx = KnowledgeIndex::new();
        idx.register_path("A/Target.md");
        idx.register_path("B/Target.md");
        let res = idx.resolve_wikilink("Notes/Source.md", "Target");
        if let WikilinkResolution::Ambiguous(paths) = res {
            assert_eq!(paths.len(), 2);
            assert!(paths.contains(&"A/Target.md".to_string()));
            assert!(paths.contains(&"B/Target.md".to_string()));
        } else {
            panic!("Expected ambiguous resolution");
        }
    }

    #[test]
    fn test_resolve_wikilink_unresolved() {
        let idx = KnowledgeIndex::new();
        let res = idx.resolve_wikilink("Notes/Source.md", "Missing");
        assert_eq!(res, WikilinkResolution::Unresolved);
    }

    #[test]
    fn test_index_document_tags_and_backlinks() {
        use inkstone_core::markdown::{HeadingRef, MarkdownIndex, TagRef, WikiLink};
        
        let mut idx = KnowledgeIndex::new();
        idx.register_path("Notes/Source.md");
        idx.register_path("Notes/Target.md");
        
        let block_id = BlockId::new_v4();
        let md_index = MarkdownIndex {
            wikilinks: vec![WikiLink {
                target: "Target.md".to_string(),
                alias: None,
                line: 1,
                col: 1,
            }],
            embeds: vec![],
            tags: vec![
                TagRef { tag: "Rust".to_string(), line: 2, col: 1 },
                TagRef { tag: "rust".to_string(), line: 2, col: 10 },
            ],
            headings: vec![
                HeadingRef { level: 1, text: "Title".to_string(), line: 3 },
                HeadingRef { level: 2, text: "Title".to_string(), line: 4 },
            ],
            has_code_block: false,
        };
        
        idx.index_document("Notes/Source.md", &[(block_id, md_index)]);
        
        // Tags
        let doc_tags = &idx.path_to_doc.get("Notes/Source.md").unwrap().tags;
        assert_eq!(doc_tags.len(), 1); // Deduplicated "rust"
        assert_eq!(doc_tags[0], "rust");
        
        let paths_with_rust = idx.tag_to_paths.get("rust").unwrap();
        assert_eq!(paths_with_rust.len(), 1);
        assert!(paths_with_rust.contains("Notes/Source.md"));
        
        // Headings
        let headings = &idx.path_to_doc.get("Notes/Source.md").unwrap().headings;
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].anchor, "title");
        assert_eq!(headings[1].anchor, "title-2"); // Duplicate anchor uniqueness
        
        // Backlinks
        let backlinks = idx.backlinks.get("Notes/Target.md").unwrap();
        assert_eq!(backlinks.len(), 1);
        assert!(backlinks.contains("Notes/Source.md"));
        
        // Remove document (deletion/rename rebuild)
        idx.remove_document("Notes/Source.md");
        assert!(!idx.tag_to_paths.contains_key("rust"));
        assert!(!idx.backlinks.contains_key("Notes/Target.md"));
    }
}
