#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

impl Color {
    pub fn code(self) -> &'static str {
        match self {
            Color::Reset => "\x1b[0m",
            Color::Black => "\x1b[30m",
            Color::Red => "\x1b[31m",
            Color::Green => "\x1b[32m",
            Color::Yellow => "\x1b[33m",
            Color::Blue => "\x1b[34m",
            Color::Magenta => "\x1b[35m",
            Color::Cyan => "\x1b[36m",
            Color::White => "\x1b[37m",
            Color::BrightBlack => "\x1b[90m",
            Color::BrightRed => "\x1b[91m",
            Color::BrightGreen => "\x1b[92m",
            Color::BrightYellow => "\x1b[93m",
            Color::BrightBlue => "\x1b[94m",
            Color::BrightMagenta => "\x1b[95m",
            Color::BrightCyan => "\x1b[96m",
            Color::BrightWhite => "\x1b[97m",
        }
    }
}

pub fn get_terminal_width() -> usize {
    80
}

pub fn get_terminal_height() -> usize {
    24
}

pub fn is_terminal() -> bool {
    false
}

#[derive(Debug)]
pub struct ProgressBar {
    description: String,
    current: usize,
    total: usize,
    width: usize,
}

impl ProgressBar {
    pub fn new(description: &str, total: usize) -> Self {
        Self {
            description: description.to_string(),
            current: 0,
            total,
            width: 40,
        }
    }

    pub fn advance(&mut self, amount: usize) {
        self.current = (self.current + amount).min(self.total);
    }

    pub fn set(&mut self, value: usize) {
        self.current = value.min(self.total);
    }

    pub fn render(&self) -> String {
        let pct = if self.total > 0 {
            self.current as f64 / self.total as f64
        } else {
            1.0
        };
        let filled = (self.width as f64 * pct) as usize;
        let empty = self.width.saturating_sub(filled);
        format!(
            "{} [{}{}] {}/{}",
            self.description,
            "=".repeat(filled),
            " ".repeat(empty),
            self.current,
            self.total,
        )
    }

    pub fn is_finished(&self) -> bool {
        self.current >= self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_codes() {
        assert_eq!(Color::Red.code(), "\x1b[31m");
        assert_eq!(Color::Reset.code(), "\x1b[0m");
    }

    #[test]
    fn get_terminal_width_at_least_80() {
        assert!(get_terminal_width() >= 80);
    }

    #[test]
    fn get_terminal_height_at_least_24() {
        assert!(get_terminal_height() >= 24);
    }

    #[test]
    fn progress_bar_advance_and_set() {
        let mut pb = ProgressBar::new("test", 100);
        assert_eq!(pb.current, 0);
        pb.advance(10);
        assert_eq!(pb.current, 10);
        pb.set(50);
        assert_eq!(pb.current, 50);
        assert!(!pb.is_finished());
        pb.set(100);
        assert!(pb.is_finished());
    }
}
