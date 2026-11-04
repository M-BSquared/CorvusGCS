//! A terminal screen: the bytes of a shell in, a grid of cells out. The
//! counterpart of xterm.js in `python/src/js/ssh-term.js`, with what a shell and
//! the programs run from one need: colours (16, 256, true colour), cursor
//! movement, erasing, scrolling regions, the alternate screen of `vim` and
//! `top`, and 5000 lines of scrollback.

use std::collections::VecDeque;

use vte::{Params, Parser, Perform};

pub const SCROLLBACK: usize = 5000;

/// A colour a program asked for.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Colour {
    #[default]
    Default,
    Index(u8),
    Rgb(u8, u8, u8),
}

/// One character cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub fg: Colour,
    pub bg: Colour,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Default for Cell {
    fn default() -> Cell {
        Cell {
            ch: ' ',
            fg: Colour::Default,
            bg: Colour::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
        }
    }
}

type Line = Vec<Cell>;

/// The grid, the cursor and the modes a program can switch.
pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    grid: Vec<Line>,
    pub scrollback: VecDeque<Line>,
    pub x: usize,
    pub y: usize,
    pen: Cell,
    saved: (usize, usize, Cell),
    top: usize,
    bottom: usize,
    wrap_pending: bool,
    main: Option<(Vec<Line>, (usize, usize))>,
    pub app_cursor: bool,
    pub cursor_visible: bool,
    pub bracketed_paste: bool,
    pub title: String,
    tabs: Vec<bool>,
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Screen {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Screen {
            cols,
            rows,
            grid: vec![vec![Cell::default(); cols]; rows],
            scrollback: VecDeque::new(),
            x: 0,
            y: 0,
            pen: Cell::default(),
            saved: (0, 0, Cell::default()),
            top: 0,
            bottom: rows - 1,
            wrap_pending: false,
            main: None,
            app_cursor: false,
            cursor_visible: true,
            bracketed_paste: false,
            title: String::new(),
            tabs: (0..cols).map(|c| c % 8 == 0).collect(),
        }
    }

    pub fn on_alt_screen(&self) -> bool {
        self.main.is_some()
    }

    /// The cell at a visible position.
    pub fn cell(&self, x: usize, y: usize) -> Cell {
        self.grid.get(y).and_then(|r| r.get(x)).copied().unwrap_or_default()
    }

    /// A line counted from `offset` lines above the live screen (0 is the
    /// live screen's first line when `back` is 0).
    pub fn line_at(&self, back: usize, y: usize) -> &Line {
        let sb = self.scrollback.len();
        let back = back.min(sb);
        let idx = sb - back + y;
        if idx < sb {
            &self.scrollback[idx]
        } else {
            &self.grid[(idx - sb).min(self.rows - 1)]
        }
    }

    fn blank(&self) -> Cell {
        Cell {
            ch: ' ',
            bg: self.pen.bg,
            ..Cell::default()
        }
    }

    fn blank_line(&self) -> Line {
        vec![self.blank(); self.cols]
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if cols == self.cols && rows == self.rows {
            return;
        }
        for line in self.grid.iter_mut() {
            line.resize(cols, Cell::default());
        }
        for line in self.scrollback.iter_mut() {
            line.resize(cols, Cell::default());
        }
        while self.grid.len() > rows {
            let line = self.grid.remove(0);
            if self.main.is_none() {
                self.push_scrollback(line);
            }
            self.y = self.y.saturating_sub(1);
        }
        while self.grid.len() < rows {
            self.grid.push(vec![Cell::default(); cols]);
        }
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.x = self.x.min(cols - 1);
        self.y = self.y.min(rows - 1);
        self.wrap_pending = false;
        self.tabs = (0..cols).map(|c| c % 8 == 0).collect();
    }

    fn push_scrollback(&mut self, line: Line) {
        self.scrollback.push_back(line);
        while self.scrollback.len() > SCROLLBACK {
            self.scrollback.pop_front();
        }
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n {
            let line = self.grid.remove(self.top);
            if self.top == 0 && self.main.is_none() {
                self.push_scrollback(line);
            }
            let blank = self.blank_line();
            self.grid.insert(self.bottom, blank);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n {
            self.grid.remove(self.bottom);
            let blank = self.blank_line();
            self.grid.insert(self.top, blank);
        }
    }

    fn linefeed(&mut self) {
        if self.y == self.bottom {
            self.scroll_up(1);
        } else if self.y + 1 < self.rows {
            self.y += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.y == self.top {
            self.scroll_down(1);
        } else {
            self.y = self.y.saturating_sub(1);
        }
    }

    fn put(&mut self, c: char) {
        if self.wrap_pending {
            self.x = 0;
            self.linefeed();
            self.wrap_pending = false;
        }
        let mut cell = self.pen;
        cell.ch = c;
        self.grid[self.y][self.x] = cell;
        if self.x + 1 >= self.cols {
            self.wrap_pending = true;
        } else {
            self.x += 1;
        }
    }

    fn erase_line(&mut self, y: usize, from: usize, to: usize) {
        let blank = self.blank();
        let to = to.min(self.cols);
        for c in from.min(to)..to {
            self.grid[y][c] = blank;
        }
    }

    fn enter_alt(&mut self) {
        if self.main.is_none() {
            let saved = std::mem::replace(&mut self.grid, vec![vec![Cell::default(); self.cols]; self.rows]);
            self.main = Some((saved, (self.x, self.y)));
            self.x = 0;
            self.y = 0;
            self.top = 0;
            self.bottom = self.rows - 1;
        }
    }

    fn leave_alt(&mut self) {
        if let Some((grid, (x, y))) = self.main.take() {
            self.grid = grid;
            self.x = x.min(self.cols - 1);
            self.y = y.min(self.rows - 1);
            self.top = 0;
            self.bottom = self.rows - 1;
        }
    }
}

/// The parser and the screen it draws on.
pub struct Emulator {
    parser: Parser,
    pub screen: Screen,
}

impl Emulator {
    pub fn new(cols: usize, rows: usize) -> Emulator {
        Emulator {
            parser: Parser::new(),
            screen: Screen::new(cols, rows),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.screen, bytes);
    }

    pub fn reset(&mut self) {
        let (c, r) = (self.screen.cols, self.screen.rows);
        self.parser = Parser::new();
        self.screen = Screen::new(c, r);
    }
}

fn param(params: &Params, i: usize, default: usize) -> usize {
    params
        .iter()
        .nth(i)
        .and_then(|p| p.first().copied())
        .filter(|v| *v != 0)
        .map_or(default, |v| v as usize)
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        self.put(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => {
                self.wrap_pending = false;
                self.linefeed();
            }
            b'\r' => {
                self.x = 0;
                self.wrap_pending = false;
            }
            0x08 => {
                self.x = self.x.saturating_sub(1);
                self.wrap_pending = false;
            }
            b'\t' => {
                let mut c = self.x + 1;
                while c < self.cols - 1 && !self.tabs[c] {
                    c += 1;
                }
                self.x = c.min(self.cols - 1);
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell: bool) {
        if matches!(params.first().copied(), Some(b"0" | b"2")) {
            if let Some(t) = params.get(1) {
                self.title = String::from_utf8_lossy(t).into_owned();
            }
        }
    }

    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], _ignore: bool, action: char) {
        let n = |i, d| param(params, i, d);
        let private = inter.first() == Some(&b'?');
        match action {
            'A' => {
                self.y = self
                    .y
                    .saturating_sub(n(0, 1))
                    .max(if self.y >= self.top { self.top } else { 0 })
            }
            'B' | 'e' => {
                self.y = (self.y + n(0, 1)).min(if self.y <= self.bottom {
                    self.bottom
                } else {
                    self.rows - 1
                })
            }
            'C' | 'a' => self.x = (self.x + n(0, 1)).min(self.cols - 1),
            'D' => self.x = self.x.saturating_sub(n(0, 1)),
            'E' => {
                self.y = (self.y + n(0, 1)).min(self.rows - 1);
                self.x = 0;
            }
            'F' => {
                self.y = self.y.saturating_sub(n(0, 1));
                self.x = 0;
            }
            'G' | '`' => self.x = (n(0, 1) - 1).min(self.cols - 1),
            'd' => self.y = (n(0, 1) - 1).min(self.rows - 1),
            'H' | 'f' => {
                self.y = (n(0, 1) - 1).min(self.rows - 1);
                self.x = (n(1, 1) - 1).min(self.cols - 1);
            }
            'J' => {
                let mode = params.iter().next().and_then(|p| p.first().copied()).unwrap_or(0);
                match mode {
                    0 => {
                        self.erase_line(self.y, self.x, self.cols);
                        for y in self.y + 1..self.rows {
                            self.erase_line(y, 0, self.cols);
                        }
                    }
                    1 => {
                        for y in 0..self.y {
                            self.erase_line(y, 0, self.cols);
                        }
                        self.erase_line(self.y, 0, self.x + 1);
                    }
                    2 => {
                        for y in 0..self.rows {
                            self.erase_line(y, 0, self.cols);
                        }
                    }
                    3 => self.scrollback.clear(),
                    _ => {}
                }
            }
            'K' => {
                let mode = params.iter().next().and_then(|p| p.first().copied()).unwrap_or(0);
                match mode {
                    0 => self.erase_line(self.y, self.x, self.cols),
                    1 => self.erase_line(self.y, 0, self.x + 1),
                    2 => self.erase_line(self.y, 0, self.cols),
                    _ => {}
                }
            }
            'L' => {
                if self.y >= self.top && self.y <= self.bottom {
                    for _ in 0..n(0, 1).min(self.bottom - self.y + 1) {
                        self.grid.remove(self.bottom);
                        let blank = self.blank_line();
                        self.grid.insert(self.y, blank);
                    }
                }
            }
            'M' => {
                if self.y >= self.top && self.y <= self.bottom {
                    for _ in 0..n(0, 1).min(self.bottom - self.y + 1) {
                        self.grid.remove(self.y);
                        let blank = self.blank_line();
                        self.grid.insert(self.bottom, blank);
                    }
                }
            }
            'P' => {
                let k = n(0, 1).min(self.cols - self.x);
                let blank = self.blank();
                let line = &mut self.grid[self.y];
                line.drain(self.x..self.x + k);
                line.extend(std::iter::repeat_n(blank, k));
            }
            '@' => {
                let k = n(0, 1).min(self.cols - self.x);
                let blank = self.blank();
                let line = &mut self.grid[self.y];
                for _ in 0..k {
                    line.pop();
                    line.insert(self.x, blank);
                }
            }
            'X' => {
                let k = n(0, 1);
                self.erase_line(self.y, self.x, self.x + k);
            }
            'S' => self.scroll_up(n(0, 1)),
            'T' => self.scroll_down(n(0, 1)),
            'r' => {
                let top = n(0, 1) - 1;
                let bottom = n(1, self.rows) - 1;
                if top < bottom && bottom < self.rows {
                    self.top = top;
                    self.bottom = bottom;
                    self.x = 0;
                    self.y = 0;
                }
            }
            's' if !private => self.saved = (self.x, self.y, self.pen),
            'u' if !private => {
                self.x = self.saved.0.min(self.cols - 1);
                self.y = self.saved.1.min(self.rows - 1);
                self.pen = self.saved.2;
            }
            'h' | 'l' if private => {
                let on = action == 'h';
                for p in params.iter() {
                    match p.first().copied().unwrap_or(0) {
                        1 => self.app_cursor = on,
                        25 => self.cursor_visible = on,
                        47 | 1047 | 1049 => {
                            if on {
                                self.enter_alt();
                            } else {
                                self.leave_alt();
                            }
                        }
                        2004 => self.bracketed_paste = on,
                        _ => {}
                    }
                }
            }
            'm' => self.sgr(params),
            _ => {}
        }
        self.wrap_pending = false;
    }

    fn esc_dispatch(&mut self, inter: &[u8], _ignore: bool, byte: u8) {
        if !inter.is_empty() {
            return;
        }
        match byte {
            b'7' => self.saved = (self.x, self.y, self.pen),
            b'8' => {
                self.x = self.saved.0.min(self.cols - 1);
                self.y = self.saved.1.min(self.rows - 1);
                self.pen = self.saved.2;
            }
            b'M' => self.reverse_index(),
            b'D' => self.linefeed(),
            b'E' => {
                self.x = 0;
                self.linefeed();
            }
            b'H' => {
                if self.x < self.cols {
                    self.tabs[self.x] = true;
                }
            }
            b'c' => {
                let (c, r) = (self.cols, self.rows);
                *self = Screen::new(c, r);
            }
            _ => {}
        }
        self.wrap_pending = false;
    }
}

impl Screen {
    fn sgr(&mut self, params: &Params) {
        let flat: Vec<u16> = params.iter().flat_map(|p| p.iter().copied()).collect();
        if flat.is_empty() {
            self.pen = Cell::default();
            return;
        }
        let mut i = 0;
        while i < flat.len() {
            match flat[i] {
                0 => self.pen = Cell::default(),
                1 => self.pen.bold = true,
                2 => self.pen.dim = true,
                3 => self.pen.italic = true,
                4 => self.pen.underline = true,
                7 => self.pen.inverse = true,
                22 => {
                    self.pen.bold = false;
                    self.pen.dim = false;
                }
                23 => self.pen.italic = false,
                24 => self.pen.underline = false,
                27 => self.pen.inverse = false,
                30..=37 => self.pen.fg = Colour::Index((flat[i] - 30) as u8),
                39 => self.pen.fg = Colour::Default,
                40..=47 => self.pen.bg = Colour::Index((flat[i] - 40) as u8),
                49 => self.pen.bg = Colour::Default,
                90..=97 => self.pen.fg = Colour::Index((flat[i] - 90 + 8) as u8),
                100..=107 => self.pen.bg = Colour::Index((flat[i] - 100 + 8) as u8),
                38 | 48 => {
                    let fg = flat[i] == 38;
                    let colour = match flat.get(i + 1) {
                        Some(5) => {
                            let c = flat.get(i + 2).map(|v| Colour::Index(*v as u8));
                            i += 2;
                            c
                        }
                        Some(2) => {
                            let c = (flat.get(i + 2), flat.get(i + 3), flat.get(i + 4));
                            i += 4;
                            match c {
                                (Some(r), Some(g), Some(b)) => Some(Colour::Rgb(*r as u8, *g as u8, *b as u8)),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    if let Some(c) = colour {
                        if fg {
                            self.pen.fg = c;
                        } else {
                            self.pen.bg = c;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// The text of one line, without trailing blanks.
    #[cfg(test)]
    pub fn line_text(line: &Line) -> String {
        let s: String = line.iter().map(|c| c.ch).collect();
        s.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(e: &Emulator, y: usize) -> String {
        Screen::line_text(&e.screen.grid[y])
    }

    #[test]
    fn text_wraps_and_scrolls_into_the_scrollback() {
        let mut e = Emulator::new(5, 2);
        e.feed(b"abcdefg\r\nxy\r\nz");
        assert_eq!(text(&e, 0), "xy");
        assert_eq!(text(&e, 1), "z");
        assert_eq!(Screen::line_text(&e.screen.scrollback[0]), "abcde");
        assert_eq!(Screen::line_text(&e.screen.scrollback[1]), "fg");
    }

    #[test]
    fn cursor_moves_and_erases() {
        let mut e = Emulator::new(10, 3);
        e.feed(b"hello\x1b[3D\x1b[K!");
        assert_eq!(text(&e, 0), "he!");
        e.feed(b"\x1b[2;4Hx\x1b[1;1H\x1b[2J");
        assert_eq!(text(&e, 0), "");
        assert_eq!(text(&e, 1), "");
    }

    #[test]
    fn colours_come_from_sgr() {
        let mut e = Emulator::new(10, 1);
        e.feed(b"\x1b[1;31ma\x1b[38;5;200mb\x1b[38;2;1;2;3mc\x1b[0md");
        assert_eq!(e.screen.cell(0, 0).fg, Colour::Index(1));
        assert!(e.screen.cell(0, 0).bold);
        assert_eq!(e.screen.cell(1, 0).fg, Colour::Index(200));
        assert_eq!(e.screen.cell(2, 0).fg, Colour::Rgb(1, 2, 3));
        assert_eq!(e.screen.cell(3, 0).fg, Colour::Default);
    }

    #[test]
    fn the_alternate_screen_leaves_the_shell_as_it_was() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"prompt$ ");
        e.feed(b"\x1b[?1049hvim\x1b[?1049l");
        assert_eq!(text(&e, 0), "prompt$");
        assert!(!e.screen.on_alt_screen());
    }

    #[test]
    fn scroll_regions_keep_the_rest_still() {
        let mut e = Emulator::new(4, 4);
        e.feed(b"a\r\nb\r\nc\r\nd");
        e.feed(b"\x1b[2;3r\x1b[3;1H\n");
        assert_eq!(text(&e, 0), "a");
        assert_eq!(text(&e, 1), "c");
        assert_eq!(text(&e, 3), "d");
    }

    #[test]
    fn deleting_and_inserting_characters() {
        let mut e = Emulator::new(8, 1);
        e.feed(b"abcdef\x1b[1;2H\x1b[2P");
        assert_eq!(text(&e, 0), "adef");
        e.feed(b"\x1b[1;2H\x1b[2@");
        assert_eq!(text(&e, 0), "a  def");
    }

    #[test]
    fn a_resize_keeps_the_cursor_on_the_screen() {
        let mut e = Emulator::new(10, 5);
        e.feed(b"\x1b[5;9H");
        e.screen.resize(4, 2);
        assert!(e.screen.x < 4 && e.screen.y < 2);
    }
}
