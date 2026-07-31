use inkstone_workspace::{WorkspaceEntry, WorkspaceEntryKind};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub is_folder: bool,
    pub depth: usize,
    pub children: Vec<TreeNode>,
    pub entry: Option<WorkspaceEntry>,
}

#[derive(Debug, Clone)]
pub struct FlatNode {
    pub name: String,
    pub path: String,
    pub is_folder: bool,
    pub depth: usize,
    pub is_expanded: bool,
    pub kind: WorkspaceEntryKind,
    pub size_bytes: u64,
}

fn insert_entry(
    nodes: &mut Vec<TreeNode>,
    parent_path: &str,
    segments: &[&str],
    depth: usize,
    entry: &WorkspaceEntry,
) {
    if segments.is_empty() {
        return;
    }
    let name = segments[0].to_string();
    let current_path = if parent_path.is_empty() {
        name.clone()
    } else {
        format!("{}/{}", parent_path, name)
    };
    let is_leaf = segments.len() == 1;

    if is_leaf && entry.kind == WorkspaceEntryKind::Folder {
        if !nodes.iter().any(|node| node.name == name && node.is_folder) {
            nodes.push(TreeNode {
                name,
                path: current_path,
                is_folder: true,
                depth,
                children: Vec::new(),
                entry: Some(entry.clone()),
            });
        }
    } else if is_leaf {
        nodes.push(TreeNode {
            name,
            path: current_path,
            is_folder: false,
            depth,
            children: Vec::new(),
            entry: Some(entry.clone()),
        });
    } else if let Some(folder) = nodes.iter_mut().find(|n| n.name == name && n.is_folder) {
        insert_entry(
            &mut folder.children,
            &current_path,
            &segments[1..],
            depth + 1,
            entry,
        );
    } else {
        let mut folder = TreeNode {
            name,
            path: current_path.clone(),
            is_folder: true,
            depth,
            children: Vec::new(),
            entry: None,
        };
        insert_entry(
            &mut folder.children,
            &current_path,
            &segments[1..],
            depth + 1,
            entry,
        );
        nodes.push(folder);
    }
}

fn sort_tree(nodes: &mut Vec<TreeNode>) {
    nodes.sort_by(|a, b| {
        if a.is_folder != b.is_folder {
            return if a.is_folder {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        a.name.cmp(&b.name)
    });
    for node in nodes.iter_mut() {
        sort_tree(&mut node.children);
    }
}

fn flatten_tree(
    nodes: &[TreeNode],
    expanded_folders: &HashSet<String>,
    depth: usize,
    result: &mut Vec<FlatNode>,
) {
    for node in nodes {
        if node.is_folder {
            let is_expanded = expanded_folders.contains(&node.path);
            result.push(FlatNode {
                name: node.name.clone(),
                path: node.path.clone(),
                is_folder: true,
                depth,
                is_expanded,
                kind: WorkspaceEntryKind::Folder,
                size_bytes: 0,
            });
            if is_expanded {
                flatten_tree(&node.children, expanded_folders, depth + 1, result);
            }
        } else {
            let entry = node.entry.as_ref();
            let (kind, size) = match entry {
                Some(e) => (e.kind, e.size_bytes),
                None => (WorkspaceEntryKind::Asset, 0),
            };
            result.push(FlatNode {
                name: node.name.clone(),
                path: node.path.clone(),
                is_folder: false,
                depth,
                is_expanded: false,
                kind,
                size_bytes: size,
            });
        }
    }
}

pub fn build_flat_tree(
    entries: &[WorkspaceEntry],
    expanded_folders: &HashSet<String>,
) -> Vec<FlatNode> {
    let user_entries: Vec<WorkspaceEntry> = entries
        .iter()
        .filter(|e| e.kind != WorkspaceEntryKind::Internal)
        .cloned()
        .collect();

    let mut root_nodes: Vec<TreeNode> = Vec::new();
    for entry in &user_entries {
        let segments: Vec<&str> = entry.path.segments();
        insert_entry(&mut root_nodes, "", &segments, 0, entry);
    }
    sort_tree(&mut root_nodes);

    let mut result = Vec::new();
    flatten_tree(&root_nodes, expanded_folders, 0, &mut result);
    result
}

pub fn parent_folder(path: &str) -> Option<String> {
    let idx = path.rfind('/')?;
    if idx == 0 {
        Some(String::new())
    } else {
        Some(path[..idx].to_string())
    }
}

pub fn file_name_only(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}
