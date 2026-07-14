//! The player's interface: a model, an input handler, and a painter.
//!
//! The same `Ui` serves both places a menu appears. Before a game is running it
//! is the library, filling the window. While one is running and paused, it is the
//! overlay — the same screens, at the same size, in the same place, over the
//! frozen frame instead of over the page. That is the whole reason this is a
//! crate and not a pile of drawing code inside the player: there is exactly one
//! interface, used twice.
//!
//! Three rules the whole thing is built around.
//!
//! **Every screen carries its own way back, and wears the same brand.** The
//! wordmark and the tree the splash opened with, a title saying where you are, and
//! a button in the corner to leave by. Escape and F12 still work, but they are
//! shortcuts for that button — not the only door, spelled out in a line of grey
//! text at the foot of the page that you have to read before you can leave.
//!
//! **One thing means "this one".** A ring in the accent, on a lifted surface,
//! whether it is a card in the library, an action on a game's page, a row of a menu
//! or an answer to a question. Nothing is filled with the accent: a solid pink slab
//! and a ringed card are two languages for the same sentence, and this used to speak
//! both, a few inches apart.
//!
//! **Nothing irreversible happens without an answer.** Going to the library from
//! a paused game does not end it: the library is a screen *over* the game, the
//! way the pause menu is, and coming back resumes exactly where you were. The
//! only things that truly throw something away — removing a game, and starting
//! any game over the one you have paused — ask first, with the cursor on Cancel.

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
    ///
    /// This is the one action that ends a game already in progress — the host can
    /// only do it by replacing the process. So while a game is paused it is never
    /// raised until the player has answered for it; see `request_launch`.
    Launch {
        game: usize,
        restore: Option<usize>,
    },
    /// Close the overlay and carry on exactly where we were.
    Resume,
    /// Snapshot the running game into a new save.
    Save,
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
    /// A game's page: play it, or pick a save. Paused, it is also the pause menu.
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

/// A question that has to be answered before something that cannot be undone.
///
/// It carries the action it is asking about, so the one place that decides *what*
/// needs an answer is also the only place that can raise it, and a `yes` cannot
/// drift from the thing the words on screen described.
#[derive(Clone, PartialEq, Debug)]
pub struct Confirm {
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) note: Option<String>,
    /// The affirmative button's label — "Remove", "Leave", "Start over".
    pub(crate) yes: String,
    /// Paint the affirmative button as a warning rather than an accent. Removal
    /// is the only thing here that destroys a file.
    pub(crate) danger: bool,
    /// Raised if, and only if, the answer is yes.
    pub(crate) act: Action,
    /// Which button holds the focus. Starts on Cancel: the answer that throws
    /// something away should never be the one a stray Enter gives.
    pub(crate) yes_focused: bool,
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
    /// The way back, in the corner of every screen that has somewhere to go back
    /// to. The same thing Escape does.
    Back,
    /// A sheet's own way out — "Close", "Back". A sheet covers the back button,
    /// so it has to carry one.
    SheetCancel,
    ConfirmYes,
    ConfirmNo,
    /// The volume slider's track. Unlike every other hit, this one is
    /// *positional*: where along it you landed is the value you asked for.
    Volume(Rect),
}

pub struct Ui {
    pub fonts: Fonts,
    pub logo: Image,
    /// The two halves of the logo, cut out of it once, for the bar every screen
    /// wears: the wordmark, and the tree over the machine.
    ///
    /// They are not separate assets. The splash is one picture — the tree, the PC,
    /// and `saisei` set underneath it — and the header is that same picture, taken
    /// apart. A second file would be a second thing to keep in step with the first.
    pub wordmark: Image,
    pub mark: Image,
    pub games: Vec<GameView>,
    pub screen: Screen,
    /// The game the cursor is on in the library, and the subject of the Game page.
    pub game: usize,
    pub action: usize,
    pub save: usize,
    pub focus: Focus,
    /// The game that is running and paused behind us, if there is one. It is an
    /// *index*, not a flag, because the library is reachable from a paused game:
    /// the page you are looking at is not necessarily the game you are in, and
    /// what a page offers depends entirely on which of the two it is.
    pub running: Option<usize>,
    /// Whether a snapshot is possible at the instant we paused. The overlay opens
    /// at a savable resting point, so this is normally true; it is false only when
    /// a game never reached one and we opened anyway rather than trap the player.
    pub can_save: bool,
    pub add: AddState,
    /// A transient line under the actions ("Saved.", "Could not save.").
    pub message: Option<String>,
    /// The question waiting on an answer. Nothing irreversible happens until one
    /// is given.
    pub confirm: Option<Confirm>,

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
    /// Whatever the pointer is over, or None if it is over nothing.
    ///
    /// Distinct from the cursor. For most things the two are the same — hovering a
    /// card or an action *is* selecting it, so the keyboard and the mouse never
    /// fight over two positions (see `point_at`). But a few controls are outside
    /// the cursor's cycle and were left with no way to answer the pointer at all:
    /// Back sat permanently in `SURFACE_HI`, the colour this palette reserves for
    /// "hovered", so the one state it could show was the one it was always in.
    hover: Option<Hit>,
    /// Columns the library grid last used, so Up/Down move by a row.
    cols: usize,
    /// First visible row of the library grid.
    scroll: usize,
}

/// Where in the splash each half of it is.
///
/// Fractions, not pixels, so the art can be re-exported at another size without
/// this going quietly wrong; measured off `runtime/assets/saisei_logo.png`, whose
/// tree sits at (64, 63)–(1370, 845) of 1402x1122 and whose wordmark sits at
/// (224, 850)–(1114, 1018), with the band of black between them that makes the cut
/// obvious. `the_header_is_cut_from_the_logo` holds it to that.
const MARK_BOX: [f32; 4] = [0.045, 0.055, 0.932, 0.700];
const WORDMARK_BOX: [f32; 4] = [0.159, 0.757, 0.636, 0.155];

/// Cut a box out of `img` and shrink it to something a header can afford to draw
/// every frame.
fn cut(img: &Image, [x, y, w, h]: [f32; 4], tall: usize) -> Image {
    if img.is_empty() {
        return Image::default();
    }
    let px = |f: f32, of: usize| (f * of as f32).round() as usize;
    img.crop(px(x, img.w), px(y, img.h), px(w, img.w), px(h, img.h))
        .scaled_to_height(tall)
}

impl Ui {
    pub fn new(logo: Image, games: Vec<GameView>) -> Ui {
        Ui {
            fonts: Fonts::new(),
            wordmark: cut(&logo, WORDMARK_BOX, 96),
            mark: cut(&logo, MARK_BOX, 256),
            logo,
            games,
            screen: Screen::Library,
            game: 0,
            action: 0,
            save: 0,
            focus: Focus::Actions,
            running: None,
            can_save: true,
            add: AddState::default(),
            message: None,
            confirm: None,
            menu: None,
            menu_idx: 0,
            picking: None,
            pick_idx: 0,
            volume: 0.6,
            dragging_volume: false,
            hot: Vec::new(),
            hover: None,
            cols: 1,
            scroll: 0,
        }
    }

    /// Open the pause menu for the game at `game`, which is running and frozen.
    pub fn open_overlay(&mut self, game: usize, can_save: bool) {
        self.running = Some(game);
        self.can_save = can_save;
        self.game = game;
        self.message = None;
        self.open_game_page();
    }

    /// True when a game is running and paused behind whatever is on screen.
    pub fn in_game(&self) -> bool {
        self.running.is_some()
    }

    /// True when the page on screen is the paused game's own — the pause menu, as
    /// against some other game's page browsed to from it.
    fn on_running_game(&self) -> bool {
        self.running == Some(self.game)
    }

    /// Whether the library is being browsed or edited.
    ///
    /// With a game paused behind it, it is a place to *switch* games from and
    /// nothing else: no Add, no "…", no Delete. Adding or removing a game
    /// renumbers the very list the paused game is identified by, and none of it is
    /// work that cannot wait until you have left the game.
    fn can_edit_library(&self) -> bool {
        self.running.is_none()
    }

    /// Where the cursor goes when a game's page opens.
    fn open_game_page(&mut self) {
        self.screen = Screen::Game;
        self.save = 0;
        self.focus = Focus::Actions;
        self.action = self.first_enabled_action();
    }

    /// Never onto a disabled action. A game with no saves opens with Continue
    /// dead, and a cursor resting there is a page whose Enter key does nothing —
    /// and, since a disabled action draws no selection, one that doesn't even look
    /// like it has a cursor.
    fn first_enabled_action(&self) -> usize {
        (0..self.actions().len())
            .find(|&i| self.action_enabled(i))
            .unwrap_or(0)
    }

    /// The labels of the action column.
    ///
    /// The paused game's own page is the pause menu, and offers what a pause menu
    /// offers. Every other page — including another game's, browsed to while one
    /// is paused — is a game you have not started.
    pub fn actions(&self) -> Vec<&'static str> {
        if self.on_running_game() {
            vec!["Resume", "Save", "New game", "Settings", "Library"]
        } else {
            vec!["Continue", "New game", "Settings"]
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

    fn title_of(&self, game: usize) -> String {
        self.games
            .get(game)
            .map(|g| g.title.clone())
            .unwrap_or_default()
    }

    // ---- going back -------------------------------------------------------

    /// Is there anywhere to go back to? The library, with no game behind it, is
    /// where the app starts: it has no back button, because there is nothing
    /// behind it to draw one for.
    pub fn has_back(&self) -> bool {
        self.screen != Screen::Library || self.in_game()
    }

    /// One step back: the button in the corner, and what Escape does.
    ///
    /// Nothing here destroys anything. Notably, backing out of a paused game's
    /// page resumes it, and backing out of the library you reached *from* a paused
    /// game returns you to that game's page — the game is still sitting there,
    /// frozen, exactly as you left it.
    pub fn back(&mut self) -> Action {
        // Sheets first, innermost out: each is modal, and closing one must land on
        // whatever opened it rather than skipping a level.
        if self.confirm.is_some() {
            self.confirm = None;
            return Action::None;
        }
        if let Some(game) = self.picking.take() {
            self.open_menu(game);
            return Action::None;
        }
        if self.menu.is_some() {
            self.menu = None;
            return Action::None;
        }
        match self.screen {
            // The bundle is unpacked and we are choosing which file is the game:
            // back is back to the drop well, not out of adding altogether.
            Screen::AddGame if !self.add.exes.is_empty() => {
                self.add = AddState::default();
                Action::None
            }
            Screen::AddGame => {
                self.screen = Screen::Library;
                Action::None
            }
            Screen::Settings => {
                self.screen = Screen::Game;
                Action::None
            }
            Screen::Game => {
                if self.on_running_game() {
                    // Behind this page is the game itself.
                    Action::Resume
                } else {
                    self.screen = Screen::Library;
                    Action::None
                }
            }
            Screen::Library => match self.running {
                // We came here from a paused game. It is still paused.
                Some(r) => {
                    self.game = r;
                    self.open_game_page();
                    Action::None
                }
                None => Action::Quit,
            },
        }
    }

    // ---- asking first -----------------------------------------------------

    /// Every launch passes through here, and while a game is paused every launch
    /// ends it — the host can only start a game by replacing the process, so
    /// loading a save, starting over and switching games are all the same
    /// irreversible thing wearing three labels. So: ask, once, in one place.
    fn request_launch(&mut self, game: usize, restore: Option<usize>) -> Action {
        let act = Action::Launch { game, restore };
        let Some(running) = self.running else {
            return act;
        };
        let paused = self.title_of(running);
        let (title, detail, yes) = if game != running {
            (
                format!("Leave {paused}?"),
                format!(
                    "Starting {} will end the game you have paused.",
                    self.title_of(game)
                ),
                "Leave",
            )
        } else if restore.is_some() {
            (
                "Load this save?".to_string(),
                format!("{paused} is paused. Loading a save will end it."),
                "Load",
            )
        } else {
            (
                "Start over?".to_string(),
                format!("{paused} is paused. Starting a new game will end it."),
                "Start over",
            )
        };
        self.confirm = Some(Confirm {
            title,
            detail,
            note: Some("Anything you have not saved is lost. Your saves are kept.".to_string()),
            yes: yes.to_string(),
            danger: false,
            act,
            yes_focused: false,
        });
        Action::None
    }

    /// Offer to remove the game at `game`. Answering is the only way anything is
    /// actually deleted.
    ///
    /// It names what will go — the bundle *and* the saves — because both do, and a
    /// library is the only record you have of either.
    fn offer_delete(&mut self, game: usize) {
        if game >= self.games.len() || !self.can_edit_library() {
            return;
        }
        let saves = self.games[game].saves.len();
        let detail = match saves {
            0 => "The game and its files will be deleted.".to_string(),
            1 => "The game, its files and 1 save will be deleted.".to_string(),
            n => format!("The game, its files and {n} saves will be deleted."),
        };
        self.confirm = Some(Confirm {
            title: format!("Remove {}?", self.title_of(game)),
            detail,
            note: Some("This cannot be undone.".to_string()),
            yes: "Remove".to_string(),
            danger: true,
            act: Action::DeleteGame(game),
            yes_focused: false,
        });
    }

    fn key_confirm(&mut self, k: Key) -> Action {
        match k {
            Key::Left | Key::Right => {
                if let Some(c) = self.confirm.as_mut() {
                    c.yes_focused = !c.yes_focused;
                }
            }
            // Either answer closes the question. Only one of them acts on it.
            Key::Enter => {
                if let Some(c) = self.confirm.take() {
                    if c.yes_focused {
                        return c.act;
                    }
                }
            }
            _ => {}
        }
        Action::None
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
        // Escape is the back button, wherever you are — not a separate idea about
        // what "cancel" means on each screen.
        if k == Key::Escape {
            return self.back();
        }
        // And F12, with a game paused, is the way back to the game from anywhere
        // in the menus. It drops whatever sheet is open on the way — every one of
        // them opens with the cursor on the harmless answer, so dropping one is
        // always the safe reading of "put me back in my game".
        if k == Key::Overlay && self.in_game() {
            self.confirm = None;
            self.menu = None;
            self.picking = None;
            return Action::Resume;
        }
        // These are modal on purpose: while one is up, nothing behind it can be
        // reached, by key or by click. Innermost first — the program list opens
        // from the menu, and closing it must land back on the menu, not the grid.
        if self.confirm.is_some() {
            return self.key_confirm(k);
        }
        if self.picking.is_some() {
            return self.key_pick(k);
        }
        if self.menu.is_some() {
            return self.key_menu(k);
        }
        match self.screen {
            Screen::Library => self.key_library(k),
            Screen::Game => self.key_game(k),
            // Settings holds one control, so it takes the arrows directly rather
            // than making you first move a cursor onto the only thing there is.
            Screen::Settings => match k {
                Key::Left => self.volume_from_key(-0.05),
                Key::Right => self.volume_from_key(0.05),
                Key::Enter => self.back(),
                _ => Action::None,
            },
            Screen::AddGame => self.key_add(k),
        }
    }

    /// Open the "…" menu on the game at `game`.
    pub fn open_menu(&mut self, game: usize) {
        if game < self.games.len() && self.can_edit_library() {
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
        let last = if self.can_edit_library() {
            add
        } else {
            n.saturating_sub(1)
        };
        let cols = self.cols.max(1);
        match k {
            Key::Left => self.game = self.game.saturating_sub(1),
            Key::Right => self.game = (self.game + 1).min(last),
            Key::Up => {
                if self.game == add {
                    // Already on the button; there is nothing above it.
                } else if self.game >= cols {
                    self.game -= cols;
                } else if self.can_edit_library() {
                    self.game = add;
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
                if self.game >= n {
                    self.screen = Screen::AddGame;
                    self.add = AddState::default();
                } else {
                    self.open_game_page();
                }
            }
            Key::Delete => self.offer_delete(self.game),
            _ => {}
        }
        Action::None
    }

    fn key_game(&mut self, k: Key) -> Action {
        let labels = self.actions();
        let saves = self.cur_saves().len();
        match k {
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
                return self.request_launch(self.game, Some(self.save));
            }
            return Action::None;
        }
        if !self.action_enabled(self.action) {
            return Action::None;
        }
        match self.actions().get(self.action).copied() {
            // Continue picks up the newest save, which is the one at the top.
            Some("Continue") => self.request_launch(self.game, Some(0)),
            Some("New game") => self.request_launch(self.game, None),
            Some("Resume") => Action::Resume,
            Some("Save") => Action::Save,
            Some("Settings") => {
                self.screen = Screen::Settings;
                Action::None
            }
            // The library, over the paused game. It is a screen, not an exit: the
            // game stays where it is, and coming back resumes it.
            Some("Library") => {
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
                _ => {}
            }
            return Action::None;
        }
        match k {
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
    /// Except from inside a game: see `can_edit_library`.
    pub fn dropped(&mut self, path: &str) -> Action {
        if !self.can_edit_library() {
            return Action::None;
        }
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
        // Assigned unconditionally, so sliding off a control clears the highlight
        // — unlike the cursor below, which stays where it was put.
        self.hover = self.hit(x, y);
        if let Some(hit) = self.hover {
            self.point_at(hit);
        }
        Action::None
    }

    /// Is the pointer over `h`? For the controls the cursor never lands on.
    pub(crate) fn hovering(&self, h: Hit) -> bool {
        self.hover == Some(h)
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
        // A click is also the pointer telling us where it is: on a touchpad tap the
        // press can be the first the UI hears of that position, with no move before it.
        self.hover = self.hit(x, y);
        match self.hit(x, y) {
            Some(hit) => {
                self.point_at(hit);
                match hit {
                    Hit::Game(_) | Hit::Add => self.key(Key::Enter),
                    Hit::Action(_) | Hit::Save(_) => self.activate(),
                    Hit::Exe(i) => Action::PickExe(i),
                    Hit::Paste => Action::Paste,
                    // One idea of "back", whether it is pressed, pointed at, or
                    // typed.
                    Hit::Back | Hit::SheetCancel => self.back(),
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
                    Hit::ConfirmYes => self.key(Key::Enter),
                    Hit::ConfirmNo => self.back(),
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
            Hit::ConfirmYes | Hit::ConfirmNo => {
                let yes = hit == Hit::ConfirmYes;
                if let Some(c) = self.confirm.as_mut() {
                    c.yes_focused = yes;
                }
            }
            Hit::Back | Hit::SheetCancel | Hit::Paste => {}
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
    /// When `over_game` the page is laid over the frozen frame the host has
    /// already drawn, rather than over the library's own background. That is the
    /// *only* difference: every screen has the same size and the same place in
    /// both, so opening the menu over a game, or walking from it back to the
    /// library, never moves anything under the player's cursor.
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

        // Same in the pause menu, when the machine stopped somewhere unsavable.
        let mut u = ui(&[("Zeliard", 0)]);
        u.open_overlay(0, false);
        assert!(u.action_enabled(u.action));
        assert_eq!(u.actions()[u.action], "Resume");
    }

    #[test]
    fn the_cursor_steps_over_disabled_actions() {
        // No saves => Continue is disabled. Wrapping upward from New game must
        // skip past it to Settings, not come to rest on a dead row.
        let mut u = ui(&[("Zeliard", 0)]);
        u.key(Key::Enter);
        assert_eq!(u.actions()[u.action], "New game");
        u.key(Key::Up);
        assert_eq!(u.actions()[u.action], "Settings");
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
    fn the_pause_menu_offers_save_and_resume_and_the_library() {
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
    }

    #[test]
    fn going_to_the_library_from_a_paused_game_does_not_end_it() {
        // The whole point: the library is a screen laid *over* the game, exactly
        // as the pause menu is. It used to re-exec the player, which threw the
        // running game away for the crime of wanting to look at the list.
        let mut u = ui(&[("Zeliard", 1), ("Pop", 0)]);
        u.open_overlay(0, true);
        u.action = 4;
        assert_eq!(u.actions()[u.action], "Library");
        assert_eq!(
            u.key(Key::Enter),
            Action::None,
            "nothing is asked of the host"
        );
        assert_eq!(u.screen, Screen::Library);
        assert_eq!(u.running, Some(0), "the game is still there, still paused");

        // Back goes to the paused game's page, and back again into the game.
        assert_eq!(u.key(Key::Escape), Action::None);
        assert_eq!((u.screen, u.game), (Screen::Game, 0));
        assert_eq!(u.key(Key::Escape), Action::Resume);

        // ...and F12 from the library goes straight back into the game.
        u.screen = Screen::Library;
        assert_eq!(u.key(Key::Overlay), Action::Resume);
    }

    #[test]
    fn another_games_page_browsed_from_a_paused_game_is_not_the_pause_menu() {
        // `running` is an index, not a flag, precisely for this: the page you are
        // looking at need not be the game you are in. Offering Resume/Save on
        // Pop's page while Zeliard is the game that is actually paused would be a
        // Resume that resumes something else.
        let mut u = ui(&[("Zeliard", 1), ("Pop", 2)]);
        u.open_overlay(0, true);
        u.screen = Screen::Library;
        u.key(Key::Right); // onto Pop
        u.key(Key::Enter);
        assert_eq!((u.screen, u.game), (Screen::Game, 1));
        assert_eq!(u.actions(), ["Continue", "New game", "Settings"]);
        // And back from it is the library, not the game.
        assert_eq!(u.key(Key::Escape), Action::None);
        assert_eq!(u.screen, Screen::Library);
    }

    #[test]
    fn starting_a_game_over_a_paused_one_asks_first() {
        for (label, restore, game) in [
            ("another game", None, 1),
            ("a new game", None, 0),
            ("a save", Some(0), 0),
        ] {
            let mut u = ui(&[("Zeliard", 1), ("Pop", 1)]);
            u.open_overlay(0, true);
            u.game = game;

            let asked = u.request_launch(game, restore);
            assert_eq!(asked, Action::None, "{label}: must not launch on the spot");
            let c = u
                .confirm
                .as_ref()
                .unwrap_or_else(|| panic!("{label}: nothing was asked"));
            assert!(!c.yes_focused, "{label}: the cursor starts on Cancel");

            // Enter as it stands answers Cancel — and answers it for good.
            assert_eq!(u.key(Key::Enter), Action::None);
            assert_eq!(u.confirm, None);
            assert_eq!(u.running, Some(0), "{label}: the game is still paused");

            // Choosing the other button, and only that, launches.
            u.request_launch(game, restore);
            u.key(Key::Right);
            assert_eq!(u.key(Key::Enter), Action::Launch { game, restore });
            assert_eq!(u.confirm, None);
        }
    }

    #[test]
    fn nothing_is_asked_when_there_is_nothing_to_lose() {
        // No game running: a launch is not destroying anything, so it must not
        // grow a dialog in front of it.
        let mut u = ui(&[("Zeliard", 1)]);
        u.key(Key::Enter);
        assert_eq!(
            u.key(Key::Enter),
            Action::Launch {
                game: 0,
                restore: Some(0)
            }
        );
        assert_eq!(u.confirm, None);
    }

    #[test]
    fn escape_in_the_library_quits_but_never_while_a_game_is_paused_behind_it() {
        let mut u = ui(&[("Zeliard", 0)]);
        assert_eq!(u.key(Key::Escape), Action::Quit);
        assert!(!u.has_back(), "the root library has nowhere to go back to");

        // Reached the library from a paused game: back must go to it, not throw
        // the game away.
        let mut u = ui(&[("Zeliard", 0)]);
        u.open_overlay(0, true);
        u.screen = Screen::Library;
        assert!(u.has_back());
        assert_eq!(u.key(Key::Escape), Action::None);
        assert_eq!(u.screen, Screen::Game);
    }

    #[test]
    fn the_library_is_browse_only_while_a_game_is_paused() {
        // Adding or removing a game renumbers the list the paused game is
        // identified by, and none of it is work that cannot wait until you have
        // left the game. So: no Add, no "…", no Delete, no drop.
        let mut u = ui(&[("Zeliard", 0), ("Pop", 0)]);
        u.open_overlay(0, true);
        u.screen = Screen::Library;
        let mut cv = Canvas::new(1280, 800);
        u.paint(&mut cv, true);
        assert!(!painted(&u, Hit::Add), "no Add button over a running game");
        assert!(!painted(&u, Hit::Menu(0)), "no card menu either");

        u.key(Key::Delete);
        assert_eq!(u.confirm, None, "Delete cannot remove a game from here");
        u.open_menu(1);
        assert_eq!(u.menu, None);
        assert_eq!(u.dropped("/tmp/g.zip"), Action::None);
        assert_eq!(u.screen, Screen::Library, "and a drop did not navigate");

        // The cursor cannot walk off the end of the grid onto a button that is
        // not there.
        u.key(Key::Right);
        u.key(Key::Right);
        assert_eq!(u.game, 1);
        u.key(Key::Up);
        assert_eq!(u.game, 1, "there is nothing above the top row");
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

    fn rect_of(u: &Ui, h: Hit) -> Rect {
        u.hot
            .iter()
            .find(|(_, x)| *x == h)
            .unwrap_or_else(|| panic!("{h:?} was never painted"))
            .0
    }

    /// The real splash — the one the player hands the UI at startup. The header is
    /// cut out of *this* file, so a test that cut up a stand-in would be checking
    /// nothing.
    fn logo() -> Image {
        Image::decode_png(include_bytes!("../../runtime/assets/saisei_logo.png"))
            .expect("the splash must decode")
    }

    /// How much of a rect has anything drawn in it, as a fraction of its pixels.
    fn ink(cv: &Canvas, r: Rect) -> f32 {
        let (mut lit, mut n) = (0, 0);
        for y in r.y as usize..(r.y + r.h) as usize {
            for x in r.x as usize..(r.x + r.w) as usize {
                let i = (y * cv.w + x) * 4;
                // Against the page itself, not against black: the backdrop is a very
                // dark plum, and counting *that* as ink would pass on an empty screen.
                if cv.px[i] as u32 + cv.px[i + 1] as u32 + cv.px[i + 2] as u32 > 0x60 {
                    lit += 1;
                }
                n += 1;
            }
        }
        lit as f32 / n.max(1) as f32
    }

    #[test]
    fn the_header_is_cut_from_the_logo() {
        // The wordmark and the mark are not assets of their own: they are two boxes
        // of the splash. Which is only true for as long as the boxes are where the
        // splash keeps them — so this is the thing that has to be checked, and it
        // cannot be checked against a stand-in image.
        let logo = logo();
        let u = Ui::new(logo.clone(), vec![]);
        assert!(!u.mark.is_empty() && !u.wordmark.is_empty());
        // The wordmark is a word: far wider than it is tall. The mark is a picture.
        assert!(u.wordmark.w > u.wordmark.h * 3, "that is not a wordmark");
        assert!(u.mark.w < u.mark.h * 3, "that is not the tree");

        // How lit a picture is, on average. The art is on black, so a box that caught
        // nothing is nearly nought and a box with a picture in it is not.
        let lit = |img: &Image| {
            img.rgb
                .chunks_exact(3)
                .map(|p| p[0].max(p[1]).max(p[2]) as f32)
                .sum::<f32>()
                / (img.w * img.h).max(1) as f32
        };
        let (tree, word) = (lit(&u.mark), lit(&u.wordmark));
        assert!(tree > 20.0, "the mark's box caught no tree ({tree})");
        assert!(word > 20.0, "the wordmark's box caught no word ({word})");

        // And the cut between them runs through the black that separates them in the
        // art, rather than through either picture. If the splash is ever redrawn with
        // the word tucked up against the tree, this is what says so — instead of a
        // header that quietly grows a slice of somebody else's half.
        let cut_top = (MARK_BOX[1] + MARK_BOX[3]) * logo.h as f32;
        let cut_bot = WORDMARK_BOX[1] * logo.h as f32;
        assert!(cut_top < cut_bot, "the two boxes overlap");
        let gutter = lit(&logo.crop(
            0,
            cut_top as usize,
            logo.w,
            (cut_bot - cut_top).max(1.0) as usize,
        ));
        assert!(
            gutter * 3.0 < tree.min(word),
            "the cut runs through the picture ({gutter}), not the gap between them"
        );
    }

    #[test]
    fn the_brand_is_on_every_screen() {
        // The logo used to be the first two seconds of the session and then nothing.
        // Every screen wears it now — including the ones over a paused game, where
        // the *only* thing that may differ is the backdrop.
        let mut u = Ui::new(
            logo(),
            vec![GameView {
                key: "zeliard".into(),
                title: "Zeliard".into(),
                saves: vec![],
                programs: vec![],
            }],
        );
        let mut cv = Canvas::new(1280, 800);
        let word = Rect::new(44.0, 50.0, 160.0, 40.0);
        let mark = Rect::new(1100.0, 4.0, 176.0, 84.0);

        for (screen, over) in [
            (Screen::Library, false),
            (Screen::Game, false),
            (Screen::Settings, false),
            (Screen::AddGame, false),
            (Screen::Game, true),
            (Screen::Library, true),
        ] {
            if over {
                u.open_overlay(0, true);
            }
            u.screen = screen;
            u.paint(&mut cv, over);
            assert!(
                ink(&cv, word) > 0.10,
                "{screen:?} (over_game={over}) has no wordmark"
            );
            assert!(
                ink(&cv, mark) > 0.05,
                "{screen:?} (over_game={over}) has no mark in the corner"
            );
        }
    }

    #[test]
    fn a_selection_is_a_ring_and_never_a_slab() {
        // One language for "this is the one", everywhere. The library's cards said it
        // with a ring while the buttons an inch away said it with a slab of solid
        // pink, so the same keypress looked like two different things depending on
        // which half of the screen it landed in.
        let mut u = ui(&[("Zeliard", 1)]);
        let mut cv = Canvas::new(1280, 800);
        // Is any pixel in here the blossom? An edge is antialiased and a label has
        // gaps, so both questions are asked of a *region*, never of one pixel.
        let accent = |cv: &Canvas, r: Rect| {
            (r.y as usize..(r.y + r.h) as usize).any(|y| {
                (r.x as usize..(r.x + r.w) as usize).any(|x| {
                    let i = (y * cv.w + x) * 4;
                    let (r, g, b) = (cv.px[i], cv.px[i + 1], cv.px[i + 2]);
                    r > 0xC0 && (0x50..0xB0).contains(&g) && b > 0x80
                })
            })
        };

        // The action the cursor is on, on a game's page: ringed by the accent...
        u.key(Key::Enter);
        u.paint(&mut cv, false);
        let sel = rect_of(&u, Hit::Action(u.action));
        assert!(
            accent(&cv, Rect::new(sel.x, sel.y + sel.h / 3.0, 3.0, sel.h / 3.0)),
            "the chosen action wears no ring"
        );
        // ...and not filled by it. Past the label, where nothing but a fill could put
        // the accent.
        assert!(
            !accent(
                &cv,
                Rect::new(sel.x + sel.w * 0.7, sel.y + 6.0, sel.w * 0.25, sel.h - 12.0)
            ),
            "the chosen action is filled with the accent — it may only be ringed by it"
        );

        // The same for the button in the library's bar, which is the same idea and
        // used to be the same slab.
        u.key(Key::Escape);
        u.key(Key::Up); // out of the grid, onto Add game
        u.paint(&mut cv, false);
        let add = rect_of(&u, Hit::Add);
        assert!(
            !accent(
                &cv,
                Rect::new(add.x + add.w / 2.0, add.y + 5.0, add.w / 4.0, 6.0)
            ),
            "Add game is filled with the accent"
        );
    }

    #[test]
    fn every_screen_but_the_root_library_draws_a_way_back() {
        // The rule, as a test: no screen may rely on the player knowing that
        // Escape exists.
        let mut u = ui(&[("Zeliard", 1)]);
        let mut cv = Canvas::new(1280, 800);

        u.paint(&mut cv, false);
        assert!(!painted(&u, Hit::Back), "the library is where you start");

        for screen in [Screen::Game, Screen::Settings, Screen::AddGame] {
            u.screen = screen;
            u.paint(&mut cv, false);
            assert!(painted(&u, Hit::Back), "{screen:?} has no back button");
        }

        // Including the library, once it is a screen over a paused game.
        u.open_overlay(0, true);
        u.screen = Screen::Library;
        u.paint(&mut cv, true);
        assert!(painted(&u, Hit::Back), "the library over a game has one");

        // And the sheets, which cover it, carry their own.
        u.running = None;
        u.screen = Screen::Library;
        u.open_menu(0);
        u.paint(&mut cv, false);
        assert!(
            painted(&u, Hit::SheetCancel),
            "the card menu has no way out"
        );
        assert_eq!(u.click_at(rect_of(&u, Hit::SheetCancel)), Action::None);
        assert_eq!(u.menu, None);
    }

    #[test]
    fn the_back_button_answers_the_pointer() {
        // It used to be painted in SURFACE_HI — the colour reserved for "hovered" —
        // no matter where the mouse was, so hovering it changed nothing: the one
        // state it could show was the one it never left. Assert on the *pixels*, not
        // on a flag, because a flag nothing paints is exactly the bug that was here.
        fn px(cv: &Canvas, r: Rect) -> Vec<u8> {
            let (x, y) = ((r.x + r.w / 2.0) as usize, (r.y + r.h / 2.0) as usize);
            let i = (y * cv.w + x) * 4;
            cv.px[i..i + 4].to_vec()
        }

        let mut u = ui(&[("Zeliard", 1)]);
        let mut cv = Canvas::new(1280, 800);
        u.screen = Screen::Settings;

        u.paint(&mut cv, false);
        let btn = rect_of(&u, Hit::Back);
        // Off the button entirely — the far corner of the page.
        u.mouse_move(cv.w as f32 - 1.0, cv.h as f32 - 1.0);
        u.paint(&mut cv, false);
        let idle = px(&cv, btn);

        u.mouse_move(btn.x + btn.w / 2.0, btn.y + btn.h / 2.0);
        assert!(u.hovering(Hit::Back), "the pointer is on it");
        u.paint(&mut cv, false);
        assert_ne!(px(&cv, btn), idle, "hovering Back must change how it looks");

        // And sliding off it puts it back — a highlight that never leaves is the
        // same as no highlight at all.
        u.mouse_move(cv.w as f32 - 1.0, cv.h as f32 - 1.0);
        assert!(!u.hovering(Hit::Back));
        u.paint(&mut cv, false);
        assert_eq!(px(&cv, btn), idle, "leaving Back must undo the highlight");
    }

    #[test]
    fn the_frame_never_moves_between_screens() {
        // "No dramatic resizes": every screen is laid out in the same rect, and a
        // screen over a running game is laid out in the same rect as the same
        // screen with no game behind it. The back button is on all of them, so
        // where it lands is a fair proxy for where the page begins.
        let mut u = ui(&[("Zeliard", 2), ("Pop", 0)]);
        let mut cv = Canvas::new(1280, 800);

        u.open_overlay(0, true);
        u.paint(&mut cv, true);
        let back = rect_of(&u, Hit::Back);
        let card_in_game = {
            u.screen = Screen::Library;
            u.paint(&mut cv, true);
            (rect_of(&u, Hit::Back), rect_of(&u, Hit::Game(0)))
        };
        assert_eq!(back, card_in_game.0, "the bar moved between screens");

        for screen in [Screen::Settings, Screen::AddGame] {
            u.screen = screen;
            u.paint(&mut cv, true);
            assert_eq!(rect_of(&u, Hit::Back), back, "{screen:?} moved the bar");
        }

        // The same library, with no game behind it: same bar, same cards, same
        // place. Only the backdrop changes.
        u.running = None;
        u.screen = Screen::Library;
        u.paint(&mut cv, false);
        assert_eq!(
            rect_of(&u, Hit::Game(0)),
            card_in_game.1,
            "the library resized itself when it stopped being an overlay"
        );
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

    impl Ui {
        /// Click the middle of a rect the paint put on screen.
        fn click_at(&mut self, r: Rect) -> Action {
            self.click(r.x + r.w / 2.0, r.y + r.h / 2.0)
        }
    }

    /// Click whatever `h` was painted at. Panics if it was not painted at all,
    /// which is the point: a button nobody drew is a button nobody can press.
    fn click_hit(u: &mut Ui, h: Hit) -> Action {
        let r = rect_of(u, h);
        u.click_at(r)
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
        assert_eq!(u.confirm, None);

        // Remove is the only thing on offer for a game with nothing else to run,
        // and taking it asks rather than deletes.
        assert_eq!(u.menu_items(0), ["Remove game"]);
        u.paint(&mut cv, false);
        assert_eq!(click_hit(&mut u, Hit::MenuItem(0)), Action::None);
        assert_eq!(u.menu, None, "the menu closes behind the question");
        assert!(u.confirm.is_some());

        // Enter straight away must NOT delete: the focus starts on Cancel, so the
        // dangerous answer is never the one a stray keypress gives.
        assert_eq!(u.key(Key::Enter), Action::None);
        assert_eq!(u.confirm, None, "the question is answered either way");

        // Escape cancels too. Delete still opens the question straight from the
        // grid — the menu is another way in, not the only one.
        u.key(Key::Delete);
        assert!(u.confirm.is_some());
        assert_eq!(u.key(Key::Escape), Action::None);
        assert_eq!(u.confirm, None);

        // Choosing Remove, and only that, asks the host to delete it.
        u.key(Key::Delete);
        u.key(Key::Right); // Cancel -> Remove
        assert_eq!(u.key(Key::Enter), Action::DeleteGame(0));
        assert_eq!(u.confirm, None);
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

        // Back goes to the menu it came from, rather than dumping you out on the
        // grid a level further than you asked to go.
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
        assert!(u.confirm.is_some(), "the only row is Remove");
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
        let card = rect_of(&u, Hit::Game(1));

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
            assert_eq!(u.click_at(card), Action::None);
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
        let card = rect_of(&u, Hit::Game(1));

        u.key(Key::Delete);
        u.paint(&mut cv, false);
        assert!(
            !painted(&u, Hit::Game(1)),
            "the library stopped being clickable"
        );
        assert!(painted(&u, Hit::ConfirmYes) && painted(&u, Hit::ConfirmNo));

        // Clicking where a card used to be does nothing at all.
        assert_eq!(u.click_at(card), Action::None);
        assert_eq!(u.screen, Screen::Library);
        assert!(u.confirm.is_some(), "still waiting on an answer");

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
        assert_eq!(click_hit(&mut u, Hit::Paste), Action::Paste);

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
        let act = click_hit(&mut u, Hit::Game(1));
        assert_eq!(u.game, 1);
        assert_eq!(u.screen, Screen::Game);
        assert_eq!(act, Action::None);
    }
}
