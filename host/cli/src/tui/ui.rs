use pichainboot_proto::BootOutcome;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Gauge, Paragraph};

use super::app::App;
use crate::worker::Status;

const LOG_HEIGHT: u16 = 6;

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, status, console, log, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(LOG_HEIGHT),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);
    draw_status(frame, app, status);
    draw_console(frame, app, console);
    draw_log(frame, app, log);
    draw_help(frame, app, help);
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let auto = if app.auto { "on".green() } else { "off".dark_gray() };
    let line = Line::from(vec![
        " pichainboot ".bold().reversed(),
        format!(" {} │ {} @ {:#x} │ auto boot ", app.port, app.image, app.addr).into(),
        auto,
    ]);
    frame.render_widget(line, area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(error) = &app.fatal {
        frame.render_widget(Line::from(format!(" ✖ {error}")).red().bold(), area);
        return;
    }

    if let (Status::Loading, Some((sent, total))) = (&app.status, app.progress) {
        let label = format!(
            "loading {}% · {:.1} / {:.1} KiB",
            sent * 100 / total,
            sent as f64 / 1024.0,
            total as f64 / 1024.0
        );
        let gauge = Gauge::default()
            .ratio(sent as f64 / total as f64)
            .label(label)
            .gauge_style(Style::new().fg(Color::Cyan).bg(Color::DarkGray));
        frame.render_widget(gauge, area);
        return;
    }

    let (text, color) = match &app.status {
        Status::WaitingForDevice => ("waiting for the device, reset it to connect".to_string(), Color::Yellow),
        Status::Connected(info) => (format!("connected (protocol v{})", info.version), Color::Green),
        Status::Loading => ("loading".to_string(), Color::Cyan),
        Status::Booted(BootOutcome::Confirmed) => ("booted, console is live".to_string(), Color::Green),
        Status::Booted(BootOutcome::Unconfirmed) => ("booted (unconfirmed), console is live".to_string(), Color::Yellow),
    };
    let line = Line::from(vec![Span::styled(" ● ", Style::new().fg(color)), Span::raw(text)]);
    frame.render_widget(line, area);
}

fn draw_console(frame: &mut Frame, app: &App, area: Rect) {
    let mut block = Block::bordered().title(" Console ");
    if app.scroll > 0 {
        block = block.title_bottom(Line::from(format!(" scrolled up {} lines ", app.scroll)).right_aligned());
    }
    let inner = block.inner(area);

    let lines = app.console.lines();
    let end = lines.len().saturating_sub(app.scroll);
    let start = end.saturating_sub(inner.height as usize);
    let text: Vec<Line> = lines
        .range(start..end)
        .map(|line| Line::raw(String::from_utf8_lossy(line).into_owned()))
        .collect();

    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_log(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(" Log ");
    let height = block.inner(area).height as usize;
    let skip = app.log.len().saturating_sub(height);
    let text: Vec<Line> = app
        .log
        .iter()
        .skip(skip)
        .map(|entry| {
            let time = Span::styled(format!("{:7.1}s ", entry.elapsed_secs), Style::new().dark_gray());
            let style = if entry.error { Style::new().red() } else { Style::new() };
            Line::from(vec![time, Span::styled(entry.text.clone(), style)])
        })
        .collect();
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let reset = if app.can_reset { "r reset · " } else { "" };
    let text = if app.prefix {
        format!("Ctrl-A: b boot · {reset}a toggle auto boot · c clear console · q quit · Ctrl-A send Ctrl-A")
    } else if app.console_active() {
        let keys = if app.can_reset { "b/r/a/c/q" } else { "b/a/c/q" };
        format!("typing goes to the device · Ctrl-A then {keys} for commands · Shift-PgUp/PgDn scroll")
    } else {
        format!("b boot · {reset}a toggle auto boot · c clear console · q quit · PgUp/PgDn scroll")
    };
    frame.render_widget(
        Line::from(format!(" {text}")).style(Style::new().add_modifier(Modifier::DIM)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use pichainboot_proto::message::Capabilities;
    use pichainboot_proto::Info;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::worker::Event;

    fn render(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    #[test]
    fn shows_the_reset_key_when_available() {
        let mut app = App::new("tcpuart:pi".into(), "kernel8.img".into(), 0x80000, false);
        assert!(!render(&app).contains("r reset"));
        app.can_reset = true;
        assert!(render(&app).contains("b boot · r reset"));
    }

    #[test]
    fn renders_the_session_state() {
        let mut app = App::new("/dev/ttyUSB0".into(), "kernel8.img".into(), 0x80000, true);
        let screen = render(&app);
        assert!(screen.contains("/dev/ttyUSB0 │ kernel8.img @ 0x80000"));
        assert!(screen.contains("waiting for the device"));

        app.handle_worker_event(Event::Status(Status::Connected(Info {
            version: 1,
            max_payload: 4096,
            capabilities: Capabilities(1),
        })));
        app.handle_worker_event(Event::Log("connected".into()));
        app.handle_worker_event(Event::Status(Status::Loading));
        app.handle_worker_event(Event::Progress { sent: 2048, total: 4096 });
        let screen = render(&app);
        assert!(screen.contains("loading 50% · 2.0 / 4.0 KiB"));
        assert!(screen.contains("connected"));

        app.handle_worker_event(Event::Status(Status::Booted(BootOutcome::Confirmed)));
        app.handle_worker_event(Event::Console(b"Linux version 6.12\r\n$ ".to_vec()));
        let screen = render(&app);
        assert!(screen.contains("booted, console is live"));
        assert!(screen.contains("Linux version 6.12"));
        assert!(screen.contains("typing goes to the device"));
    }
}
