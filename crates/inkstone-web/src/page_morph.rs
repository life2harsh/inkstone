use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use wasm_bindgen::JsCast;

const SIG: &str = "data-sig";
const CONTAINERS: [&str; 2] = ["editor-container", "page-surface"];

fn children(parent: &web_sys::Element) -> Vec<web_sys::Element> {
    let list = parent.children();
    (0..list.length())
        .filter_map(|index| list.item(index))
        .collect()
}

fn sig(element: &web_sys::Element) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    element.outer_html().hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn keys(list: &[web_sys::Element]) -> Vec<String> {
    let mut seen = HashMap::<String, usize>::new();
    list.iter()
        .map(
            |element| match element.get_attribute("data-page-block-id") {
                Some(id) => format!("block:{id}"),
                None => {
                    let base = format!(
                        "{}.{}",
                        element.tag_name(),
                        element.class_name().split(' ').next().unwrap_or_default()
                    );
                    let count = seen.entry(base.clone()).or_default();
                    *count += 1;
                    format!("{base}#{count}")
                }
            },
        )
        .collect()
}

fn own_text(element: &web_sys::Element) -> String {
    let nodes = element.child_nodes();
    (0..nodes.length())
        .filter_map(|index| nodes.item(index))
        .filter(|node| node.node_type() == web_sys::Node::TEXT_NODE)
        .filter_map(|node| node.text_content())
        .collect()
}

fn sync_attributes(live: &web_sys::Element, fresh: &web_sys::Element) {
    for name in live
        .get_attribute_names()
        .iter()
        .filter_map(|name| name.as_string())
    {
        if name != SIG && !fresh.has_attribute(&name) {
            let _ = live.remove_attribute(&name);
        }
    }
    for name in fresh
        .get_attribute_names()
        .iter()
        .filter_map(|name| name.as_string())
    {
        let value = fresh.get_attribute(&name).unwrap_or_default();
        if live.get_attribute(&name).as_deref() != Some(value.as_str()) {
            let _ = live.set_attribute(&name, &value);
        }
    }
    if let Some(input) = live.dyn_ref::<web_sys::HtmlInputElement>() {
        input.set_checked(fresh.has_attribute("checked"));
    }
}

fn place(
    parent: &web_sys::Element,
    nodes: &[web_sys::Element],
    mut cursor: Option<web_sys::Element>,
) {
    for node in nodes {
        if cursor.as_ref() == Some(node) {
            cursor = node.next_element_sibling();
        } else {
            let _ = parent.insert_before(node, cursor.as_ref().map(|c| c.unchecked_ref()));
        }
    }
}

pub(crate) fn morph(
    live: &web_sys::Element,
    fresh: &web_sys::Element,
    keep: Option<&web_sys::Element>,
) {
    let old = children(live);
    let old_keys = keys(&old);
    let new = children(fresh);
    let mut by_key: HashMap<String, web_sys::Element> = old_keys.into_iter().zip(old).collect();
    let mut want = Vec::with_capacity(new.len());
    for (key, node) in keys(&new).into_iter().zip(new) {
        let chosen = match by_key.remove(&key) {
            Some(current) if keep == Some(&current) => {
                sync_attributes(&current, &node);
                current
            }
            Some(current)
                if (CONTAINERS
                    .iter()
                    .any(|class| node.class_list().contains(class))
                    || keep.is_some_and(|keep| current.contains(Some(keep))))
                    && own_text(&current) == own_text(&node) =>
            {
                sync_attributes(&current, &node);
                let _ = current.remove_attribute(SIG);
                morph(&current, &node, keep);
                current
            }
            current => {
                let fresh_sig = sig(&node);
                let chosen = match current {
                    Some(current) if current.get_attribute(SIG).as_deref() == Some(&fresh_sig) => {
                        current
                    }
                    stale => {
                        if let Some(stale) = stale {
                            stale.remove();
                        }
                        node
                    }
                };
                let _ = chosen.set_attribute(SIG, &fresh_sig);
                chosen
            }
        };
        want.push(chosen);
    }
    for stale in by_key.into_values() {
        stale.remove();
    }
    match keep.and_then(|keep| {
        want.iter()
            .position(|node| node == keep || node.contains(Some(keep)))
    }) {
        Some(index) => {
            let anchor = want[index].clone();
            place(live, &want[..index], live.first_element_child());
            place(live, &want[index + 1..], anchor.next_element_sibling());
        }
        None => place(live, &want, live.first_element_child()),
    }
}
