//! Shared plain-text table renderer for the tabular admin screens (`ps`,
//! `ls`): aligned columns sized to the live terminal width, with no border
//! characters at all — the ratatui buffer is flattened to symbols only, so
//! the output is pipe-safe plain text. The `--markdown` path is the shared
//! `common_core::format::render_markdown_table` instead (pipe tables, never
//! truncated).

use ratatui::backend::TestBackend;
use ratatui::layout::{Alignment, Constraint};
use ratatui::text::Line;
use ratatui::widgets::{Cell, Row, Table};
use ratatui::Terminal;

/// Render aligned plain-text columns over `width` terminal columns.
/// `min_widths` fixes every column except `flex`, which absorbs the remaining
/// width (ratatui clips cells that still overflow on narrow terminals, so
/// every emitted line fits `width`); `right[i]` right-aligns column `i`.
/// Columns are separated by a single space — no borders, no title row.
/// Short rows are padded with empty cells; over-long rows are clipped to the
/// header count so one ragged row cannot shift the grid.
#[allow(clippy::cast_possible_truncation)]
pub fn render_text_table(
    headers: &[&str],
    rows: &[Vec<String>],
    right: &[bool],
    flex: usize,
    min_widths: &[usize],
    width: u16,
) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let ncols = headers.len();
    let flex = flex.min(ncols - 1);
    let width = width.clamp(60, 140);
    let chrome = (ncols.saturating_sub(1)) as u16;
    let capped = |w: usize| w.min(10_000) as u16;
    let fixed: u16 = min_widths
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != flex)
        .map(|(_, w)| capped(*w))
        .fold(0, u16::saturating_add);
    let flex_w = width
        .saturating_sub(chrome.saturating_add(fixed))
        .max(capped(min_widths.get(flex).copied().unwrap_or(0)));
    let widths: Vec<Constraint> = (0..ncols)
        .map(|i| {
            if i == flex {
                Constraint::Length(flex_w)
            } else {
                Constraint::Length(capped(min_widths.get(i).copied().unwrap_or(0)))
            }
        })
        .collect();
    let header = Row::new(headers.iter().copied());
    let body: Vec<Row> = rows
        .iter()
        .map(|cells| {
            Row::new((0..ncols).map(|i| {
                let text = cells.get(i).map_or("", String::as_str);
                let line = Line::from(text);
                if right.get(i).copied().unwrap_or(false) {
                    Cell::new(line.alignment(Alignment::Right))
                } else {
                    Cell::new(line)
                }
            }))
        })
        .collect();
    // 1 header row + N body rows, no borders.
    let height = (body.len() + 1).min(10_000) as u16;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend construction infallible");
    terminal
        .draw(|frame| {
            let table = Table::new(body, widths).header(header);
            frame.render_widget(table, frame.area());
        })
        .expect("test backend draw infallible");
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            line.push_str(buffer.cell((x, y)).map_or(" ", |c| c.symbol()));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Widest content width per column (header included): the caller passes the
/// result as `min_widths` so tables size to their data and only the `flex`
/// column ever clips on narrow terminals.
pub fn content_widths(headers: &[&str], rows: &[Vec<String>]) -> Vec<usize> {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.len());
            }
        }
    }
    widths
}

/// Live terminal width in columns: `crossterm` size, else `COLUMNS`, else a
/// 100-column default — clamped to a readable band.
pub(crate) fn terminal_width() -> u16 {
    if let Ok((w, _)) = ratatui::crossterm::terminal::size() {
        if w >= 60 {
            return w.min(140);
        }
    }
    if let Ok(cols) = std::env::var("COLUMNS") {
        if let Ok(w) = cols.parse::<u16>() {
            if w >= 60 {
                return w.min(140);
            }
        }
    }
    100
}
