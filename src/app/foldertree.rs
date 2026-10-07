//! Far's "Find folder" (Alt+F10, `foldtree.cpp`, `treelist.cpp`): the
//! tree of the active panel's drive in a window, read in the background
//! (and kept for the session; Ctrl+R reads it again); typing searches the
//! folders by the start of their names (Ctrl+Enter — the next one),
//! Enter goes to the folder.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

use super::fileops::Overlay;
use super::{App, AppMsg, Focus};
use crate::theme;
use crate::tr;

/// A folder of the tree, in reading order.
#[derive(Clone, Debug)]
pub struct Node {
    pub depth: u16,
    pub path: PathBuf,
}

impl Node {
    fn name(&self) -> String {
        if self.depth == 0 {
            return self.path.display().to_string();
        }
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// A drive's tree as read so far, with what drawing it needs.
#[derive(Clone, Default)]
pub(super) struct Tree {
    nodes: Vec<Node>,
    /// Per node: a later sibling follows (`├` rather than `└`).
    more: Vec<bool>,
    /// Per node: its parent's index.
    parent: Vec<usize>,
    /// The last node seen at each depth.
    stack: Vec<usize>,
    pub done: bool,
}

impl Tree {
    fn push(&mut self, node: Node) {
        let i = self.nodes.len();
        let d = usize::from(node.depth);
        self.stack.truncate(d + 1);
        if let Some(&j) = self.stack.get(d) {
            self.more[j] = true;
        }
        let parent = if d == 0 {
            i
        } else {
            self.stack.get(d - 1).copied().unwrap_or(0)
        };
        if self.stack.len() == d {
            self.stack.push(i);
        } else {
            self.stack[d] = i;
        }
        self.nodes.push(node);
        self.more.push(false);
        self.parent.push(parent);
    }

    /// The drawing of a line: `│ ` / `  ` for each ancestor, `├─` / `└─`.
    fn prefix(&self, i: usize) -> String {
        let d = self.nodes[i].depth;
        if d == 0 {
            return String::new();
        }
        let mut parts = Vec::new();
        let mut p = self.parent[i];
        for _ in 1..d {
            parts.push(if self.more[p] { "│ " } else { "  " });
            p = self.parent[p];
        }
        parts.reverse();
        let mut s: String = parts.concat();
        s.push_str(if self.more[i] { "├─" } else { "└─" });
        s
    }
}

pub(super) struct FolderTree {
    pub root: PathBuf,
    current: usize,
    top: usize,
    search: String,
    /// The folder to put the cursor on once it is read.
    want: Option<PathBuf>,
    cancel: Arc<AtomicBool>,
    list: Rect,
}

impl Drop for FolderTree {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Reads the tree under `root` in reading order (names sorted, links not
/// followed), sending it in batches.
fn read_tree(root: PathBuf, cancel: Arc<AtomicBool>, tx: std::sync::mpsc::Sender<AppMsg>) {
    let mut batch = vec![Node {
        depth: 0,
        path: root.clone(),
    }];
    let mut stack: Vec<(PathBuf, u16)> = vec![(root.clone(), 0)];
    let mut last = std::time::Instant::now();
    while let Some((dir, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if dir != root || depth > 0 {
            batch.push(Node {
                depth,
                path: dir.clone(),
            });
        }
        let mut subs: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        subs.sort_by_key(|p| p.to_string_lossy().to_lowercase());
        stack.extend(subs.into_iter().rev().map(|p| (p, depth + 1)));
        if last.elapsed() > std::time::Duration::from_millis(150) {
            let _ = tx.send(AppMsg::TreeRead(
                root.clone(),
                std::mem::take(&mut batch),
                false,
            ));
            last = std::time::Instant::now();
        }
    }
    let _ = tx.send(AppMsg::TreeRead(root, batch, true));
}

impl App {
    /// Alt+F10: the tree of the active panel's drive, the cursor on its
    /// folder.
    pub(super) fn folder_tree(&mut self) {
        let here = self.panels[self.active].path.clone();
        let Some(root) = here.ancestors().last().map(PathBuf::from) else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        if !self.trees.get(&root).is_some_and(|t| t.done) {
            self.trees.insert(root.clone(), Tree::default());
            let (tx, c, r) = (self.tx.clone(), cancel.clone(), root.clone());
            std::thread::spawn(move || read_tree(r, c, tx));
        }
        let mut ft = FolderTree {
            root: root.clone(),
            current: 0,
            top: 0,
            search: String::new(),
            want: Some(here),
            cancel,
            list: Rect::default(),
        };
        self.tree_find_wanted(&mut ft);
        self.overlays.push(Overlay::FolderTree(Box::new(ft)));
    }

    /// Puts the cursor on the wanted folder once it is in the tree.
    fn tree_find_wanted(&self, ft: &mut FolderTree) {
        let Some(want) = &ft.want else { return };
        let Some(tree) = self.trees.get(&ft.root) else {
            return;
        };
        let lower = want.to_string_lossy().to_lowercase();
        if let Some(i) = tree
            .nodes
            .iter()
            .position(|n| n.path.to_string_lossy().to_lowercase() == lower)
        {
            ft.current = i;
            ft.want = None;
        }
    }

    /// A part of a tree read.
    pub(super) fn tree_read(&mut self, root: PathBuf, nodes: Vec<Node>, done: bool) {
        let tree = self.trees.entry(root.clone()).or_default();
        for n in nodes {
            tree.push(n);
        }
        tree.done = done;
        // The window's cursor waits for its folder.
        let mut ft = None;
        if let Some(Overlay::FolderTree(t)) = self.overlays.last_mut()
            && t.root == root
            && t.want.is_some()
        {
            ft = Some(std::mem::replace(
                t,
                Box::new(FolderTree {
                    root: PathBuf::new(),
                    current: 0,
                    top: 0,
                    search: String::new(),
                    want: None,
                    cancel: Arc::new(AtomicBool::new(false)),
                    list: Rect::default(),
                }),
            ));
        }
        if let Some(mut t) = ft {
            self.tree_find_wanted(&mut t);
            if let Some(Overlay::FolderTree(slot)) = self.overlays.last_mut() {
                *slot = t;
            }
        }
    }

    pub(super) fn folder_tree_key(&mut self, key: &KeyEvent) {
        let Some(Overlay::FolderTree(ft)) = self.overlays.last_mut() else {
            return;
        };
        let Some(tree) = self.trees.get(&ft.root) else {
            return;
        };
        let n = tree.nodes.len();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = usize::from(ft.list.height.max(1));
        let mut moved = true;
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => {
                self.overlays.pop();
                return;
            }
            KeyCode::Enter if ctrl => {
                // The next folder matching the search.
                if let Some(i) = find_from(tree, &ft.search, ft.current + 1) {
                    ft.current = i;
                }
                return;
            }
            KeyCode::Enter => {
                let path = tree.nodes.get(ft.current).map(|nd| nd.path.clone());
                self.overlays.pop();
                if let Some(p) = path {
                    self.focus = Focus::Panels;
                    let side = self.active;
                    self.change_dir(side, &p);
                }
                return;
            }
            KeyCode::Char('r') if ctrl => {
                let root = ft.root.clone();
                self.trees.remove(&root);
                self.overlays.pop();
                self.folder_tree();
                return;
            }
            KeyCode::Up => ft.current = ft.current.saturating_sub(1),
            KeyCode::Down => ft.current = (ft.current + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => ft.current = ft.current.saturating_sub(page),
            KeyCode::PageDown => ft.current = (ft.current + page).min(n.saturating_sub(1)),
            KeyCode::Home => ft.current = 0,
            KeyCode::End => ft.current = n.saturating_sub(1),
            KeyCode::Backspace => {
                ft.search.pop();
                moved = false;
            }
            KeyCode::Char(c) if !ctrl => {
                ft.search.push(c);
                moved = false;
                match find_from(tree, &ft.search, ft.current) {
                    Some(i) => ft.current = i,
                    // Far: a letter that finds nothing is not taken.
                    None => {
                        ft.search.pop();
                    }
                }
            }
            _ => moved = false,
        }
        ft.want = None;
        if moved {
            ft.search.clear();
        }
    }

    pub(super) fn folder_tree_mouse(&mut self, ev: &MouseEvent) {
        let wheel = self.wheel_lines() as usize;
        let Some(Overlay::FolderTree(ft)) = self.overlays.last_mut() else {
            return;
        };
        let n = self.trees.get(&ft.root).map_or(0, |t| t.nodes.len());
        let pos = Position::new(ev.column, ev.row);
        match ev.kind {
            MouseEventKind::ScrollUp => ft.current = ft.current.saturating_sub(wheel),
            MouseEventKind::ScrollDown => {
                ft.current = (ft.current + wheel).min(n.saturating_sub(1))
            }
            MouseEventKind::Down(MouseButton::Left) if ft.list.contains(pos) => {
                let i = ft.top + usize::from(pos.y - ft.list.y);
                if i < n {
                    if i == ft.current {
                        // A second click: go there.
                        self.folder_tree_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                        return;
                    }
                    ft.current = i;
                }
            }
            MouseEventKind::Down(_) if !ft.list.contains(pos) => {
                self.overlays.pop();
            }
            _ => {}
        }
    }
}

/// The first folder from `from` (going round) whose name starts with
/// `search` (any case).
fn find_from(tree: &Tree, search: &str, from: usize) -> Option<usize> {
    if search.is_empty() {
        return None;
    }
    let s = search.to_lowercase();
    let n = tree.nodes.len();
    (0..n)
        .map(|k| (from + k) % n.max(1))
        .find(|&i| tree.nodes[i].name().to_lowercase().starts_with(&s))
}

/// The window: Far's tree in a box — the folders with their lines, the
/// search (or "Reading the folders tree") at the bottom.
pub(super) fn draw_folder_tree(
    ft: &mut FolderTree,
    tree: Option<&Tree>,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.width < 20 || area.height < 8 {
        return;
    }
    // A window like Far's menus: cyan, a margin around the double frame,
    // a shadow; the screen shows around it.
    let window = Rect::new(area.x + 4, area.y + 1, area.width - 8, area.height - 2);
    buf.set_style(window, theme::MENU_TEXT);
    for y in window.top()..window.bottom() {
        for x in window.left()..window.right() {
            buf[(x, y)].set_symbol(" ");
        }
    }
    crate::dialog::draw_shadow(buf, window, area);
    let outer = Rect::new(
        window.x + 2,
        window.y + 1,
        window.width - 4,
        window.height - 2,
    );
    crate::panel::draw_frame(buf, outer, theme::MENU_BOX);
    let title = format!(" {} ", tr!("MFindFolderTitle"));
    crate::panel::put_title(buf, outer, outer.y, &title, theme::MENU_TITLE);
    let list = Rect::new(outer.x + 1, outer.y + 1, outer.width - 2, outer.height - 4);
    ft.list = list;
    let sep = outer.bottom() - 3;
    buf[(outer.x, sep)]
        .set_symbol("╟")
        .set_style(theme::MENU_BOX);
    buf[(outer.right() - 1, sep)]
        .set_symbol("╢")
        .set_style(theme::MENU_BOX);
    for x in outer.x + 1..outer.right() - 1 {
        buf[(x, sep)].set_symbol("─").set_style(theme::MENU_BOX);
    }
    let Some(tree) = tree else { return };
    let rows = usize::from(list.height).max(1);
    let n = tree.nodes.len();
    ft.current = ft.current.min(n.saturating_sub(1));
    if ft.current < ft.top {
        ft.top = ft.current;
    } else if ft.current >= ft.top + rows {
        ft.top = ft.current + 1 - rows;
    }
    for (k, i) in (ft.top..n).take(rows).enumerate() {
        let y = list.y + k as u16;
        let text = format!("{}{}", tree.prefix(i), tree.nodes[i].name());
        let style = if i == ft.current {
            theme::MENU_SELECTED
        } else {
            theme::MENU_TEXT
        };
        if i == ft.current {
            for x in list.left()..list.right() {
                buf[(x, y)].set_style(style);
            }
        }
        buf.set_stringn(list.x, y, &text, usize::from(list.width), style);
    }
    let status = if !tree.done {
        format!("{}… {}", tr!("MReadingTree"), n)
    } else {
        format!("{} {}", tr!("MFoldTreeSearch"), ft.search)
    };
    buf.set_stringn(
        list.x + 1,
        outer.bottom() - 2,
        &status,
        usize::from(list.width - 2),
        theme::MENU_TEXT,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_lines() {
        let mut t = Tree::default();
        for (d, p) in [
            (0, "C:\\"),
            (1, "C:\\a"),
            (2, "C:\\a\\x"),
            (2, "C:\\a\\y"),
            (1, "C:\\b"),
        ] {
            t.push(Node {
                depth: d,
                path: PathBuf::from(p),
            });
        }
        let lines: Vec<String> = (0..5)
            .map(|i| format!("{}{}", t.prefix(i), t.nodes[i].name()))
            .collect();
        assert_eq!(lines, ["C:\\", "├─a", "│ ├─x", "│ └─y", "└─b"]);
        assert_eq!(find_from(&t, "Y", 0), Some(3));
    }
}
