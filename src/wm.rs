//! Window manager: where windows are on the screen.
//!
//! - The desktop is a tiling tree of splits (`Node`). A split divides its
//!   area in two along a direction; the boundary between the halves is a
//!   splitter that can be dragged with the mouse or moved with the keyboard.
//! - One leaf of the root tree is the *screen slot*: it shows the current
//!   screen — panels, the user screen, later viewers and editors (Far's F12
//!   screens). Windows outside the slot (the agent pane) are docked: they
//!   stay visible whichever screen is current.
//! - Window contents are drawn by their owners; the window manager only
//!   computes rectangles (`Arrangement`) and keeps the split sizes.
//!
//! See docs/08-windows.md.

use ratatui::layout::{Position, Rect};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WinId {
    /// File panel 0 (left) or 1 (right).
    Panel(usize),
    /// The agent pane (one session now; several later).
    Agent(u32),
    /// Output of commands (Far's user screen, Ctrl+O).
    UserScreen,
    /// A viewer (F3) by its id.
    Viewer(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SplitId(pub u16);

/// Screen slot (above) / docked agent pane (below).
pub const MAIN_SPLIT: SplitId = SplitId(1);
/// Left / right file panel.
pub const PANELS_SPLIT: SplitId = SplitId(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    /// Children side by side; the splitter is vertical.
    Row,
    /// Children stacked; the splitter is horizontal.
    Column,
}

/// How the size of a split's first child is defined; it is kept in this
/// form when the area changes (e.g. the agent pane keeps its height when
/// the terminal grows, the panels keep their proportion).
#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Extent {
    /// Share of the area taken by the first child.
    Ratio(f32),
    FirstFixed(u16),
    SecondFixed(u16),
}

pub enum Node {
    Leaf(WinId),
    /// Where the current screen is shown.
    ScreenSlot,
    Split(Box<Split>),
}

pub struct Split {
    pub id: SplitId,
    pub dir: Dir,
    pub extent: Extent,
    /// Minimum sizes of the first and the second child.
    pub min: (u16, u16),
    pub first: Node,
    pub second: Node,
}

impl Node {
    pub fn split(
        id: SplitId,
        dir: Dir,
        extent: Extent,
        min: (u16, u16),
        first: Node,
        second: Node,
    ) -> Self {
        Node::Split(Box::new(Split {
            id,
            dir,
            extent,
            min,
            first,
            second,
        }))
    }

    fn find_split(&mut self, id: SplitId) -> Option<&mut Split> {
        let Node::Split(s) = self else { return None };
        if s.id == id {
            Some(s.as_mut())
        } else {
            let Split { first, second, .. } = s.as_mut();
            first.find_split(id).or_else(|| second.find_split(id))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScreenId {
    Panels,
    UserScreen,
    Viewer(u32),
}

pub struct Screen {
    pub id: ScreenId,
    pub tree: Node,
}

/// A boundary between the two children of a split, as last arranged.
#[derive(Clone, Copy, Debug)]
pub struct Splitter {
    pub id: SplitId,
    pub dir: Dir,
    /// Area of the whole split.
    pub area: Rect,
    /// First column (Row) or row (Column) of the second child.
    pub boundary: u16,
}

impl Splitter {
    fn start(&self) -> u16 {
        match self.dir {
            Dir::Row => self.area.x,
            Dir::Column => self.area.y,
        }
    }

    pub fn total(&self) -> u16 {
        match self.dir {
            Dir::Row => self.area.width,
            Dir::Column => self.area.height,
        }
    }

    /// Current size of the first child.
    pub fn first(&self) -> u16 {
        self.boundary - self.start()
    }

    /// The splitter is grabbed on the two cells around the boundary (the
    /// adjacent frames of the two windows); returns how far the grabbed cell
    /// is from the boundary.
    pub fn grab(&self, p: Position) -> Option<u16> {
        let (along, across, lo, hi) = match self.dir {
            Dir::Row => (p.x, p.y, self.area.y, self.area.bottom()),
            Dir::Column => (p.y, p.x, self.area.x, self.area.right()),
        };
        if across < lo || across >= hi {
            return None;
        }
        if along == self.boundary {
            Some(0)
        } else if along + 1 == self.boundary {
            Some(1)
        } else {
            None
        }
    }

    /// Size of the first child if the grabbed cell is moved to `p`.
    pub fn first_for(&self, p: Position, grab_offset: u16) -> u16 {
        let along = match self.dir {
            Dir::Row => p.x,
            Dir::Column => p.y,
        };
        (along + grab_offset).saturating_sub(self.start())
    }
}

/// Result of laying out the desktop.
#[derive(Clone, Debug, Default)]
pub struct Arrangement {
    pub windows: Vec<(WinId, Rect)>,
    /// Area of the screen slot.
    pub screen_area: Rect,
    pub splitters: Vec<Splitter>,
}

impl Arrangement {
    pub fn rect(&self, win: WinId) -> Option<Rect> {
        self.windows
            .iter()
            .find(|(w, _)| *w == win)
            .map(|(_, r)| *r)
    }

    pub fn splitter(&self, id: SplitId) -> Option<&Splitter> {
        self.splitters.iter().find(|s| s.id == id)
    }

    /// Splitter grabbed at `p`, with the grab offset.
    pub fn grab(&self, p: Position) -> Option<(SplitId, u16)> {
        self.splitters
            .iter()
            .find_map(|s| s.grab(p).map(|o| (s.id, o)))
    }
}

pub struct Wm {
    root: Node,
    screens: Vec<Screen>,
    current: usize,
    /// Windows hidden in place (Far's Ctrl+F1, Ctrl+F2, Ctrl+P): their
    /// area stays theirs, what is below shows through.
    hidden: Vec<WinId>,
}

impl Default for Wm {
    fn default() -> Self {
        Self::new()
    }
}

impl Wm {
    /// afar's desktop: screens above, the agent pane docked below; the
    /// panels screen is two file panels side by side.
    pub fn new() -> Self {
        let root = Node::split(
            MAIN_SPLIT,
            Dir::Column,
            Extent::SecondFixed(12),
            (5, 5),
            Node::ScreenSlot,
            Node::Leaf(WinId::Agent(0)),
        );
        let panels = Node::split(
            PANELS_SPLIT,
            Dir::Row,
            Extent::Ratio(0.5),
            (20, 20),
            Node::Leaf(WinId::Panel(0)),
            Node::Leaf(WinId::Panel(1)),
        );
        Self {
            root,
            screens: vec![
                Screen {
                    id: ScreenId::Panels,
                    tree: panels,
                },
                Screen {
                    id: ScreenId::UserScreen,
                    tree: Node::Leaf(WinId::UserScreen),
                },
            ],
            current: 0,
            hidden: Vec::new(),
        }
    }

    /// The agent pane is above the screens (otherwise below them).
    pub fn agent_on_top(&self) -> bool {
        matches!(&self.root, Node::Split(s) if matches!(s.first, Node::Leaf(WinId::Agent(_))))
    }

    /// Moves the agent pane above or below the screens, keeping its size.
    pub fn set_agent_on_top(&mut self, top: bool) {
        if self.agent_on_top() == top {
            return;
        }
        let Node::Split(s) = &mut self.root else {
            return;
        };
        std::mem::swap(&mut s.first, &mut s.second);
        s.min = (s.min.1, s.min.0);
        s.extent = match s.extent {
            Extent::FirstFixed(n) => Extent::SecondFixed(n),
            Extent::SecondFixed(n) => Extent::FirstFixed(n),
            Extent::Ratio(r) => Extent::Ratio(1.0 - r),
        };
    }

    /// The agent pane's height as an extent of the main split.
    pub fn agent_extent(&self, height: u16) -> Extent {
        if self.agent_on_top() {
            Extent::FirstFixed(height)
        } else {
            Extent::SecondFixed(height)
        }
    }

    pub fn is_hidden(&self, win: WinId) -> bool {
        self.hidden.contains(&win)
    }

    pub fn set_hidden(&mut self, win: WinId, hidden: bool) {
        self.hidden.retain(|w| *w != win);
        if hidden {
            self.hidden.push(win);
        }
    }

    pub fn current_screen(&self) -> ScreenId {
        self.screens[self.current].id
    }

    pub fn switch_to(&mut self, id: ScreenId) {
        if let Some(i) = self.screens.iter().position(|s| s.id == id) {
            self.current = i;
        }
    }

    /// All screens in the order they were opened.
    pub fn screens(&self) -> Vec<ScreenId> {
        self.screens.iter().map(|s| s.id).collect()
    }

    /// A new screen showing one window; it becomes current.
    pub fn add_screen(&mut self, id: ScreenId, win: WinId) {
        self.screens.push(Screen {
            id,
            tree: Node::Leaf(win),
        });
        self.current = self.screens.len() - 1;
    }

    /// Removes a screen; when it was current, the panels are shown.
    pub fn remove_screen(&mut self, id: ScreenId) {
        let Some(i) = self.screens.iter().position(|s| s.id == id) else {
            return;
        };
        self.screens.remove(i);
        if self.current == i {
            self.current = 0;
        } else if self.current > i {
            self.current -= 1;
        }
    }

    pub fn arrange(&self, area: Rect) -> Arrangement {
        let mut out = Arrangement::default();
        self.arrange_node(&self.root, area, &mut out);
        out
    }

    fn arrange_node(&self, node: &Node, area: Rect, out: &mut Arrangement) {
        match node {
            Node::Leaf(w) if self.is_hidden(*w) => {}
            Node::Leaf(w) => out.windows.push((*w, area)),
            Node::ScreenSlot => {
                out.screen_area = area;
                self.arrange_node(&self.screens[self.current].tree, area, out);
            }
            Node::Split(s) => {
                let total = match s.dir {
                    Dir::Row => area.width,
                    Dir::Column => area.height,
                };
                let wanted = match s.extent {
                    Extent::Ratio(r) => (f32::from(total) * r).round() as u16,
                    Extent::FirstFixed(n) => n,
                    Extent::SecondFixed(n) => total.saturating_sub(n),
                };
                let first = clamp_first(wanted, total, s.min);
                let (a, b) = match s.dir {
                    Dir::Row => (
                        Rect::new(area.x, area.y, first, area.height),
                        Rect::new(area.x + first, area.y, total - first, area.height),
                    ),
                    Dir::Column => (
                        Rect::new(area.x, area.y, area.width, first),
                        Rect::new(area.x, area.y + first, area.width, total - first),
                    ),
                };
                // Nothing to move next to a hidden panel. The hidden agent
                // pane keeps its boundary: the panels keep their height and
                // it can still be dragged (Ctrl+O shows the output there).
                let hidden = |n: &Node| matches!(n, Node::Leaf(w) if self.is_hidden(*w) && !matches!(w, WinId::Agent(_)));
                if !hidden(&s.first) && !hidden(&s.second) {
                    out.splitters.push(Splitter {
                        id: s.id,
                        dir: s.dir,
                        area,
                        boundary: match s.dir {
                            Dir::Row => b.x,
                            Dir::Column => b.y,
                        },
                    });
                }
                self.arrange_node(&s.first, a, out);
                self.arrange_node(&s.second, b, out);
            }
        }
    }

    fn find_split(&mut self, id: SplitId) -> Option<&mut Split> {
        std::iter::once(&mut self.root)
            .chain(self.screens.iter_mut().map(|s| &mut s.tree))
            .find_map(|n| n.find_split(id))
    }

    /// Sets the size of the split's first child (within its minimums),
    /// keeping the kind of its extent; `total` is the split's current size.
    pub fn set_first(&mut self, id: SplitId, first: i32, total: u16) {
        let Some(s) = self.find_split(id) else { return };
        let first = clamp_first(first.clamp(0, i32::from(total)) as u16, total, s.min);
        s.extent = match s.extent {
            Extent::Ratio(_) if total > 0 => Extent::Ratio(f32::from(first) / f32::from(total)),
            Extent::Ratio(r) => Extent::Ratio(r),
            Extent::FirstFixed(_) => Extent::FirstFixed(first),
            Extent::SecondFixed(_) => Extent::SecondFixed(total - first),
        };
    }

    /// Extents of all splits (for saving the layout).
    pub fn extents(&self) -> Vec<(SplitId, Extent)> {
        fn collect(node: &Node, out: &mut Vec<(SplitId, Extent)>) {
            if let Node::Split(s) = node {
                out.push((s.id, s.extent));
                collect(&s.first, out);
                collect(&s.second, out);
            }
        }
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        for s in &self.screens {
            collect(&s.tree, &mut out);
        }
        out
    }

    pub fn set_extent(&mut self, id: SplitId, extent: Extent) {
        // The agent pane's height was saved with it on either side.
        let extent = match (id == MAIN_SPLIT, extent) {
            (true, Extent::FirstFixed(n) | Extent::SecondFixed(n)) => self.agent_extent(n),
            _ => extent,
        };
        if let Some(s) = self.find_split(id) {
            s.extent = extent;
        }
    }
}

fn clamp_first(first: u16, total: u16, (min_a, min_b): (u16, u16)) -> u16 {
    if total >= min_a + min_b {
        first.clamp(min_a, total - min_b)
    } else {
        // Too small for both minimums: give the first child what it can get.
        first.min(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arranges_desktop() {
        let wm = Wm::new();
        let a = wm.arrange(Rect::new(0, 0, 100, 30));
        assert_eq!(a.screen_area, Rect::new(0, 0, 100, 18));
        assert_eq!(a.rect(WinId::Agent(0)), Some(Rect::new(0, 18, 100, 12)));
        assert_eq!(a.rect(WinId::Panel(0)), Some(Rect::new(0, 0, 50, 18)));
        assert_eq!(a.rect(WinId::Panel(1)), Some(Rect::new(50, 0, 50, 18)));
        assert_eq!(a.rect(WinId::UserScreen), None);
    }

    #[test]
    fn switches_screens_keeping_dock() {
        let mut wm = Wm::new();
        wm.switch_to(ScreenId::UserScreen);
        let a = wm.arrange(Rect::new(0, 0, 100, 30));
        assert_eq!(a.rect(WinId::UserScreen), Some(Rect::new(0, 0, 100, 18)));
        assert_eq!(a.rect(WinId::Panel(0)), None);
        assert!(a.rect(WinId::Agent(0)).is_some());
    }

    #[test]
    fn drags_splitters_within_minimums() {
        let mut wm = Wm::new();
        let area = Rect::new(0, 0, 100, 30);
        let a = wm.arrange(area);
        // Grab the right panel's left frame (x = 50) and move it to x = 30.
        let (id, offset) = a.grab(Position::new(50, 5)).unwrap();
        assert_eq!((id, offset), (PANELS_SPLIT, 0));
        let s = *a.splitter(id).unwrap();
        wm.set_first(
            id,
            i32::from(s.first_for(Position::new(30, 5), offset)),
            s.total(),
        );
        assert_eq!(wm.arrange(area).rect(WinId::Panel(0)).unwrap().width, 30);
        // Beyond the minimum of the left panel.
        wm.set_first(id, 3, s.total());
        assert_eq!(wm.arrange(area).rect(WinId::Panel(0)).unwrap().width, 20);
        // The agent pane keeps its height when the terminal grows.
        let s = *wm.arrange(area).splitter(MAIN_SPLIT).unwrap();
        wm.set_first(MAIN_SPLIT, i32::from(s.first()) - 4, s.total());
        let tall = wm.arrange(Rect::new(0, 0, 100, 50));
        assert_eq!(tall.rect(WinId::Agent(0)).unwrap().height, 16);
    }

    #[test]
    fn adds_and_removes_screens() {
        let mut wm = Wm::new();
        wm.add_screen(ScreenId::Viewer(1), WinId::Viewer(1));
        wm.add_screen(ScreenId::Viewer(2), WinId::Viewer(2));
        assert_eq!(wm.current_screen(), ScreenId::Viewer(2));
        let a = wm.arrange(Rect::new(0, 0, 100, 30));
        assert_eq!(a.rect(WinId::Viewer(2)), Some(Rect::new(0, 0, 100, 18)));
        wm.remove_screen(ScreenId::Viewer(1));
        assert_eq!(wm.current_screen(), ScreenId::Viewer(2));
        wm.remove_screen(ScreenId::Viewer(2));
        assert_eq!(wm.current_screen(), ScreenId::Panels);
    }

    #[test]
    fn agent_on_top_keeps_its_height() {
        let mut wm = Wm::new();
        wm.set_agent_on_top(true);
        let a = wm.arrange(Rect::new(0, 0, 100, 30));
        assert_eq!(a.rect(WinId::Agent(0)), Some(Rect::new(0, 0, 100, 12)));
        assert_eq!(a.screen_area, Rect::new(0, 12, 100, 18));
        // A height saved with the agent below.
        wm.set_extent(MAIN_SPLIT, Extent::SecondFixed(8));
        let a = wm.arrange(Rect::new(0, 0, 100, 30));
        assert_eq!(a.rect(WinId::Agent(0)), Some(Rect::new(0, 0, 100, 8)));
        wm.set_agent_on_top(false);
        let a = wm.arrange(Rect::new(0, 0, 100, 30));
        assert_eq!(a.rect(WinId::Agent(0)), Some(Rect::new(0, 22, 100, 8)));
    }

    #[test]
    fn hidden_agent_keeps_its_boundary() {
        let mut wm = Wm::new();
        wm.set_hidden(WinId::Agent(0), true);
        let a = wm.arrange(Rect::new(0, 0, 100, 30));
        assert_eq!(a.rect(WinId::Agent(0)), None);
        assert_eq!(a.screen_area, Rect::new(0, 0, 100, 18));
        assert_eq!(a.splitter(MAIN_SPLIT).map(|s| s.boundary), Some(18));
    }

    #[test]
    fn hidden_window_keeps_its_place() {
        let mut wm = Wm::new();
        wm.set_hidden(WinId::Panel(0), true);
        let a = wm.arrange(Rect::new(0, 0, 100, 30));
        assert_eq!(a.rect(WinId::Panel(0)), None);
        assert_eq!(a.rect(WinId::Panel(1)), Some(Rect::new(50, 0, 50, 18)));
        assert!(a.splitter(PANELS_SPLIT).is_none());
    }

    #[test]
    fn grabs_both_frame_cells() {
        let a = Wm::new().arrange(Rect::new(0, 0, 100, 30));
        assert_eq!(a.grab(Position::new(49, 3)), Some((PANELS_SPLIT, 1)));
        assert_eq!(a.grab(Position::new(40, 17)), Some((MAIN_SPLIT, 1)));
        assert_eq!(a.grab(Position::new(40, 18)), Some((MAIN_SPLIT, 0)));
        assert_eq!(a.grab(Position::new(30, 5)), None);
    }
}
