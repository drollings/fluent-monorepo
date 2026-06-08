use std::io::{self, BufRead, IsTerminal, Write};

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
    #[cfg(unix)]
    {
        use std::mem::MaybeUninit;
        use std::os::fd::AsRawFd;
        let mut ws: MaybeUninit<libc::winsize> = MaybeUninit::uninit();
        let ret = unsafe {
            libc::ioctl(io::stdout().as_raw_fd(), libc::TIOCGWINSZ, ws.as_mut_ptr())
        };
        if ret == 0 {
            let ws = unsafe { ws.assume_init() };
            if ws.ws_col > 0 {
                return ws.ws_col as usize;
            }
        }
    }
    80
}

pub fn get_terminal_height() -> usize {
    #[cfg(unix)]
    {
        use std::mem::MaybeUninit;
        use std::os::fd::AsRawFd;
        let mut ws: MaybeUninit<libc::winsize> = MaybeUninit::uninit();
        let ret = unsafe {
            libc::ioctl(io::stdout().as_raw_fd(), libc::TIOCGWINSZ, ws.as_mut_ptr())
        };
        if ret == 0 {
            let ws = unsafe { ws.assume_init() };
            if ws.ws_row > 0 {
                return ws.ws_row as usize;
            }
        }
    }
    24
}

pub fn is_terminal() -> bool {
    io::stdout().is_terminal()
}

pub fn confirm(question: &str, default: bool) -> io::Result<bool> {
    confirm_with(question, default, &mut io::stdin().lock(), &mut io::stdout())
}

pub fn confirm_with(
    question: &str,
    default: bool,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> io::Result<bool> {
    let prompt = if default {
        format!("{question} [Y/n]: ")
    } else {
        format!("{question} [y/N]: ")
    };
    write!(writer, "{prompt}")?;
    writer.flush()?;
    let mut input = String::new();
    reader.read_line(&mut input)?;
    let trimmed = input.trim().to_lowercase();
    match trimmed.as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Ok(default),
    }
}

pub fn ask(question: &str, default: &str) -> io::Result<String> {
    ask_with(question, default, &mut io::stdin().lock(), &mut io::stdout())
}

pub fn ask_with(
    question: &str,
    default: &str,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> io::Result<String> {
    let prompt = if default.is_empty() {
        format!("{question}: ")
    } else {
        format!("{question} [{default}]: ")
    };
    write!(writer, "{prompt}")?;
    writer.flush()?;
    let mut input = String::new();
    reader.read_line(&mut input)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

pub fn ask_int(question: &str, default: Option<i64>) -> io::Result<i64> {
    ask_int_with(question, default, &mut io::stdin().lock(), &mut io::stdout())
}

pub fn ask_int_with(
    question: &str,
    default: Option<i64>,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> io::Result<i64> {
    let prompt = if let Some(d) = default {
        format!("{question} [{d}]: ")
    } else {
        format!("{question}: ")
    };
    write!(writer, "{prompt}")?;
    writer.flush()?;
    let mut input = String::new();
    reader.read_line(&mut input)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        default.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no input and no default"))
    } else {
        trimmed.parse::<i64>().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid integer"))
    }
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

    #[test]
    fn all_color_codes() {
        assert_eq!(Color::Reset.code(), "\x1b[0m");
        assert_eq!(Color::Black.code(), "\x1b[30m");
        assert_eq!(Color::Red.code(), "\x1b[31m");
        assert_eq!(Color::Green.code(), "\x1b[32m");
        assert_eq!(Color::Yellow.code(), "\x1b[33m");
        assert_eq!(Color::Blue.code(), "\x1b[34m");
        assert_eq!(Color::Magenta.code(), "\x1b[35m");
        assert_eq!(Color::Cyan.code(), "\x1b[36m");
        assert_eq!(Color::White.code(), "\x1b[37m");
        assert_eq!(Color::BrightBlack.code(), "\x1b[90m");
        assert_eq!(Color::BrightRed.code(), "\x1b[91m");
        assert_eq!(Color::BrightGreen.code(), "\x1b[92m");
        assert_eq!(Color::BrightYellow.code(), "\x1b[93m");
        assert_eq!(Color::BrightBlue.code(), "\x1b[94m");
        assert_eq!(Color::BrightMagenta.code(), "\x1b[95m");
        assert_eq!(Color::BrightCyan.code(), "\x1b[96m");
        assert_eq!(Color::BrightWhite.code(), "\x1b[97m");
    }

    #[test]
    fn progress_bar_render_states() {
        let pb = ProgressBar::new("test", 100);
        let rendered = pb.render();
        assert!(rendered.starts_with("test ["));
        assert!(rendered.contains("0/100"));

        let pb = ProgressBar::new("test", 0);
        let rendered = pb.render();
        assert!(rendered.contains("0/0"));

        let mut pb = ProgressBar::new("test", 100);
        pb.set(100);
        let rendered = pb.render();
        assert!(rendered.contains("100/100"));
    }

    #[test]
    fn confirm_default_yes_with_empty_input() {
        let mut input = b"\n" as &[u8];
        let mut output = Vec::new();
        let result = confirm_with("Proceed?", true, &mut input, &mut output);
        assert!(result.unwrap());
        let out = String::from_utf8(output).unwrap();
        assert!(out.contains("Proceed?"));
        assert!(out.contains("[Y/n]"));
    }

    #[test]
    fn confirm_accepts_yes() {
        let mut input = b"y\n" as &[u8];
        let mut output = Vec::new();
        let result = confirm_with("Go?", false, &mut input, &mut output);
        assert!(result.unwrap());
    }

    #[test]
    fn confirm_accepts_no() {
        let mut input = b"n\n" as &[u8];
        let mut output = Vec::new();
        let result = confirm_with("Go?", true, &mut input, &mut output);
        assert!(!result.unwrap());
    }

    #[test]
    fn confirm_default_no_with_invalid_input() {
        let mut input = b"maybe\n" as &[u8];
        let mut output = Vec::new();
        let result = confirm_with("Go?", false, &mut input, &mut output);
        assert!(!result.unwrap());
    }

    #[test]
    fn confirm_default_yes_with_invalid_input() {
        let mut input = b"maybe\n" as &[u8];
        let mut output = Vec::new();
        let result = confirm_with("Go?", true, &mut input, &mut output);
        assert!(result.unwrap());
    }

    #[test]
    fn ask_returns_default_on_empty() {
        let mut input = b"\n" as &[u8];
        let mut output = Vec::new();
        let result = ask_with("Name", "default_val", &mut input, &mut output);
        assert_eq!(result.unwrap(), "default_val");
    }

    #[test]
    fn ask_returns_input() {
        let mut input = b"custom\n" as &[u8];
        let mut output = Vec::new();
        let result = ask_with("Name", "default_val", &mut input, &mut output);
        assert_eq!(result.unwrap(), "custom");
    }

    #[test]
    fn ask_no_default_shows_no_brackets() {
        let mut input = b"val\n" as &[u8];
        let mut output = Vec::new();
        let result = ask_with("Name", "", &mut input, &mut output);
        assert_eq!(result.unwrap(), "val");
        let out = String::from_utf8(output).unwrap();
        assert!(!out.contains('['));
    }

    #[test]
    fn ask_int_returns_default_on_empty() {
        let mut input = b"\n" as &[u8];
        let mut output = Vec::new();
        let result = ask_int_with("Count", Some(42), &mut input, &mut output);
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn ask_int_returns_input() {
        let mut input = b"99\n" as &[u8];
        let mut output = Vec::new();
        let result = ask_int_with("Count", Some(42), &mut input, &mut output);
        assert_eq!(result.unwrap(), 99);
    }

    #[test]
    fn ask_int_rejects_invalid() {
        let mut input = b"not_a_number\n" as &[u8];
        let mut output = Vec::new();
        let result = ask_int_with("Count", Some(42), &mut input, &mut output);
        assert!(result.is_err());
    }

    #[test]
    fn ask_int_empty_no_default_is_error() {
        let mut input = b"\n" as &[u8];
        let mut output = Vec::new();
        let result = ask_int_with("Count", None, &mut input, &mut output);
        assert!(result.is_err());
    }

    #[test]
    fn is_terminal_returns_bool() {
        let result = is_terminal();
        assert!(result == true || result == false);
    }

    #[test]
    fn progress_bar_set_clamps_at_total() {
        let mut pb = ProgressBar::new("test", 50);
        pb.set(100);
        assert_eq!(pb.current, 50);
        assert!(pb.is_finished());
    }

    #[test]
    fn progress_bar_advance_clamps_at_total() {
        let mut pb = ProgressBar::new("test", 50);
        pb.advance(100);
        assert_eq!(pb.current, 50);
    }
}
