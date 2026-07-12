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
}

pub struct SaveView {
    pub when: String,
    pub thumb: Option<Image>,
}

pub struct GameView {
    pub key: String,
    pub title: String,
    pub saves: Vec<SaveView>,
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
    Add,
    Action(usize),
    Save(usize),
    Exe(usize),
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

    pub fn key(&mut self, k: Key) -> Action {
        self.message = None;
        match self.screen {
            Screen::Library => self.key_library(k),
            Screen::Game => self.key_game(k),
            Screen::Settings => {
                if matches!(k, Key::Escape | Key::Enter | Key::Overlay) {
                    self.screen = Screen::Game;
                }
                Action::None
            }
            Screen::AddGame => self.key_add(k),
        }
    }

    fn key_library(&mut self, k: Key) -> Action {
        // The "+ Add game" tile sits after the games, and is selectable.
        let n = self.games.len() + 1;
        let cols = self.cols.max(1);
        match k {
            Key::Left => self.game = self.game.saturating_sub(1),
            Key::Right => self.game = (self.game + 1).min(n - 1),
            Key::Up => self.game = self.game.saturating_sub(cols),
            Key::Down => self.game = (self.game + cols).min(n - 1),
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
            Key::Enter if !self.add.url.trim().is_empty() && !self.add.busy => {
                return Action::AddUrl(self.add.url.trim().to_string())
            }
            _ => {}
        }
        Action::None
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

    pub fn mouse_move(&mut self, x: f32, y: f32) {
        if let Some(hit) = self.hit(x, y) {
            self.point_at(hit);
        }
    }

    pub fn click(&mut self, x: f32, y: f32) -> Action {
        match self.hit(x, y) {
            Some(hit) => {
                self.point_at(hit);
                match hit {
                    Hit::Game(_) | Hit::Add => self.key(Key::Enter),
                    Hit::Action(_) | Hit::Save(_) => self.activate(),
                    Hit::Exe(i) => Action::PickExe(i),
                }
            }
            None => Action::None,
        }
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
            Hit::Exe(i) => self.add.exe_idx = i,
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hot
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| *h)
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

    #[test]
    fn a_library_taller_than_the_window_scrolls_to_keep_the_cursor_in_view() {
        // Enough games that the grid cannot fit, and "Add game" — the last tile —
        // is below the fold. It must still be reachable, or there is no way left
        // to add a game at all.
        let games: Vec<(&str, usize)> = (0..24).map(|_| ("Game", 0)).collect();
        let mut u = ui(&games);
        let mut cv = Canvas::new(1280, 800);
        u.paint(&mut cv, false);

        let painted = |u: &Ui, h: Hit| u.hot.iter().any(|(_, x)| *x == h);
        assert!(painted(&u, Hit::Game(0)), "the first row is on screen");
        assert!(!painted(&u, Hit::Add), "Add starts below the fold");

        // Walk to the very end.
        for _ in 0..30 {
            u.key(Key::Down);
        }
        assert_eq!(u.game, 24, "the cursor lands on the Add tile");
        u.paint(&mut cv, false);
        assert!(painted(&u, Hit::Add), "Add scrolled into view");
        assert!(
            !painted(&u, Hit::Game(0)),
            "the first row scrolled off the top"
        );

        // ...and back to the top.
        for _ in 0..30 {
            u.key(Key::Up);
        }
        u.paint(&mut cv, false);
        assert_eq!(u.scroll, 0);
        assert!(painted(&u, Hit::Game(0)));
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
