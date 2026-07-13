//! The player's interface: a model, an input handler, and a painter.
//!
//! The same `Ui` serves both places a menu appears. Before a game is running it
//! is the library, filling the window. While one is running and paused, it is the
//! overlay, drawn over the frozen frame — the same screens, plus a Save button
//! and a way back out. That is the whole reason this is a crate and not a pile of
//! drawing code inside the player: there is exactly one interface, used twice.

pub mod canvas;
pub mod font;
pub mod theme;
mod view;

pub use canvas::{Canvas, Color, Image, Rect};
pub use font::{Fonts, Weight};

/// A key, already interpreted — the host maps SDL to this so nothing downstream
/// has to know about SDL.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Backspace,
    /// The overlay hotkey (F12), which also closes the overlay.
    Overlay,
    /// Ctrl+V. The UI cannot read a clipboard; it asks the host to.
    Paste,
    /// Offer to remove the game the cursor is on.
    Delete,
}

/// What the host must go and do. The UI itself performs nothing: it has no idea
/// how to start a game or write a snapshot, and keeping it that way is what makes
/// it testable.
#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    None,
    /// Start `game`. `restore` indexes that game's saves; `None` starts it fresh.
    Launch {
        game: usize,
        restore: Option<usize>,
    },
    /// Close the overlay and carry on exactly where we were.
    Resume,
    /// Snapshot the running game into a new save.
    Save,
    /// Leave the running game; go back to the library.
    ToLibrary,
    Quit,
    /// Add a game from a dropped zip / directory, or from a pasted URL.
    AddPath(String),
    AddUrl(String),
    /// The player picked which executable in the bundle is the game.
    PickExe(usize),
    /// The volume changed. The UI cannot make a sound or write a settings file,
    /// so it does what it does with everything else: it says so, and the host
    /// applies it and remembers it.
    SetVolume(f32),
    /// Put the clipboard's text in the link field. Only the host can read a
    /// clipboard, so the UI can only ask.
    Paste,
    /// Remove a game, and everything of it: the bundle and the saves. Only ever
    /// raised from a confirmation the player answered.
    DeleteGame(usize),
    /// Run one of the other programs on a game's disk — its setup, its installer
    /// — once, instead of the game. `exe` indexes that game's `programs`.
    RunFile {
        game: usize,
        exe: usize,
    },
}

pub struct SaveView {
    pub when: String,
    pub thumb: Option<Image>,
}

pub struct GameView {
    pub key: String,
    pub title: String,
    pub saves: Vec<SaveView>,
    /// The other programs on this game's disk, as the guest would see them
    /// ("SETUP.EXE"): its setup, its installer. Not the game itself — Play is
    /// already the button for that. Usually empty.
    pub programs: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Library,
    /// A game's page: play it, or pick a save. Also the overlay's screen.
    Game,
    Settings,
    AddGame,
}

/// Which column the cursor is in on a game's page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Actions,
    Saves,
}

#[derive(Default)]
pub struct AddState {
    pub url: String,
    pub status: Option<String>,
    /// Executables found in a freshly-extracted bundle, for the player to choose
    /// between — the CLI asks this on a tty; here it is a list.
    pub exes: Vec<String>,
    pub exe_idx: usize,
    pub busy: bool,
}

/// Something the mouse can land on. Built during paint, so the layout is defined
/// once and hit-testing cannot drift from what is on screen.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Game(usize),
    /// The "…" button on a game's card, which opens everything you can do to a
    /// game that is not playing it.
    Menu(usize),
    /// A row of the card menu.
    MenuItem(usize),
    /// A program in the "Run a file" list.
    Program(usize),
    Add,
    Action(usize),
    Save(usize),
    Exe(usize),
    Paste,
    ConfirmYes,
    ConfirmNo,
    /// The volume slider's track. Unlike every other hit, this one is
    /// *positional*: where along it you landed is the value you asked for.
    Volume(Rect),
}

pub struct Ui {
    pub fonts: Fonts,
    pub logo: Image,
    pub games: Vec<GameView>,
    pub screen: Screen,
    /// The game the cursor is on in the library, and the subject of the Game page.
    pub game: usize,
    pub action: usize,
    pub save: usize,
    pub focus: Focus,
    /// True when a game is running and paused behind us: the overlay.
    pub in_game: bool,
    /// Whether a snapshot is possible at the instant we paused. The overlay opens
    /// at a savable resting point, so this is normally true; it is false only when
    /// a game never reached one and we opened anyway rather than trap the player.
    pub can_save: bool,
    pub add: AddState,
    /// A transient line under the actions ("Saved.", "Could not save.").
    pub message: Option<String>,
    /// The game a delete has been offered for, and is waiting on an answer to.
    /// Nothing is removed until this is answered yes.
    pub deleting: Option<usize>,
    /// Which button the confirmation has focus on. Starts on Cancel: the
    /// dangerous answer should never be the one a stray Enter gives.
    delete_yes: bool,

    /// The game whose "…" menu is open, if one is.
    pub menu: Option<usize>,
    menu_idx: usize,
    /// The game whose program list is open — "Run a file", one level in from the
    /// menu. Both are modal, and only one is ever up.
    pub picking: Option<usize>,
    pick_idx: usize,

    /// Master volume, 0..1. The host seeds this from the settings file (per game,
    /// falling back to the default) and writes it back when it changes.
    pub volume: f32,
    /// True while the volume knob is being dragged, so a mouse-move keeps moving
    /// it even once the pointer has slid off the track.
    dragging_volume: bool,

    /// Filled by paint; read by the mouse. Cleared each frame.
    hot: Vec<(Rect, Hit)>,
    /// Columns the library grid last used, so Up/Down move by a row.
    cols: usize,
    /// First visible row of the library grid.
    scroll: usize,
}

impl Ui {
    pub fn new(logo: Image, games: Vec<GameView>) -> Ui {
        Ui {
            fonts: Fonts::new(),
            logo,
            games,
            screen: Screen::Library,
            game: 0,
            action: 0,
            save: 0,
            focus: Focus::Actions,
            in_game: false,
            can_save: true,
            add: AddState::default(),
            message: None,
            deleting: None,
            delete_yes: false,
            menu: None,
            menu_idx: 0,
            picking: None,
            pick_idx: 0,
            volume: 0.6,
            dragging_volume: false,
            hot: Vec::new(),
            cols: 1,
            scroll: 0,
        }
    }

    /// Enter overlay mode for the game at `game`.
    pub fn open_overlay(&mut self, game: usize, can_save: bool) {
        self.in_game = true;
        self.can_save = can_save;
        self.screen = Screen::Game;
        self.game = game;
        self.focus = Focus::Actions;
        self.message = None;
        self.action = self.first_enabled_action();
    }

    /// Where the cursor goes when a game's page opens.
    ///
    /// Never onto a disabled action. A game with no saves opens with Continue
    /// dead, and a cursor resting there is a page whose Enter key does nothing —
    /// and, since a disabled action draws no selection, one that doesn't even look
    /// like it has a cursor.
    fn first_enabled_action(&self) -> usize {
        (0..self.actions().len())
            .find(|&i| self.action_enabled(i))
            .unwrap_or(0)
    }

    /// The labels of the action column, which differ between "not started yet"
    /// and "paused mid-game".
    pub fn actions(&self) -> Vec<&'static str> {
        if self.in_game {
            vec!["Resume", "Save", "New game", "Settings", "Library"]
        } else {
            vec!["Continue", "New game", "Settings", "Back"]
        }
    }

    /// An action the player cannot take right now — drawn dim and skipped over.
    pub fn action_enabled(&self, i: usize) -> bool {
        let labels = self.actions();
        match labels.get(i).copied() {
            // Nothing to continue into until there is a save.
            Some("Continue") => !self.cur_saves().is_empty(),
            Some("Save") => self.can_save,
            _ => true,
        }
    }

    fn cur_saves(&self) -> &[SaveView] {
        self.games
            .get(self.game)
            .map(|g| g.saves.as_slice())
            .unwrap_or(&[])
    }

    // ---- input ------------------------------------------------------------

    /// What the "…" menu offers for the game at `game`.
    ///
    /// "Run a file" is *absent*, not disabled, on a bundle that holds nothing else
    /// to run — which is most of them. A menu row that can never do anything is
    /// worse than a menu that is honest about being short.
    pub fn menu_items(&self, game: usize) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.games.get(game).is_some_and(|g| !g.programs.is_empty()) {
            v.push("Run a file");
        }
        v.push("Remove game");
        v
    }

    pub fn key(&mut self, k: Key) -> Action {
        self.message = None;
        // These are modal on purpose: while one is up, nothing behind it can be
        // reached, by key or by click. Innermost first — the program list opens
        // from the menu, and closing it must land back on the menu, not the grid.
        if self.picking.is_some() {
            return self.key_pick(k);
        }
        if self.menu.is_some() {
            return self.key_menu(k);
        }
        if self.deleting.is_some() {
            return self.key_confirm(k);
        }
        match self.screen {
            Screen::Library => self.key_library(k),
            Screen::Game => self.key_game(k),
            // Settings holds one control, so it takes the arrows directly rather
            // than making you first move a cursor onto the only thing there is.
            Screen::Settings => match k {
                Key::Left => self.volume_from_key(-0.05),
                Key::Right => self.volume_from_key(0.05),
                Key::Escape | Key::Enter | Key::Overlay => {
                    self.screen = Screen::Game;
                    Action::None
                }
                _ => Action::None,
            },
            Screen::AddGame => self.key_add(k),
        }
    }

    fn key_confirm(&mut self, k: Key) -> Action {
        match k {
            Key::Left | Key::Right => self.delete_yes = !self.delete_yes,
            Key::Escape => self.deleting = None,
            Key::Enter => {
                let game = self.deleting.take();
                if self.delete_yes {
                    if let Some(i) = game {
                        return Action::DeleteGame(i);
                    }
                }
            }
            _ => {}
        }
        Action::None
    }

    /// Offer to remove the game at `game`. Answering is the only way anything is
    /// actually deleted.
    fn offer_delete(&mut self, game: usize) {
        if game < self.games.len() {
            self.deleting = Some(game);
            self.delete_yes = false;
        }
    }

    /// Open the "…" menu on the game at `game`.
    pub fn open_menu(&mut self, game: usize) {
        if game < self.games.len() {
            self.game = game;
            self.menu = Some(game);
            self.menu_idx = 0;
        }
    }

    fn key_menu(&mut self, k: Key) -> Action {
        let Some(game) = self.menu else {
            return Action::None;
        };
        let n = self.menu_items(game).len();
        match k {
            Key::Up => self.menu_idx = self.menu_idx.saturating_sub(1),
            Key::Down => self.menu_idx = (self.menu_idx + 1).min(n.saturating_sub(1)),
            Key::Escape | Key::Overlay => self.menu = None,
            Key::Enter => return self.activate_menu(self.menu_idx),
            _ => {}
        }
        Action::None
    }

    /// Take a menu row. Nothing here *does* anything on its own: both rows open
    /// the question the row is really asking.
    fn activate_menu(&mut self, i: usize) -> Action {
        let Some(game) = self.menu else {
            return Action::None;
        };
        match self.menu_items(game).get(i).copied() {
            Some("Run a file") => {
                self.menu = None;
                self.picking = Some(game);
                self.pick_idx = 0;
            }
            Some("Remove game") => {
                self.menu = None;
                self.offer_delete(game);
            }
            _ => {}
        }
        Action::None
    }

    fn key_pick(&mut self, k: Key) -> Action {
        let Some(game) = self.picking else {
            return Action::None;
        };
        let n = self.games.get(game).map(|g| g.programs.len()).unwrap_or(0);
        match k {
            Key::Up => self.pick_idx = self.pick_idx.saturating_sub(1),
            Key::Down => self.pick_idx = (self.pick_idx + 1).min(n.saturating_sub(1)),
            // Back to the menu it was opened from, not out to the grid.
            Key::Escape => {
                self.picking = None;
                self.open_menu(game);
            }
            Key::Overlay => self.picking = None,
            Key::Enter if self.pick_idx < n => {
                self.picking = None;
                return Action::RunFile {
                    game,
                    exe: self.pick_idx,
                };
            }
            _ => {}
        }
        Action::None
    }

    fn key_library(&mut self, k: Key) -> Action {
        // "Add game" is a button in the header, above the grid, and it keeps the
        // index just past the last game. So Up out of the top row reaches it and
        // Down comes back down — which is where it actually is on screen.
        let n = self.games.len();
        let add = n;
        let cols = self.cols.max(1);
        match k {
            Key::Left => self.game = self.game.saturating_sub(1),
            Key::Right => self.game = (self.game + 1).min(add),
            Key::Up => {
                if self.game != add {
                    self.game = if self.game < cols {
                        add
                    } else {
                        self.game - cols
                    };
                }
            }
            Key::Down => {
                self.game = if self.game == add {
                    0
                } else {
                    (self.game + cols).min(n.saturating_sub(1))
                };
            }
            Key::Enter => {
                if self.game >= self.games.len() {
                    self.screen = Screen::AddGame;
                    self.add = AddState::default();
                } else {
                    self.screen = Screen::Game;
                    self.save = 0;
                    self.focus = Focus::Actions;
                    self.action = self.first_enabled_action();
                }
            }
            Key::Delete => self.offer_delete(self.game),
            // In the library with a game paused behind us, Escape goes back to it
            // rather than killing it.
            Key::Escape | Key::Overlay => {
                return if self.in_game {
                    Action::Resume
                } else {
                    Action::Quit
                }
            }
            _ => {}
        }
        Action::None
    }

    fn key_game(&mut self, k: Key) -> Action {
        let labels = self.actions();
        let saves = self.cur_saves().len();
        match k {
            Key::Overlay if self.in_game => return Action::Resume,
            Key::Escape => {
                if self.in_game {
                    return Action::Resume;
                }
                self.screen = Screen::Library;
            }
            Key::Right if saves > 0 => self.focus = Focus::Saves,
            Key::Left => self.focus = Focus::Actions,
            Key::Up | Key::Down => {
                let down = k == Key::Down;
                match self.focus {
                    Focus::Actions => {
                        // Step over disabled actions so the cursor never rests on
                        // something Enter would do nothing to.
                        let n = labels.len();
                        let mut i = self.action;
                        for _ in 0..n {
                            i = if down { (i + 1) % n } else { (i + n - 1) % n };
                            if self.action_enabled(i) {
                                break;
                            }
                        }
                        self.action = i;
                    }
                    Focus::Saves => {
                        if saves > 0 {
                            self.save = if down {
                                (self.save + 1).min(saves - 1)
                            } else {
                                self.save.saturating_sub(1)
                            };
                        }
                    }
                }
            }
            Key::Enter => return self.activate(),
            _ => {}
        }
        Action::None
    }

    /// Enter on whatever the cursor is on.
    fn activate(&mut self) -> Action {
        if self.focus == Focus::Saves {
            if self.save < self.cur_saves().len() {
                return Action::Launch {
                    game: self.game,
                    restore: Some(self.save),
                };
            }
            return Action::None;
        }
        if !self.action_enabled(self.action) {
            return Action::None;
        }
        match self.actions().get(self.action).copied() {
            // Continue picks up the newest save, which is the one at the top.
            Some("Continue") => Action::Launch {
                game: self.game,
                restore: Some(0),
            },
            Some("New game") => Action::Launch {
                game: self.game,
                restore: None,
            },
            Some("Resume") => Action::Resume,
            Some("Save") => Action::Save,
            Some("Settings") => {
                self.screen = Screen::Settings;
                Action::None
            }
            Some("Library") => Action::ToLibrary,
            Some("Back") => {
                self.screen = Screen::Library;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn key_add(&mut self, k: Key) -> Action {
        if !self.add.exes.is_empty() {
            // Choosing which executable in the bundle is the game.
            match k {
                Key::Up => self.add.exe_idx = self.add.exe_idx.saturating_sub(1),
                Key::Down => self.add.exe_idx = (self.add.exe_idx + 1).min(self.add.exes.len() - 1),
                Key::Enter => return Action::PickExe(self.add.exe_idx),
                Key::Escape => {
                    self.add = AddState::default();
                }
                _ => {}
            }
            return Action::None;
        }
        match k {
            Key::Escape => self.screen = Screen::Library,
            Key::Backspace => {
                self.add.url.pop();
            }
            Key::Paste => return Action::Paste,
            Key::Enter if !self.add.url.trim().is_empty() && !self.add.busy => {
                return Action::AddUrl(self.add.url.trim().to_string())
            }
            _ => {}
        }
        Action::None
    }

    /// The host fetched the clipboard for us.
    pub fn pasted(&mut self, text: &str) {
        if self.screen != Screen::AddGame || !self.add.exes.is_empty() {
            return;
        }
        // A copied URL routinely carries a trailing newline; typing one is
        // impossible, so nothing is lost by dropping the whitespace.
        self.add.url.push_str(text.trim());
    }

    /// A typed character (the URL field).
    pub fn text(&mut self, ch: char) {
        if self.screen == Screen::AddGame && self.add.exes.is_empty() && !ch.is_control() {
            self.add.url.push(ch);
        }
    }

    /// A file dropped on the window — a bundle to add, from anywhere in the UI.
    pub fn dropped(&mut self, path: &str) -> Action {
        self.screen = Screen::AddGame;
        self.add = AddState::default();
        Action::AddPath(path.to_string())
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> Action {
        // A drag in progress owns the pointer: the knob keeps following it even
        // once it has slid off the track, which is what every slider does and
        // what anyone dragging one expects.
        if self.dragging_volume {
            if let Some(track) = self.volume_track() {
                return self.set_volume(value_along(track, x));
            }
        }
        if let Some(hit) = self.hit(x, y) {
            self.point_at(hit);
        }
        Action::None
    }

    /// The mouse came up. Ends a drag; nothing else in the UI cares.
    pub fn mouse_up(&mut self) {
        self.dragging_volume = false;
    }

    /// The wheel turned. `dy` is in notches, positive away from the user.
    pub fn wheel(&mut self, dy: f32) -> Action {
        if self.volume_track().is_some() {
            return self.set_volume(self.volume + dy * 0.05);
        }
        Action::None
    }

    pub fn click(&mut self, x: f32, y: f32) -> Action {
        match self.hit(x, y) {
            Some(hit) => {
                self.point_at(hit);
                match hit {
                    Hit::Game(_) | Hit::Add => self.key(Key::Enter),
                    Hit::Action(_) | Hit::Save(_) => self.activate(),
                    Hit::Exe(i) => Action::PickExe(i),
                    Hit::Paste => Action::Paste,
                    // Clicking a slider anywhere jumps to that value and starts a
                    // drag from there — one gesture, not two.
                    Hit::Volume(track) => {
                        self.dragging_volume = true;
                        self.set_volume(value_along(track, x))
                    }
                    Hit::Menu(i) => {
                        self.open_menu(i);
                        Action::None
                    }
                    Hit::MenuItem(i) => self.activate_menu(i),
                    Hit::Program(_) => self.key(Key::Enter),
                    Hit::ConfirmYes => {
                        self.delete_yes = true;
                        self.key(Key::Enter)
                    }
                    Hit::ConfirmNo => {
                        self.deleting = None;
                        Action::None
                    }
                }
            }
            None => Action::None,
        }
    }

    /// The volume slider's track, if one is on screen. Recorded during paint, so
    /// it cannot drift from what the player can actually see.
    fn volume_track(&self) -> Option<Rect> {
        self.hot.iter().find_map(|(_, h)| match h {
            Hit::Volume(track) => Some(*track),
            _ => None,
        })
    }

    fn set_volume(&mut self, v: f32) -> Action {
        let v = v.clamp(0.0, 1.0);
        if (v - self.volume).abs() < 1.0e-4 {
            return Action::None;
        }
        self.volume = v;
        Action::SetVolume(v)
    }

    /// Move the cursor to whatever the pointer is over, so the keyboard and the
    /// mouse share one selection rather than fighting over two.
    fn point_at(&mut self, hit: Hit) {
        match hit {
            Hit::Game(i) => self.game = i,
            Hit::Add => self.game = self.games.len(),
            Hit::Action(i) => {
                self.focus = Focus::Actions;
                self.action = i;
            }
            Hit::Save(i) => {
                self.focus = Focus::Saves;
                self.save = i;
            }
            // The slider is not a cursor position — it is a value, and hovering it
            // must not steal the selection from the actions behind it.
            Hit::Volume(_) => {}
            Hit::Exe(i) => self.add.exe_idx = i,
            // Hovering the "…" must not open it — only pressing it does. Hovering a
            // card is not asking for its menu.
            Hit::Menu(i) => self.game = i,
            Hit::MenuItem(i) => self.menu_idx = i,
            Hit::Program(i) => self.pick_idx = i,
            Hit::ConfirmYes => self.delete_yes = true,
            Hit::ConfirmNo => self.delete_yes = false,
            Hit::Paste => {}
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hot
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| *h)
    }

    /// Where along a track an x lands, as 0..1.
    fn volume_from_key(&mut self, delta: f32) -> Action {
        self.set_volume(self.volume + delta)
    }

    // ---- paint ------------------------------------------------------------

    /// Paint the interface into `cv`.
    ///
    /// When `over_game` the background is left as a scrim over the frozen frame
    /// the caller has already drawn; otherwise the page is opaque.
    pub fn paint(&mut self, cv: &mut Canvas, over_game: bool) {
        self.hot.clear();
        view::paint(self, cv, over_game);
    }

    fn push_hot(&mut self, r: Rect, h: Hit) {
        self.hot.push((r, h));
    }
}

/// Where along `track` the x coordinate `x` lands, as 0..1.
fn value_along(track: Rect, x: f32) -> f32 {
    if track.w <= 0.0 {
        return 0.0;
    }
    ((x - track.x) / track.w).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui(games: &[(&str, usize)]) -> Ui {
        Ui::new(
            Image::default(),
            games
                .iter()
                .map(|(t, n)| GameView {
                    key: t.to_lowercase(),
                    title: t.to_string(),
                    saves: (0..*n)
                        .map(|i| SaveView {
                            when: format!("save {i}"),
                            thumb: None,
                        })
                        .collect(),
                    programs: Vec::new(),
                })
                .collect(),
        )
    }

    #[test]
    fn enter_on_a_game_opens_its_page_and_escape_comes_back() {
        let mut u = ui(&[("Zeliard", 0), ("Pop", 0)]);
        assert_eq!(u.screen, Screen::Library);
        u.key(Key::Right);
        assert_eq!(u.game, 1);
        u.key(Key::Enter);
        assert_eq!(u.screen, Screen::Game);
        u.key(Key::Escape);
        assert_eq!(u.screen, Screen::Library);
    }

    #[test]
    fn continue_is_disabled_until_there_is_a_save() {
        let mut u = ui(&[("Zeliard", 0)]);
        u.key(Key::Enter);
        assert_eq!(u.actions()[0], "Continue");
        assert!(!u.action_enabled(0), "Continue with no saves");
        // Even reached deliberately, Enter on it launches nothing.
        u.action = 0;
        assert_eq!(u.key(Key::Enter), Action::None);

        let mut u = ui(&[("Zeliard", 2)]);
        u.key(Key::Enter);
        assert!(u.action_enabled(0));
        assert_eq!(u.actions()[u.action], "Continue");
        // Continue resumes the NEWEST save, which the host sorts to index 0.
        assert_eq!(
            u.key(Key::Enter),
            Action::Launch {
                game: 0,
                restore: Some(0)
            }
        );
    }

    #[test]
    fn a_page_never_opens_with_the_cursor_on_a_dead_action() {
        // An unplayed game has no Continue. Opening its page with the cursor
        // parked there is a page where Enter does nothing and — since a disabled
        // action draws no highlight — one that doesn't look like it has a cursor.
        let mut u = ui(&[("Zeliard", 0)]);
        u.key(Key::Enter);
        assert!(u.action_enabled(u.action));
        assert_eq!(u.actions()[u.action], "New game");
        assert_eq!(
            u.key(Key::Enter),
            Action::Launch {
                game: 0,
                restore: None
            },
            "Enter on a freshly-opened page must start the game"
        );

        // Same in the overlay, when the machine stopped somewhere unsavable.
        let mut u = ui(&[("Zeliard", 0)]);
        u.open_overlay(0, false);
        assert!(u.action_enabled(u.action));
        assert_eq!(u.actions()[u.action], "Resume");
    }

    #[test]
    fn the_cursor_steps_over_disabled_actions() {
        // No saves => Continue is disabled. Wrapping upward from New game must
        // skip past it to Back, not come to rest on a dead row.
        let mut u = ui(&[("Zeliard", 0)]);
        u.key(Key::Enter);
        assert_eq!(u.actions()[u.action], "New game");
        u.key(Key::Up);
        assert_eq!(u.actions()[u.action], "Back");
        u.key(Key::Down);
        assert_eq!(u.actions()[u.action], "New game");
    }

    #[test]
    fn new_game_launches_without_a_save() {
        let mut u = ui(&[("Zeliard", 3)]);
        u.key(Key::Enter);
        u.key(Key::Down); // Continue -> New game
        assert_eq!(u.actions()[u.action], "New game");
        assert_eq!(
            u.key(Key::Enter),
            Action::Launch {
                game: 0,
                restore: None
            }
        );
    }

    #[test]
    fn a_save_in_the_list_launches_that_save() {
        let mut u = ui(&[("Zeliard", 3)]);
        u.key(Key::Enter);
        u.key(Key::Right); // focus the saves
        u.key(Key::Down); // second save
        assert_eq!(u.focus, Focus::Saves);
        assert_eq!(
            u.key(Key::Enter),
            Action::Launch {
                game: 0,
                restore: Some(1)
            }
        );
    }

    #[test]
    fn overlay_offers_save_and_resume_and_the_way_back_to_the_library() {
        let mut u = ui(&[("Zeliard", 1)]);
        u.open_overlay(0, true);
        assert_eq!(
            u.actions(),
            ["Resume", "Save", "New game", "Settings", "Library"]
        );
        // The hotkey and Escape both mean "put me back where I was".
        assert_eq!(u.key(Key::Overlay), Action::Resume);
        assert_eq!(u.key(Key::Escape), Action::Resume);

        u.action = 1;
        assert_eq!(u.key(Key::Enter), Action::Save);

        u.action = 4;
        assert_eq!(u.key(Key::Enter), Action::ToLibrary);
    }

    #[test]
    fn save_is_disabled_when_the_machine_never_reached_a_savable_point() {
        let mut u = ui(&[("Zeliard", 0)]);
        u.open_overlay(0, false);
        assert!(
            !u.action_enabled(1),
            "Save must be off when it cannot happen"
        );
        u.action = 1;
        assert_eq!(u.key(Key::Enter), Action::None);
    }

    #[test]
    fn escape_in_the_library_quits_but_never_while_a_game_is_paused_behind_it() {
        let mut u = ui(&[("Zeliard", 0)]);
        assert_eq!(u.key(Key::Escape), Action::Quit);

        // Reached the library from a paused game: Escape must go back to it, not
        // throw the game away.
        let mut u = ui(&[("Zeliard", 0)]);
        u.open_overlay(0, true);
        u.screen = Screen::Library;
        assert_eq!(u.key(Key::Escape), Action::Resume);
    }

    #[test]
    fn the_add_tile_sits_after_the_games_and_opens_the_add_screen() {
        let mut u = ui(&[("Zeliard", 0), ("Pop", 0)]);
        u.key(Key::Right);
        u.key(Key::Right); // past both games, onto Add
        assert_eq!(u.game, 2);
        u.key(Key::Enter);
        assert_eq!(u.screen, Screen::AddGame);
    }

    fn painted(u: &Ui, h: Hit) -> bool {
        u.hot.iter().any(|(_, x)| *x == h)
    }

    #[test]
    fn add_game_is_on_screen_however_many_games_there_are() {
        // It used to be the last tile of the grid, so the more games you had the
        // further down it slid — and past a screenful it was off the bottom, which
        // made the one way to add a game the one thing you could not see. It lives
        // in the header now, so no amount of scrolling can take it away.
        for count in [0usize, 3, 24] {
            let games: Vec<(&str, usize)> = (0..count).map(|_| ("Game", 0)).collect();
            let mut u = ui(&games);
            let mut cv = Canvas::new(1280, 800);
            u.paint(&mut cv, false);
            assert!(painted(&u, Hit::Add), "Add missing with {count} games");
        }
    }

    #[test]
    fn a_library_taller_than_the_window_scrolls_to_keep_the_cursor_in_view() {
        let games: Vec<(&str, usize)> = (0..24).map(|_| ("Game", 0)).collect();
        let mut u = ui(&games);
        let mut cv = Canvas::new(1280, 800);
        u.paint(&mut cv, false);
        assert!(painted(&u, Hit::Game(0)), "the first row is on screen");

        // Walk to the last game.
        for _ in 0..30 {
            u.key(Key::Down);
        }
        u.paint(&mut cv, false);
        assert!(
            painted(&u, Hit::Game(23)),
            "the last row scrolled into view"
        );
        assert!(
            !painted(&u, Hit::Game(0)),
            "the first row scrolled off the top"
        );
        assert!(
            painted(&u, Hit::Add),
            "the header button never scrolls away"
        );

        // ...and back up to the top row (one Up per row above us).
        let rows = 24usize.div_ceil(u.cols);
        for _ in 0..rows - 1 {
            u.key(Key::Up);
        }
        u.paint(&mut cv, false);
        assert_eq!(u.scroll, 0);
        assert!(painted(&u, Hit::Game(0)));

        // One more Up leaves the grid for the button — and must not drag the grid
        // back down to wherever the last game was.
        u.key(Key::Up);
        assert_eq!(u.game, 24);
        u.paint(&mut cv, false);
        assert_eq!(u.scroll, 0, "the Add button is not in any row");
    }

    #[test]
    fn up_from_the_top_row_reaches_add_game_and_down_comes_back() {
        // The button sits above the grid, so that is where the cursor should find
        // it — and Down must come back into the grid rather than trapping you.
        let mut u = ui(&[("A", 0), ("B", 0), ("C", 0)]);
        let mut cv = Canvas::new(1280, 800);
        u.paint(&mut cv, false); // establishes the column count

        u.key(Key::Up);
        assert_eq!(u.game, 3, "Up out of the grid lands on Add game");
        assert_eq!(u.key(Key::Enter), Action::None);
        assert_eq!(u.screen, Screen::AddGame);

        u.screen = Screen::Library;
        u.key(Key::Down);
        assert_eq!(u.game, 0, "Down comes back into the grid");
    }

    #[test]
    fn typing_a_url_and_pressing_enter_asks_the_host_to_fetch_it() {
        let mut u = ui(&[]);
        u.key(Key::Enter); // the Add tile is the only thing there
        assert_eq!(u.screen, Screen::AddGame);
        for c in "http://x/g.zip".chars() {
            u.text(c);
        }
        assert_eq!(u.add.url, "http://x/g.zip");
        u.key(Key::Backspace);
        assert_eq!(u.add.url, "http://x/g.zi");
        u.add.url = "http://x/g.zip".into();
        assert_eq!(
            u.key(Key::Enter),
            Action::AddUrl("http://x/g.zip".to_string())
        );
    }

    /// Click whatever `h` was painted at. Panics if it was not painted at all,
    /// which is the point: a button nobody drew is a button nobody can press.
    fn click_hit(u: &mut Ui, h: Hit) -> Action {
        let (r, _) = *u
            .hot
            .iter()
            .find(|(_, x)| *x == h)
            .unwrap_or_else(|| panic!("{h:?} was never painted"));
        u.click(r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn removing_a_game_takes_an_answer_and_defaults_to_not() {
        let mut u = ui(&[("Zeliard", 2), ("Pop", 0)]);
        let mut cv = Canvas::new(1280, 800);
        u.paint(&mut cv, false);

        // The "…" is on the card the cursor is on, and only that one.
        assert!(painted(&u, Hit::Menu(0)));
        assert!(!painted(&u, Hit::Menu(1)), "only the selected card has one");

        // It opens a menu, and does nothing else.
        assert_eq!(click_hit(&mut u, Hit::Menu(0)), Action::None);
        assert_eq!(u.menu, Some(0));
        assert_eq!(u.deleting, None);

        // Remove is the only thing on offer for a game with nothing else to run,
        // and taking it asks rather than deletes.
        assert_eq!(u.menu_items(0), ["Remove game"]);
        u.paint(&mut cv, false);
        assert_eq!(click_hit(&mut u, Hit::MenuItem(0)), Action::None);
        assert_eq!(u.menu, None, "the menu closes behind the question");
        assert_eq!(u.deleting, Some(0));

        // Enter straight away must NOT delete: the focus starts on Cancel, so the
        // dangerous answer is never the one a stray keypress gives.
        assert_eq!(u.key(Key::Enter), Action::None);
        assert_eq!(u.deleting, None, "the question is answered either way");

        // Escape cancels too. Delete still opens the question straight from the
        // grid — the menu is another way in, not the only one.
        u.key(Key::Delete);
        assert_eq!(u.deleting, Some(0));
        assert_eq!(u.key(Key::Escape), Action::None);
        assert_eq!(u.deleting, None);

        // Choosing Remove, and only that, asks the host to delete it.
        u.key(Key::Delete);
        u.key(Key::Right); // Cancel -> Remove
        assert_eq!(u.key(Key::Enter), Action::DeleteGame(0));
        assert_eq!(u.deleting, None);
    }

    #[test]
    fn a_bundles_other_programs_can_be_run_one_off() {
        let mut u = ui(&[("Prince", 0)]);
        u.games[0].programs = vec!["SETUP.EXE".into(), "MIDI.EXE".into()];
        let mut cv = Canvas::new(1280, 800);
        u.paint(&mut cv, false);

        // A bundle with something else on its disk offers to run it.
        assert_eq!(u.menu_items(0), ["Run a file", "Remove game"]);
        click_hit(&mut u, Hit::Menu(0));
        u.paint(&mut cv, false);
        assert_eq!(click_hit(&mut u, Hit::MenuItem(0)), Action::None);
        assert_eq!(u.picking, Some(0), "the program list, not a launch");

        // Every program is on offer, and taking one runs THAT one.
        u.paint(&mut cv, false);
        assert!(painted(&u, Hit::Program(0)) && painted(&u, Hit::Program(1)));
        assert_eq!(
            click_hit(&mut u, Hit::Program(1)),
            Action::RunFile { game: 0, exe: 1 }
        );
        assert_eq!(u.picking, None);

        // The same by keyboard.
        u.open_menu(0);
        u.key(Key::Enter); // "Run a file"
        assert_eq!(u.picking, Some(0));
        u.key(Key::Down);
        assert_eq!(u.key(Key::Enter), Action::RunFile { game: 0, exe: 1 });

        // Escape goes back to the menu it came from, rather than dumping you out
        // on the grid a level further than you asked to go.
        u.open_menu(0);
        u.key(Key::Enter);
        assert_eq!(u.key(Key::Escape), Action::None);
        assert_eq!((u.picking, u.menu), (None, Some(0)), "back to the menu");
    }

    #[test]
    fn a_game_with_nothing_else_on_its_disk_is_not_offered_a_file_to_run() {
        // Most bundles are one executable. A "Run a file" row that could only ever
        // open an empty list is worse than a short menu.
        let mut u = ui(&[("Alley Cat", 0)]);
        assert_eq!(u.menu_items(0), ["Remove game"]);
        u.open_menu(0);
        assert_eq!(u.key(Key::Enter), Action::None);
        assert_eq!(u.deleting, Some(0), "the only row is Remove");
        assert_eq!(u.picking, None);
    }

    #[test]
    fn the_card_menu_is_modal() {
        // Same rule as the delete question: while it is up, the library behind it
        // is not clickable. Both sheets clear the hit rects the grid pushed.
        let mut u = ui(&[("Zeliard", 0), ("Pop", 0)]);
        u.games[0].programs = vec!["INSTALL.EXE".into()];
        let mut cv = Canvas::new(1280, 800);
        u.paint(&mut cv, false);
        let (card, _) = *u.hot.iter().find(|(_, h)| *h == Hit::Game(1)).unwrap();

        for sheet in ["menu", "programs"] {
            u.menu = None;
            u.picking = None;
            match sheet {
                "menu" => u.open_menu(0),
                _ => u.picking = Some(0),
            }
            u.paint(&mut cv, false);
            assert!(
                !painted(&u, Hit::Game(1)),
                "{sheet}: the library stopped taking clicks"
            );
            assert_eq!(
                u.click(card.x + card.w / 2.0, card.y + card.h / 2.0),
                Action::None
            );
            assert_eq!(
                u.screen,
                Screen::Library,
                "{sheet}: nothing opened behind it"
            );
        }
    }

    #[test]
    fn the_confirmation_is_modal() {
        // While the question is up, nothing behind it can be reached — by key or by
        // click. A click meant for the dialog must never fall through onto a card.
        let mut u = ui(&[("Zeliard", 0), ("Pop", 0)]);
        let mut cv = Canvas::new(1280, 800);
        u.paint(&mut cv, false);
        let (card, _) = *u.hot.iter().find(|(_, h)| *h == Hit::Game(1)).unwrap();

        u.key(Key::Delete);
        u.paint(&mut cv, false);
        assert!(
            !painted(&u, Hit::Game(1)),
            "the library stopped being clickable"
        );
        assert!(painted(&u, Hit::ConfirmYes) && painted(&u, Hit::ConfirmNo));

        // Clicking where a card used to be does nothing at all.
        assert_eq!(
            u.click(card.x + card.w / 2.0, card.y + card.h / 2.0),
            Action::None
        );
        assert_eq!(u.screen, Screen::Library);
        assert_eq!(u.deleting, Some(0), "still waiting on an answer");

        // Arrows drive the dialog, not the grid.
        assert_eq!(u.game, 0);
        u.key(Key::Right);
        assert_eq!(u.game, 0, "the grid did not move behind the dialog");
    }

    #[test]
    fn a_link_can_be_pasted_as_well_as_typed() {
        // Typing worked and pasting didn't, which is backwards: a URL is the one
        // thing here you are far more likely to have on the clipboard than in your
        // head. The UI cannot read a clipboard — it knows nothing about SDL — so it
        // asks, and the host answers with `pasted`.
        let mut u = ui(&[]);
        u.key(Key::Enter); // the Add screen
        assert_eq!(u.screen, Screen::AddGame);

        // Both the chord and the button raise the same request.
        assert_eq!(u.key(Key::Paste), Action::Paste);
        let mut cv = Canvas::new(1280, 800);
        u.paint(&mut cv, false);
        let (r, _) = *u
            .hot
            .iter()
            .find(|(_, h)| *h == Hit::Paste)
            .expect("a Paste button was painted");
        assert_eq!(u.click(r.x + r.w / 2.0, r.y + r.h / 2.0), Action::Paste);

        // A copied URL routinely carries a trailing newline; typing one is
        // impossible, so it must not survive into the field.
        u.pasted("  http://x/g.zip\n");
        assert_eq!(u.add.url, "http://x/g.zip");
        assert_eq!(
            u.key(Key::Enter),
            Action::AddUrl("http://x/g.zip".to_string())
        );
    }

    #[test]
    fn a_pasted_link_shows_its_end_not_its_scheme() {
        // The field elides from the front: what you want to see after a paste is
        // where the caret is, not `https://` and a cut-off host.
        let mut f = Fonts::new();
        let long = "https://archive.example.org/dos/games/1990/some-very-long-name.zip";
        let out = f.elide_front(long, Weight::Regular, 15.0, 200.0);
        assert!(out.starts_with('…'), "elided from the wrong end: {out}");
        assert!(out.ends_with("name.zip"), "lost the end: {out}");
        assert!(f.width(&out, Weight::Regular, 15.0) <= 200.0);
    }

    #[test]
    fn a_dropped_file_is_offered_to_the_host_from_anywhere() {
        let mut u = ui(&[("Zeliard", 0)]);
        assert_eq!(
            u.dropped("/tmp/game.zip"),
            Action::AddPath("/tmp/game.zip".into())
        );
        assert_eq!(u.screen, Screen::AddGame);
    }

    #[test]
    fn clicking_selects_and_activates_what_was_painted_there() {
        let mut u = ui(&[("Zeliard", 0), ("Pop", 0)]);
        let mut cv = Canvas::new(1280, 800);
        u.paint(&mut cv, false);
        // Click the second card, wherever the layout put it.
        let (r, _) = *u
            .hot
            .iter()
            .find(|(_, h)| *h == Hit::Game(1))
            .expect("second game was painted");
        let act = u.click(r.x + r.w / 2.0, r.y + r.h / 2.0);
        assert_eq!(u.game, 1);
        assert_eq!(u.screen, Screen::Game);
        assert_eq!(act, Action::None);
    }
}
