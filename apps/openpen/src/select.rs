//! Pure selection rules (Pen/Figma-like), independent of GPUI and pen-doc.
//!
//! A click resolves the *hit path* (root → deepest visible node under the
//! cursor) to the node that gets selected:
//! - a selected node on the path stays selected (so it can be dragged);
//! - otherwise the sibling of a selected node on the path (the user already
//!   "entered" that container);
//! - otherwise top-level frames act as artboards: their direct child is
//!   picked; any other root is picked as a whole;
//! - Ctrl/Cmd (deep select) picks the leaf;
//! - double click enters one level below the selected container.

/// Tree queries the rules need (implemented by the scene document).
pub trait PickTree<K: Copy + PartialEq> {
    fn parent(&self, k: K) -> Option<K>;
    /// Top-level frame whose children are selected directly (Pen artboard).
    fn is_artboard(&self, k: K) -> bool;
}

/// Root → `leaf` chain.
pub fn ancestry<K: Copy + PartialEq>(tree: &dyn PickTree<K>, leaf: K) -> Vec<K> {
    let mut v = vec![leaf];
    let mut cur = tree.parent(leaf);
    while let Some(p) = cur {
        v.push(p);
        cur = tree.parent(p);
    }
    v.reverse();
    v
}

/// Node selected by a click on `path` (root → leaf).
pub fn pick<K: Copy + PartialEq>(
    tree: &dyn PickTree<K>,
    path: &[K],
    selection: &[K],
    deep: bool,
) -> Option<K> {
    let leaf = *path.last()?;
    if deep {
        return Some(leaf);
    }
    if let Some(&s) = path.iter().rev().find(|k| selection.contains(k)) {
        return Some(s);
    }
    for s in selection {
        if let Some(p) = tree.parent(*s)
            && let Some(i) = path.iter().position(|k| *k == p)
            && let Some(&k) = path.get(i + 1)
        {
            return Some(k);
        }
    }
    if path.len() > 1 && tree.is_artboard(path[0]) {
        return Some(path[1]);
    }
    Some(path[0])
}

/// Double click: the child (on the path) of the selected node under the
/// cursor; falls back to a normal pick.
pub fn pick_enter<K: Copy + PartialEq>(
    tree: &dyn PickTree<K>,
    path: &[K],
    selection: &[K],
) -> Option<K> {
    if let Some(i) = path.iter().position(|k| selection.contains(k)) {
        return path.get(i + 1).or(path.get(i)).copied();
    }
    pick(tree, path, selection, false)
}

/// Click selection update: `additive` (Shift) toggles `k`, otherwise `k`
/// alone. Clicking an already selected node without Shift keeps the whole
/// selection (it may be the start of a drag).
pub fn click_select<K: Copy + PartialEq>(selection: &[K], k: K, additive: bool) -> Vec<K> {
    let mut out = selection.to_vec();
    if additive {
        if let Some(i) = out.iter().position(|s| *s == k) {
            out.remove(i);
        } else {
            out.push(k);
        }
        out
    } else if out.contains(&k) {
        out
    } else {
        vec![k]
    }
}

/// Removes nodes whose ancestor is also selected (moving/deleting a parent
/// already covers them).
pub fn top_level_only<K: Copy + PartialEq>(tree: &dyn PickTree<K>, sel: &[K]) -> Vec<K> {
    sel.iter()
        .copied()
        .filter(|&k| {
            let mut cur = tree.parent(k);
            while let Some(p) = cur {
                if sel.contains(&p) {
                    return false;
                }
                cur = tree.parent(p);
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0 = artboard frame { 1 = card frame { 2 = text, 3 = rect } }, 4 = group { 5 = rect }
    struct T;
    impl PickTree<u32> for T {
        fn parent(&self, k: u32) -> Option<u32> {
            match k {
                1 => Some(0),
                2 | 3 => Some(1),
                5 => Some(4),
                _ => None,
            }
        }
        fn is_artboard(&self, k: u32) -> bool {
            k == 0
        }
    }

    #[test]
    fn ancestry_root_first() {
        assert_eq!(ancestry(&T, 2), vec![0, 1, 2]);
        assert_eq!(ancestry(&T, 4), vec![4]);
    }

    #[test]
    fn default_pick_uses_artboard_children() {
        let path = ancestry(&T, 2);
        assert_eq!(pick(&T, &path, &[], false), Some(1));
        // Clicking the artboard background selects the artboard.
        assert_eq!(pick(&T, &[0], &[], false), Some(0));
        // A root group is picked as a whole.
        assert_eq!(pick(&T, &ancestry(&T, 5), &[], false), Some(4));
    }

    #[test]
    fn deep_and_sibling_levels() {
        let path = ancestry(&T, 2);
        assert_eq!(pick(&T, &path, &[], true), Some(2));
        // With 3 selected (inside card), clicking text selects its sibling 2.
        assert_eq!(pick(&T, &path, &[3], false), Some(2));
        // A selected ancestor on the path stays selected.
        assert_eq!(pick(&T, &path, &[1], false), Some(1));
        assert_eq!(pick(&T, &[], &[1], false), None);
    }

    #[test]
    fn double_click_enters_one_level() {
        let path = ancestry(&T, 2);
        assert_eq!(pick_enter(&T, &path, &[1]), Some(2));
        assert_eq!(pick_enter(&T, &path, &[0]), Some(1));
        // Leaf already selected: stays.
        assert_eq!(pick_enter(&T, &path, &[2]), Some(2));
        assert_eq!(pick_enter(&T, &path, &[]), Some(1));
    }

    #[test]
    fn click_selection_updates() {
        assert_eq!(click_select(&[1u32], 2, false), vec![2]);
        assert_eq!(click_select(&[1u32, 2], 2, false), vec![1, 2]);
        assert_eq!(click_select(&[1u32], 2, true), vec![1, 2]);
        assert_eq!(click_select(&[1u32, 2], 1, true), vec![2]);
        assert_eq!(top_level_only(&T, &[0, 2, 4]), vec![0, 4]);
    }
}
