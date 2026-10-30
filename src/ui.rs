use crate::{
    app::{AppState, Row, RowKind},
    models::{AgentProvider, AgentStatus, View},
    theme::Palette,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Paragraph, Row as TableRow, Table, TableState},
};

pub fn render(frame: &mut Frame, app: &AppState, palette: &Palette) {
    render_at(
        frame,
        app,
        palette,
        crate::tracker::unix_millis().unwrap_or_default(),
    );
}

fn format_duration(seconds: u64) -> String {
    [
        (seconds / 86400, "d"),
        (seconds / 3600 % 24, "h"),
        (seconds / 60 % 60, "m"),
        (seconds % 60, "s"),
    ]
    .into_iter()
    .filter(|(value, _)| *value > 0)
    .map(|(value, unit)| format!("{value}{unit}"))
    .collect::<Vec<_>>()
    .join(" ")
}

fn ellipsize(line: Line<'_>, width: usize, style: Style) -> Line<'_> {
    if width == 0 {
        return Line::default();
    }
    if line.width() <= width {
        return line;
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for grapheme in line.styled_graphemes(Style::default()) {
        let cells = Span::raw(grapheme.symbol).width();
        if used + cells > width - 1 {
            break;
        }
        spans.push(Span::styled(grapheme.symbol.to_owned(), grapheme.style));
        used += cells;
    }
    if used < width - 1 {
        spans.push(Span::raw(" ".repeat(width - 1 - used)));
    }
    spans.push(Span::styled("…", style));
    Line::from(spans)
}

fn icon(
    kind: RowKind,
    provider: Option<AgentProvider>,
    palette: &Palette,
) -> (&'static str, Color) {
    match kind {
        RowKind::Workspace => (" ", palette.blue),
        RowKind::Worktree => (" ", palette.mauve),
        RowKind::Tab => (" ", palette.yellow),
        RowKind::Pane => match provider {
            Some(AgentProvider::Codex) => ("֎ ", Color::Indexed(6)),
            Some(AgentProvider::Claude) => ("✳ ", palette.peach),
            Some(AgentProvider::Other) => ("◇ ", palette.teal),
            None => (" ", palette.text),
        },
    }
}

fn label(
    name: &str,
    matches: &[usize],
    style: Style,
    disabled: bool,
    palette: &Palette,
) -> Line<'static> {
    let mut offset = 0;
    let spans = Span::raw(name)
        .styled_graphemes(Style::default())
        .map(|grapheme| {
            let end = offset + grapheme.symbol.chars().count();
            let index = matches.partition_point(|&i| i < offset);
            let matched = matches.get(index).is_some_and(|&i| i < end);
            offset = end;
            Span::styled(
                grapheme.symbol.to_owned(),
                if matched && !disabled {
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    style
                },
            )
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn agent_name(row: &Row, width: usize, palette: &Palette) -> Line<'static> {
    let disabled = row.target.is_none();
    let style = Style::default().fg(if disabled {
        palette.muted
    } else {
        palette.text
    });
    let mut parts: Vec<_> = row
        .breadcrumbs
        .iter()
        .map(|p| (p.kind, p.name.as_str(), p.matches.as_slice()))
        .collect();
    parts.push((row.kind, &row.name, &row.matches));
    let budget = width.saturating_sub(1 + 2 * parts.len() + 3 * (parts.len() - 1));
    let mut lengths: Vec<_> = parts
        .iter()
        .map(|(_, name, _)| Span::raw(*name).width().min(budget))
        .collect();
    // Shorten the longest names first, retaining each breadcrumb and the agent icon.
    while lengths.iter().sum::<usize>() > budget {
        let i = (0..lengths.len())
            .max_by_key(|&i| (lengths[i], std::cmp::Reverse(i)))
            .unwrap();
        lengths[i] -= 1;
    }
    let mut spans = vec![Span::raw(" ")];
    for (i, ((kind, name, matches), length)) in parts.into_iter().zip(lengths).enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(palette.muted)));
        }
        let (glyph, color) = icon(kind, row.provider, palette);
        spans.push(Span::styled(
            glyph,
            Style::default().fg(if disabled { palette.muted } else { color }),
        ));
        spans.extend(
            ellipsize(
                label(name, matches, style, disabled, palette),
                length,
                style,
            )
            .spans,
        );
    }
    Line::from(spans)
}

fn render_at(frame: &mut Frame, app: &AppState, palette: &Palette, now_ms: u64) {
    let area = frame.area();
    let style = Style::default().bg(palette.background).fg(palette.text);
    frame.render_widget(Block::default().style(style), area);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(app.error.is_some())),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(area);
    let mut prompt = vec![Span::styled(" > ", Style::default().fg(palette.accent))];
    if app.query.is_empty() {
        prompt.push(Span::styled(
            if app.view == View::Agents {
                "Search agents"
            } else {
                "Search workspaces, tabs, panes"
            },
            Style::default().fg(palette.muted),
        ));
    } else {
        prompt.push(Span::raw(&app.query));
    }
    if app.show_all {
        prompt.push(Span::styled("  [all]", Style::default().fg(palette.muted)));
    }
    frame.render_widget(Paragraph::new(Line::from(prompt)).style(style), chunks[0]);
    if let Some(error) = &app.error {
        frame.render_widget(
            Paragraph::new(error.as_str()).style(style.fg(palette.red)),
            chunks[1],
        );
    }
    frame.render_widget(
        Paragraph::new("─".repeat(area.width as usize)).style(style.fg(palette.muted)),
        chunks[2],
    );
    if app.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(" No matches").style(style.fg(palette.muted)),
            chunks[3],
        );
        return;
    }
    let elapsed: Vec<String> = app
        .rows
        .iter()
        .map(|row| {
            if row.kind == RowKind::Pane
                && row.provider.is_some_and(AgentProvider::supports_timing)
                && row.agents == [AgentStatus::Working]
            {
                row.working_since_ms
                    .and_then(|start| now_ms.checked_sub(start))
                    .map(|ms| format_duration(ms / 1000))
                    .unwrap_or_default()
            } else {
                String::new()
            }
        })
        .collect();
    // Keep complete timers with their bubble, leaving at least one name cell.
    let time_width = elapsed
        .iter()
        .filter(|time| !time.is_empty())
        .map(|time| time.len() + 4)
        .filter(|&width| width < area.width as usize)
        .max()
        .unwrap_or(0);
    let status_width = app
        .rows
        .iter()
        .map(|row| row.agents.len().saturating_mul(2))
        .max()
        .unwrap_or(0)
        .max(time_width)
        .min((area.width as usize).saturating_sub(8).max(time_width)) as u16;
    let mut widths = vec![Constraint::Fill(1)];
    if status_width > 0 {
        widths.push(Constraint::Length(status_width));
    }
    let spacing = if status_width > 0 {
        2.min(area.width.saturating_sub(status_width + 1))
    } else {
        0
    };
    let name_width = area.width.saturating_sub(status_width + spacing);
    let mut rows = Vec::new();
    let mut selected = app.selected;
    for (i, (row, elapsed)) in app.rows.iter().zip(&elapsed).enumerate() {
        if i > 0 && row.kind == RowKind::Workspace && row.depth == 0 {
            rows.push(TableRow::new([Cell::from("─".repeat(area.width as usize))
                .style(Style::default().fg(palette.muted))
                .column_span(widths.len() as u16)]));
            if i <= app.selected {
                selected += 1;
            }
        }
        let current_pane = row.kind == RowKind::Pane && row.target.is_none();
        let label_style = if current_pane {
            Style::default().fg(palette.muted)
        } else if row.depth == 0 && app.view == View::Tree {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let name = if app.view == View::Agents {
            agent_name(row, name_width as usize, palette)
        } else {
            let mut name = vec![Span::styled(
                format!(" {}", row.prefix),
                Style::default().fg(palette.muted),
            )];
            let (glyph, color) = icon(row.kind, row.provider, palette);
            name.push(Span::styled(
                glyph,
                Style::default().fg(if current_pane { palette.muted } else { color }),
            ));
            name.extend(label(&row.name, &row.matches, label_style, current_pane, palette).spans);
            if row.has_children && !row.expanded {
                name.push(Span::styled(" [+]", Style::default().fg(palette.muted)));
            }
            Line::from(name)
        };
        let mut status = Vec::new();
        if !elapsed.is_empty() && elapsed.len() + 4 <= status_width as usize {
            status.push(Span::styled(
                elapsed.as_str(),
                Style::default().fg(if current_pane {
                    palette.muted
                } else {
                    palette.subtext
                }),
            ));
            status.push(Span::raw("  "));
        }
        status.extend(row.agents.iter().map(|status| {
            let color = match status {
                AgentStatus::Blocked => palette.red,
                AgentStatus::Working => palette.yellow,
                AgentStatus::Done => palette.blue,
                AgentStatus::Idle => palette.green,
                AgentStatus::Unknown => palette.muted,
            };
            Span::styled(
                if *status == AgentStatus::Idle {
                    " "
                } else {
                    "● "
                },
                Style::default().fg(color),
            )
        }));
        let mut cells = vec![Cell::from(ellipsize(
            name,
            name_width as usize,
            label_style,
        ))];
        if status_width > 0 {
            cells.push(Cell::from(Line::from(status).right_aligned()));
        }
        rows.push(TableRow::new(cells));
    }
    let table = Table::new(rows, widths)
        .column_spacing(spacing)
        .style(style)
        .row_highlight_style(Style::default().bg(palette.selection));
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, chunks[3], &mut state);
}

#[cfg(test)]
mod tree_tests {
    use super::*;
    use crate::{
        app::AppState,
        models::{ActiveLocation, History, Keybindings, View},
        test_helpers::snapshot,
        theme::Palette,
    };
    use ratatui::style::Color;
    use ratatui::{Terminal, backend::TestBackend};

    fn agents_screen(app: &AppState, width: u16, height: u16, now: u64) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_at(frame, app, &Palette::default(), now))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn line(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
    }

    #[test]
    fn agent_rows_render_workspace_optional_worktree_provider_and_status_without_tabs_or_tree() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].working_since_ms = Some(1000);
        let app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Agents,
        );
        let buffer = agents_screen(&app, 65, 30, 6000);
        assert!(line(&buffer, 0).contains("Search agents"));
        assert_eq!(
            line(&buffer, 2).trim_end(),
            "  Project ·  feature · ֎ reviewer                            ●"
        );
        assert_eq!(
            line(&buffer, 3).trim_end(),
            "  Project · ✳ notes                                       5s  ●"
        );
        assert_eq!(
            line(&buffer, 4).trim_end(),
            "  Project · ✳ hidden-title                                    "
        );
        for y in 2..30 {
            let text = line(&buffer, y);
            assert!(!text.contains(['│', '├', '└', '─']));
            assert!(
                !text.contains("Editor") && !text.contains("Review") && !text.contains("[all]")
            );
        }
        let palette = Palette::default();
        for (symbol, expected) in [
            ("", palette.blue),
            ("", palette.mauve),
            ("֎", Color::Indexed(6)),
        ] {
            let x = (0..65)
                .find(|&x| buffer[(x, 2)].symbol() == symbol)
                .unwrap();
            assert_eq!(buffer[(x, 2)].fg, expected);
        }
        let x = (0..65).find(|&x| buffer[(x, 3)].symbol() == "5").unwrap();
        assert_eq!(buffer[(x, 3)].fg, palette.subtext);
        assert_eq!(buffer[(63, 2)].fg, palette.blue);
        assert_eq!(buffer[(63, 3)].fg, palette.yellow);
        assert_eq!(buffer[(63, 4)].fg, palette.green);
    }

    #[test]
    fn current_agent_breadcrumbs_icons_matches_and_time_are_gray_except_the_bubble() {
        let mut data = snapshot();
        data.workspaces[1].tabs[0].panes[0].status = AgentStatus::Working;
        data.workspaces[1].tabs[0].panes[0].working_since_ms = Some(1000);
        let current = ActiveLocation {
            workspace_id: Some("worktree".into()),
            tab_id: Some("review".into()),
            pane_id: Some("named-review".into()),
        };
        let mut app = AppState::new(
            data,
            &History::default(),
            current,
            Keybindings::default(),
            View::Agents,
        );
        app.set_query("Project");
        let buffer = agents_screen(&app, 65, 30, 6000);
        let y = (2 + app
            .rows
            .iter()
            .position(|r| r.key == "pane:named-review")
            .unwrap()) as u16;
        assert!(line(&buffer, y).contains("Project ·  feature · ֎ reviewer"));
        for x in 0..63 {
            if buffer[(x, y)].symbol() != " " {
                assert_eq!(buffer[(x, y)].fg, Palette::default().muted);
            }
        }
        assert_eq!(buffer[(63, y)].fg, Palette::default().yellow);
        assert!(
            app.rows
                .iter()
                .find(|r| r.key == "pane:named-review")
                .unwrap()
                .target
                .is_none()
        );
    }

    #[test]
    fn agent_breadcrumb_search_highlights_workspace_worktree_and_pane_names() {
        let mut app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Agents,
        );
        for (query, matched) in [
            ("Project", "Project"),
            ("feature", "feature"),
            ("reviewer", "reviewer"),
        ] {
            app.set_query(query);
            let buffer = agents_screen(&app, 65, 30, 0);
            let text = line(&buffer, 2);
            let start = text.chars().collect::<String>().find(matched).unwrap();
            let x = text[..start].chars().count() as u16;
            for i in 0..matched.len() as u16 {
                assert_eq!(buffer[(x + i, 2)].fg, Palette::default().accent, "{query}");
                assert!(buffer[(x + i, 2)].modifier.contains(Modifier::BOLD));
            }
        }
    }

    #[test]
    fn long_agent_paths_ellipsize_names_individually_and_reserve_whole_timers() {
        let mut data = snapshot();
        data.workspaces[0].name = "Workspace-long-name".repeat(20);
        data.workspaces[1].name = "分支🦀long-name".repeat(20);
        data.workspaces[1].tabs[0].panes[0].name = Some("agent-long-name".repeat(20));
        data.workspaces[1].tabs[0].panes[0].status = AgentStatus::Working;
        data.workspaces[1].tabs[0].panes[0].working_since_ms = Some(1000);
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Agents,
        );
        app.set_query("分支");
        let buffer = agents_screen(&app, 65, 30, 93_785_000);
        let text = line(&buffer, 2);
        assert!(text.contains('') && text.contains('') && text.contains('֎'));
        assert_eq!(text.matches('…').count(), 3);
        assert!(text.ends_with("1d 2h 3m 4s  ● "));
        for width in 0..22 {
            let _ = agents_screen(&app, width, 6, 93_785_000);
        }
        let empty = AppState::new(
            crate::models::Snapshot::default(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Agents,
        );
        assert!(line(&agents_screen(&empty, 65, 10, 0), 2).contains("No matches"));
    }

    fn tree_y(app: &AppState, key: &str) -> u16 {
        let index = app.rows.iter().position(|row| row.key == key).unwrap();
        let dividers = app.rows[..=index]
            .iter()
            .skip(1)
            .filter(|row| row.kind == RowKind::Workspace && row.depth == 0)
            .count();
        (2 + index + dividers) as u16
    }

    #[test]
    fn search_and_workspace_rules_use_guide_gray_without_dividing_worktrees() {
        for (query, workspace_rules) in [("", 1), ("reviewer", 0)] {
            let (_, buffer) = screen(55, 20, query);
            for x in 0..55 {
                assert_eq!(buffer[(x, 1)].symbol(), "─");
                assert_eq!(buffer[(x, 1)].fg, Palette::default().muted);
            }
            let rules: Vec<_> = (2..20)
                .filter(|&y| buffer[(0, y)].symbol() == "─")
                .collect();
            assert_eq!(rules.len(), workspace_rules);
            if query.is_empty() {
                assert_eq!(rules, vec![10]);
                for x in 0..55 {
                    assert_eq!(buffer[(x, 10)].symbol(), "─");
                    assert_eq!(buffer[(x, 10)].fg, Palette::default().muted);
                }
                assert_eq!(buffer[(4, 3)].symbol(), "");
                assert_eq!(buffer[(1, 11)].symbol(), "");
            }
            for y in 2..20 {
                assert_eq!(
                    buffer[(47, y)].symbol(),
                    if query.is_empty() && y == 10 {
                        "─"
                    } else {
                        " "
                    }
                );
            }
            if query.is_empty() {
                assert_eq!(buffer[(1, 4)].symbol(), "│");
            }
        }
    }

    #[test]
    fn time_precedes_its_bubble_in_the_same_right_aligned_status_cell() {
        let now = 100_000_000;
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].working_since_ms = Some(now - 5000);
        data.workspaces[1].tabs[0].panes[0].status = AgentStatus::Working;
        data.workspaces[1].tabs[0].panes[0].working_since_ms = Some(now - 93_784_000);
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        app.toggle_all();
        let palette =
            Palette::from_config("[theme.custom]\noverlay0 = \"#888888\"\nsubtext0 = \"#a9b1d6\"");
        let mut terminal = Terminal::new(TestBackend::new(65, 30)).unwrap();
        terminal
            .draw(|frame| render_at(frame, &app, &palette, now))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let notes_y = tree_y(&app, "pane:named-notes");
        let review_y = tree_y(&app, "pane:named-review");
        let name = |y| (0..48).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        let status = |y| {
            (50..65)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        };
        assert_eq!(name(notes_y).trim_end(), " │  ├─ ✳ notes");
        assert_eq!(name(review_y).trim_end(), " │     └─ ֎ reviewer");
        assert_eq!(status(notes_y).trim_start(), "5s  ● ");
        assert_eq!(status(review_y), "1d 2h 3m 4s  ● ");
        assert_eq!(buffer[(59, notes_y)].symbol(), "5");
        assert_eq!(buffer[(59, notes_y)].fg, Color::Rgb(169, 177, 214));
        assert_eq!(buffer[(9, notes_y)].fg, palette.text);
        for y in [notes_y, review_y] {
            assert_eq!(buffer[(63, y)].symbol(), "●");
            assert_eq!(buffer[(63, y)].fg, palette.yellow);
        }
        assert_eq!(status(2).trim_start(), "● ●  ");
        assert!((0..65).all(|x| buffer[(x, 1)].symbol() == "─"));
        assert!((2..30).all(|y| buffer[(48, y)].symbol() != "│"));
    }

    #[test]
    fn full_time_is_preserved_when_a_workspace_has_many_agent_bubbles() {
        let now = 100_000_000;
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].name = Some("long-title".repeat(30));
        data.workspaces[0].tabs[0].panes[0].working_since_ms = Some(now - 93_784_000);
        let extra = data.workspaces[0].tabs[0].panes[1].clone();
        for i in 0..40 {
            let mut pane = extra.clone();
            pane.id = format!("extra-{i}");
            data.workspaces[0].tabs[0].panes.push(pane);
        }
        let app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|frame| render_at(frame, &app, &Palette::default(), now))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let y = tree_y(&app, "pane:named-notes");
        assert_eq!(buffer[(5, y)].symbol(), "…");
        let status: String = (45..60).map(|x| buffer[(x, y)].symbol()).collect();
        assert_eq!(status, "1d 2h 3m 4s  ● ");
    }

    #[test]
    fn narrow_viewports_show_a_complete_time_or_omit_it_rather_than_cutting_it() {
        let now = 100_000_000;
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].working_since_ms = Some(now - 93_784_000);
        let app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        for width in [1, 3, 8, 11, 12, 14, 15, 16, 17, 18, 19, 20] {
            let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
            terminal
                .draw(|frame| render_at(frame, &app, &Palette::default(), now))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let y = tree_y(&app, "pane:named-notes");
            let line: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
            if width >= 16 {
                assert!(line.ends_with("1d 2h 3m 4s  ● "), "{width}: {line}");
                let first = (0..width)
                    .find(|&x| buffer[(x, y)].symbol() == "1")
                    .unwrap();
                assert_eq!(buffer[(first, y)].fg, Color::Gray);
            } else {
                assert!(!line.chars().any(|c| c.is_ascii_digit()), "{width}: {line}");
            }
        }
    }

    #[test]
    fn separators_do_not_steal_selection_when_scrolling_to_a_workspace() {
        let mut app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        app.toggle_all();
        app.selected = app.rows.iter().position(|row| row.name == "Other").unwrap();
        let target = app.selected_target();
        let palette = Palette::default();
        let mut terminal = Terminal::new(TestBackend::new(55, 4)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, &palette))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 2)].symbol(), "─");
        assert_eq!(buffer[(0, 2)].bg, palette.background);
        assert_eq!(buffer[(1, 3)].symbol(), "");
        assert_eq!(buffer[(3, 3)].symbol(), "O");
        assert_eq!(buffer[(3, 3)].bg, palette.selection);
        assert_eq!(app.selected_target(), target);
        assert_eq!(
            app.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE
            )),
            crate::models::KeyAction::Select
        );
    }

    #[test]
    fn green_status_uses_the_open_marker_and_all_status_colors_are_preserved() {
        let palette = Palette::default();
        for (status, symbol, color) in [
            (AgentStatus::Idle, "", palette.green),
            (AgentStatus::Done, "●", palette.blue),
            (AgentStatus::Working, "●", palette.yellow),
            (AgentStatus::Blocked, "●", palette.red),
            (AgentStatus::Unknown, "●", palette.muted),
        ] {
            let mut data = snapshot();
            data.workspaces[0].tabs[0].panes[0].status = status;
            let app = AppState::new(
                data,
                &History::default(),
                ActiveLocation::default(),
                Keybindings::default(),
                View::Tree,
            );
            let mut terminal = Terminal::new(TestBackend::new(55, 20)).unwrap();
            terminal
                .draw(|frame| render(frame, &app, &palette))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let y = tree_y(&app, "pane:named-notes");
            assert_eq!(buffer[(53, y)].symbol(), symbol);
            assert_eq!(buffer[(53, y)].fg, color);
            let green: Vec<_> = (49..55)
                .map(|x| &buffer[(x, 2)])
                .filter(|cell| cell.fg == palette.green && !cell.symbol().trim().is_empty())
                .collect();
            assert!(!green.is_empty());
            assert!(green.iter().all(|cell| cell.symbol() == ""));
        }
    }

    #[test]
    fn empty_and_error_states_keep_the_search_rule_and_empty_status_columns_are_hidden() {
        let mut data = snapshot();
        for pane in data
            .workspaces
            .iter_mut()
            .flat_map(|w| &mut w.tabs)
            .flat_map(|t| &mut t.panes)
        {
            pane.agent = None;
        }
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        let palette = Palette::default();
        let mut terminal = Terminal::new(TestBackend::new(55, 10)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, &palette))
            .unwrap();
        assert!((0..55).all(|x| terminal.backend().buffer()[(x, 1)].symbol() == "─"));
        assert_eq!(terminal.backend().buffer()[(53, 2)].symbol(), " ");
        app.set_query("zzzzzz");
        app.error = Some("Refresh failed".into());
        terminal
            .draw(|frame| render(frame, &app, &palette))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 1)].symbol(), "R");
        assert!((0..55).all(|x| buffer[(x, 2)].symbol() == "─"));
        assert_eq!(buffer[(1, 3)].symbol(), "N");
    }

    #[test]
    fn ellipsis_uses_display_cells_and_never_splits_a_grapheme() {
        for (text, width, expected) in [
            ("abcd", 4, "abcd"),
            ("abcde", 4, "abc…"),
            ("abc", 0, ""),
            ("abc", 1, "…"),
            ("a界界", 4, "a界…"),
            ("界界", 2, " …"),
            ("e\u{301}xyz", 3, "e\u{301}x…"),
            ("👩‍💻abc", 4, "👩‍💻a…"),
        ] {
            let line = ellipsize(Line::from(text), width, Style::default());
            let result: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert_eq!(result, expected);
            assert!(line.width() <= width);
        }
    }

    #[test]
    fn every_row_kind_marks_truncation_before_timer_and_status_columns() {
        let now = 100_000_000;
        for with_timer in [false, true] {
            let mut data = snapshot();
            data.workspaces[0].name = "w".repeat(80);
            data.workspaces[0].tabs[0].name = "t".repeat(80);
            data.workspaces[0].tabs[0].panes[0].name = Some("p".repeat(80));
            data.workspaces[1].name = "g".repeat(80);
            if with_timer {
                data.workspaces[0].tabs[0].panes[0].working_since_ms = Some(now - 93_784_000);
            }
            let mut app = AppState::new(
                data,
                &History::default(),
                ActiveLocation::default(),
                Keybindings::default(),
                View::Tree,
            );
            app.toggle_all();
            let mut terminal = Terminal::new(TestBackend::new(55, 20)).unwrap();
            terminal
                .draw(|frame| render_at(frame, &app, &Palette::default(), now))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let notes_y = tree_y(&app, "pane:named-notes");
            for kind in [
                RowKind::Workspace,
                RowKind::Worktree,
                RowKind::Tab,
                RowKind::Pane,
            ] {
                let row = app
                    .rows
                    .iter()
                    .find(|row| row.kind == kind && row.name.len() == 80)
                    .unwrap();
                let y = tree_y(&app, &row.key);
                let last = if with_timer { 37 } else { 46 };
                assert_eq!(buffer[(last, y)].symbol(), "…", "{kind:?}");
            }
            assert_eq!(buffer[(53, notes_y)].symbol(), "●");
            if with_timer {
                let status: String = (40..55).map(|x| buffer[(x, notes_y)].symbol()).collect();
                assert_eq!(status, "1d 2h 3m 4s  ● ");
            }
        }
    }

    #[test]
    fn names_that_fit_exactly_do_not_get_an_ellipsis() {
        for (length, expected_last) in [(43, " "), (44, "w"), (45, "…")] {
            let mut data = snapshot();
            data.workspaces[0].name = "w".repeat(length);
            let app = AppState::new(
                data,
                &History::default(),
                ActiveLocation::default(),
                Keybindings::default(),
                View::Tree,
            );
            let mut terminal = Terminal::new(TestBackend::new(55, 20)).unwrap();
            terminal
                .draw(|frame| render(frame, &app, &Palette::default()))
                .unwrap();
            assert_eq!(terminal.backend().buffer()[(46, 2)].symbol(), expected_last);
        }
    }

    #[test]
    fn current_agent_icon_name_search_matches_and_timer_are_gray_but_its_bubble_keeps_its_color() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].working_since_ms = Some(10_000);
        data.workspaces[0].tabs[0].panes[0].name = Some("notes ".repeat(20));
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation {
                workspace_id: Some("project".into()),
                tab_id: Some("editor".into()),
                pane_id: Some("named-notes".into()),
            },
            Keybindings::default(),
            View::Tree,
        );
        app.set_query("notes");
        let palette =
            Palette::from_config("[theme.custom]\noverlay0 = \"#888888\"\nyellow = \"#ffcc00\"");
        let mut terminal = Terminal::new(TestBackend::new(55, 20)).unwrap();
        terminal
            .draw(|frame| render_at(frame, &app, &palette, 15_000))
            .unwrap();
        let y = app
            .rows
            .iter()
            .position(|r| r.name.starts_with("notes"))
            .unwrap() as u16
            + 2;
        let buffer = terminal.backend().buffer();
        let mut saw_timer = false;
        for x in 0..55 {
            let cell = &buffer[(x, y)];
            if cell.symbol() == "●" {
                assert_eq!(cell.fg, palette.yellow);
            } else if !cell.symbol().trim().is_empty() {
                assert_eq!(cell.fg, palette.muted, "{}", cell.symbol());
            }
            saw_timer |= cell.symbol() == "5";
        }
        assert!(saw_timer);
        assert_eq!(buffer[(46, y)].symbol(), "…");
        assert_eq!(buffer[(49, y)].symbol(), "5");
        assert_eq!(buffer[(53, y)].symbol(), "●");
    }

    #[test]
    fn working_duration_omits_zero_units_and_padding() {
        for (seconds, expected) in [
            (0, ""),
            (5, "5s"),
            (60, "1m"),
            (65, "1m 5s"),
            (3600, "1h"),
            (3605, "1h 5s"),
            (86400, "1d"),
            (93784, "1d 2h 3m 4s"),
            (8640005, "100d 5s"),
        ] {
            assert_eq!(format_duration(seconds), expected);
        }
    }

    #[test]
    fn status_times_remain_whole_while_names_shorten_and_tick_without_a_snapshot_refresh() {
        let now = 100_000_000;
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].working_since_ms = Some(now - 5000);
        data.workspaces[0].tabs[0].panes[0].name = Some("notes 模型 café ".repeat(30));
        data.workspaces[1].tabs[0].panes[0].status = AgentStatus::Working;
        data.workspaces[1].tabs[0].panes[0].working_since_ms = Some(now - 93_784_000);
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        app.toggle_all();
        let notes_y = tree_y(&app, "pane:named-notes");
        let review_y = tree_y(&app, "pane:named-review");
        let mut terminal = Terminal::new(TestBackend::new(55, 20)).unwrap();
        for (time, short, long) in [
            (now, "5s", "1d 2h 3m 4s"),
            (now + 1000, "6s", "1d 2h 3m 5s"),
        ] {
            terminal
                .draw(|frame| render_at(frame, &app, &Palette::default(), time))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let name = |y| (0..38).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            let status = |y| {
                (40..55)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            };
            assert!(name(notes_y).ends_with('…'));
            assert!(name(review_y).trim_end().ends_with("reviewer"));
            assert_eq!(status(notes_y).trim_start(), format!("{short}  ● "));
            assert_eq!(status(review_y), format!("{long}  ● "));
            for y in (2..12).filter(|&y| y != notes_y && y != review_y) {
                assert!(!status(y).chars().any(|c| c.is_ascii_digit()));
            }
            assert_eq!(buffer[(53, notes_y)].symbol(), "●");
            assert_eq!(buffer[(53, review_y)].symbol(), "●");
        }
        app.set_query("notes");
        terminal
            .draw(|frame| render_at(frame, &app, &Palette::default(), now))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("5s"));
        for width in [1, 3, 8, 20] {
            let mut terminal = Terminal::new(TestBackend::new(width, 4)).unwrap();
            terminal
                .draw(|frame| render_at(frame, &app, &Palette::default(), now))
                .unwrap();
        }
    }

    #[test]
    fn timers_hide_unknown_zero_future_unsupported_and_nonworking_durations() {
        let now = 10_000;
        for (agent, status, start) in [
            ("codex", AgentStatus::Working, None),
            ("claude", AgentStatus::Working, Some(now)),
            ("codex", AgentStatus::Working, Some(now + 1000)),
            ("other", AgentStatus::Working, Some(1)),
            ("", AgentStatus::Working, Some(1)),
            ("claude", AgentStatus::Idle, Some(1)),
            ("claude", AgentStatus::Done, Some(1)),
            ("codex", AgentStatus::Blocked, Some(1)),
            ("codex", AgentStatus::Unknown, Some(1)),
        ] {
            let mut data = snapshot();
            let pane = &mut data.workspaces[0].tabs[0].panes[0];
            pane.agent = Some(agent.into());
            pane.status = status;
            pane.working_since_ms = start;
            let mut app = AppState::new(
                data,
                &History::default(),
                ActiveLocation::default(),
                Keybindings::default(),
                View::Tree,
            );
            app.toggle_all();
            let mut terminal = Terminal::new(TestBackend::new(55, 20)).unwrap();
            terminal
                .draw(|frame| render_at(frame, &app, &Palette::default(), now))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(
                !text.chars().any(|c| c.is_ascii_digit()),
                "{agent:?} {status:?}: {text}"
            );
        }
    }

    #[test]
    fn agent_icons_replace_the_pane_icon_and_leave_no_provider_column() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].name = Some("n".repeat(60));
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        app.toggle_all();
        let mut terminal = Terminal::new(TestBackend::new(45, 20)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, &Palette::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let notes_y = tree_y(&app, "pane:named-notes");
        let review_y = tree_y(&app, "pane:named-review");
        assert_eq!(buffer[(7, notes_y)].symbol(), "✳");
        assert_eq!(buffer[(10, review_y)].symbol(), "֎");
        assert!((9..36).all(|x| buffer[(x, notes_y)].symbol() == "n"));
        assert_eq!(buffer[(36, notes_y)].symbol(), "…");
        for y in [notes_y, review_y] {
            assert_eq!(buffer[(43, y)].symbol(), "●");
        }
        assert_eq!(buffer[(1, 2)].symbol(), "");
        assert_eq!(buffer[(4, 6)].symbol(), "");
        assert_eq!(buffer[(4, 3)].symbol(), "");
    }

    #[test]
    fn pane_provider_icons_distinguish_known_other_and_absent_agents() {
        for (agent, expected, color) in [
            (Some("codex"), "֎", Color::Indexed(6)),
            (Some("claude"), "✳", Color::LightRed),
            (Some(" CODEX "), "֎", Color::Indexed(6)),
            (Some("Claude"), "✳", Color::LightRed),
            (Some("other"), "◇", Color::Cyan),
            (None, "", Color::Reset),
            (Some(" \t"), "", Color::Reset),
        ] {
            let mut data = snapshot();
            data.workspaces[0].tabs[0].panes[0].agent = agent.map(str::to_owned);
            let mut app = AppState::new(
                data,
                &History::default(),
                ActiveLocation::default(),
                Keybindings::default(),
                View::Tree,
            );
            app.toggle_all();
            let mut terminal = Terminal::new(TestBackend::new(64, 20)).unwrap();
            terminal
                .draw(|frame| render(frame, &app, &Palette::default()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let y = tree_y(&app, "pane:named-notes");
            let icons: Vec<_> = (0..64)
                .map(|x| buffer[(x, y)].symbol())
                .filter(|symbol| ["֎", "✳", "◇", ""].contains(symbol))
                .collect();
            assert_eq!(icons, vec![expected], "{agent:?}");
            assert_eq!(buffer[(7, y)].symbol(), expected);
            assert_eq!(buffer[(7, y)].fg, color);
            assert_eq!(buffer[(62, y)].symbol() == "●", expected != "");
        }
    }

    #[test]
    fn icon_colors_follow_custom_palette_without_recoloring_names_connectors_or_bubbles() {
        let palette = Palette::from_config(
            r##"[theme.custom]
blue = "#112233"
mauve = "#223344"
yellow = "#334455"
green = "#445566"
peach = "#556677"
teal = "#667788"
text = "#778899"
overlay0 = "#8899aa"
"##,
        );
        for (agent, icon, color) in [
            (Some("codex"), "֎", Color::Indexed(6)),
            (Some("claude"), "✳", Color::Rgb(85, 102, 119)),
            (Some("other"), "◇", Color::Rgb(102, 119, 136)),
            (None, "", Color::Rgb(119, 136, 153)),
        ] {
            let mut data = snapshot();
            data.workspaces[0].tabs[0].panes[0].agent = agent.map(str::to_owned);
            let mut app = AppState::new(
                data,
                &History::default(),
                ActiveLocation::default(),
                Keybindings::default(),
                View::Tree,
            );
            app.toggle_all();
            let mut terminal = Terminal::new(TestBackend::new(50, 20)).unwrap();
            terminal
                .draw(|frame| render(frame, &app, &palette))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let notes_y = tree_y(&app, "pane:named-notes");
            let review_y = tree_y(&app, "pane:named-review");
            let editor_y = tree_y(&app, "tab:editor");
            let feature_y = tree_y(&app, "workspace:worktree");
            for (x, y, glyph, expected) in [
                (1, 2, "", Color::Rgb(17, 34, 51)),
                (4, editor_y, "", Color::Rgb(51, 68, 85)),
                (7, notes_y, icon, color),
                (4, feature_y, "", Color::Rgb(34, 51, 68)),
                (10, review_y, "֎", Color::Indexed(6)),
            ] {
                assert_eq!(buffer[(x, y)].symbol(), glyph);
                assert_eq!(buffer[(x, y)].fg, expected, "{glyph}");
            }
            assert_eq!(buffer[(1, editor_y)].fg, palette.muted);
            assert_eq!(buffer[(3, 2)].fg, palette.text);
            assert_eq!(buffer[(9, notes_y)].fg, palette.text);
            assert_eq!(buffer[(48, review_y)].fg, palette.blue);
            if agent.is_some() {
                assert_eq!(buffer[(48, notes_y)].fg, palette.yellow);
            }
        }
    }

    #[test]
    fn provider_icons_survive_search_and_update_when_the_pane_agent_changes() {
        let mut data = snapshot();
        let mut app = AppState::new(
            data.clone(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        app.set_query("notes");
        let mut terminal = Terminal::new(TestBackend::new(64, 20)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, &Palette::default()))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(7, 4)].symbol(), "✳");

        data.workspaces[0].tabs[0].panes[0].agent = Some("codex".into());
        app.update_snapshot(data.clone(), &History::default());
        terminal
            .draw(|frame| render(frame, &app, &Palette::default()))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(7, 4)].symbol(), "֎");

        data.workspaces[0].tabs[0].panes[0].agent = None;
        app.update_snapshot(data, &History::default());
        terminal
            .draw(|frame| render(frame, &app, &Palette::default()))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(7, 4)].symbol(), "");
    }

    #[test]
    fn bubbles_are_right_aligned_in_a_separate_column_and_sorted_by_state() {
        let app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        let palette = Palette::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, &palette))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let dots: Vec<_> = (0..80)
            .filter(|&x| matches!(buffer[(x, 2)].symbol(), "●" | ""))
            .collect();
        assert_eq!(dots, vec![74, 76, 78]);
        assert_eq!(buffer[(74, 2)].fg, palette.yellow);
        assert_eq!(buffer[(76, 2)].fg, Color::Blue);
        assert_eq!(buffer[(78, 2)].fg, palette.green);
        assert_eq!(buffer[(78, 2)].symbol(), "");
        assert_eq!(buffer[(78, tree_y(&app, "tab:editor"))].symbol(), "");
        assert_eq!(buffer[(1, 2)].symbol(), "");
        assert_eq!(buffer[(3, 2)].symbol(), "P");
    }

    #[test]
    fn completed_agent_bubbles_use_custom_blue_on_panes_and_all_ancestor_rows() {
        let mut app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        app.toggle_all();
        let palette =
            Palette::from_config("[theme.custom]\nblue = \"#6495ed\"\nteal = \"#00ffff\"");
        let mut terminal = Terminal::new(TestBackend::new(50, 20)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, &palette))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for (x, y) in [
            (46, 2),
            (48, tree_y(&app, "workspace:worktree")),
            (48, tree_y(&app, "tab:review")),
            (48, tree_y(&app, "pane:named-review")),
        ] {
            assert_eq!(buffer[(x, y)].symbol(), "●");
            assert_eq!(buffer[(x, y)].fg, Color::Rgb(100, 149, 237));
        }
        assert_eq!(buffer[(44, 2)].fg, palette.yellow);
        assert_eq!(buffer[(48, 2)].fg, palette.green);
    }

    #[test]
    fn worktree_has_a_git_branch_glyph_without_changing_its_label() {
        let app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, &Palette::default()))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(4, tree_y(&app, "workspace:worktree"))].symbol(),
            ""
        );
        assert!(app.rows.iter().any(|r| r.name == "feature"));
    }

    #[test]
    fn each_row_kind_has_a_distinct_glyph_in_the_same_icon_position() {
        let (_, buffer) = screen(80, 15, "");
        assert_eq!(buffer[(1, 2)].symbol(), "");
        assert_eq!(buffer[(4, 6)].symbol(), "");
        assert_eq!(buffer[(7, 7)].symbol(), "✳");
        assert_eq!(buffer[(4, 3)].symbol(), "");
    }

    #[test]
    fn long_unicode_names_cannot_push_bubbles_out_of_their_column() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].name =
            Some(format!("模型 café {}", "世界 café ".repeat(30)));
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        app.toggle_all();
        app.set_query("模型");
        let mut terminal = Terminal::new(TestBackend::new(80, 15)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, &Palette::default()))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(78, 4)].symbol(), "●");
        assert_eq!(terminal.backend().buffer()[(7, 4)].symbol(), "✳");
        assert_eq!(
            terminal.backend().buffer()[(9, 4)].fg,
            Palette::default().accent
        );
        assert_eq!(terminal.backend().buffer()[(71, 4)].symbol(), "…");
        assert_eq!(
            terminal.backend().buffer()[(78, 4)].fg,
            Palette::default().yellow
        );
    }

    fn screen(width: u16, height: u16, query: &str) -> (String, ratatui::buffer::Buffer) {
        let mut app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        app.toggle_all();
        app.set_query(query);
        let palette = Palette::from_config("[theme.custom]\nsidebar_bg = \"#171b20\"");
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, &app, &palette)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text = buffer
            .content()
            .chunks(width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        (text, buffer)
    }

    #[test]
    fn renders_indented_tree_without_category_tabs_footer_or_row_numbers() {
        let (text, _) = screen(60, 15, "");
        assert!(text.contains("Project"));
        assert!(text.contains("├─ "));
        assert!(text.contains("reviewer"));
        for removed in [
            "Workspaces",
            "Agents",
            "Panes",
            "Ctrl+",
            "Enter",
            "Esc",
            "1.",
            "2.",
        ] {
            assert!(!text.contains(removed), "unexpected UI: {removed}\n{text}");
        }
    }

    #[test]
    fn paints_entire_view_with_sidebar_background() {
        let (_, buffer) = screen(60, 15, "");
        assert_eq!(buffer[(59, 14)].bg, Color::Rgb(23, 27, 32));
    }

    fn tree_lines(text: &str, count: usize) -> Vec<&str> {
        text.lines()
            .skip(2)
            .filter(|line| !line.starts_with('─'))
            .take(count)
            .map(|line| line.split(['●', '']).next().unwrap().trim_end())
            .collect()
    }

    #[test]
    fn branch_lines_join_at_the_same_columns_without_duplicate_markers() {
        let (text, _) = screen(60, 15, "");
        assert_eq!(
            tree_lines(&text, 9),
            vec![
                "  Project",
                " ├─  feature",
                " │  └─  Review",
                " │     └─ ֎ reviewer",
                " ├─  Editor",
                " │  ├─ ✳ notes",
                " │  └─ ✳ hidden-title",
                " └─  Tests",
                "  Other",
            ]
        );
    }

    #[test]
    fn search_reconnects_only_the_visible_branches() {
        let (text, _) = screen(60, 15, "reviewer");
        assert_eq!(
            tree_lines(&text, 4),
            vec![
                "  Project",
                " └─  feature",
                "    └─  Review",
                "       └─ ֎ reviewer",
            ]
        );
    }

    #[test]
    fn collapsed_indicator_follows_the_name_without_shifting_tree_columns() {
        let mut app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        app.selected = 0;
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ));
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, &Palette::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let line: String = (0..40).map(|x| buffer[(x, 2)].symbol()).collect();
        assert_eq!(
            line.split(['●', '']).next().unwrap().trim_end(),
            "  Project [+]"
        );
    }

    #[test]
    fn renders_no_matches_and_tiny_terminals_without_panicking() {
        assert!(screen(40, 8, "zzzzzz").0.contains("No matches"));
        for (w, h) in [(1, 1), (3, 2), (8, 2), (20, 4)] {
            for query in ["", "模型"] {
                screen(w, h, query);
            }
        }
    }

    #[test]
    fn scrolls_selection_into_view_and_clips_long_unicode_names() {
        let mut data = snapshot();
        data.workspaces[1].tabs[0].panes[0].name = Some("模型 café ".repeat(30));
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        );
        app.toggle_all();
        app.selected = app
            .rows
            .iter()
            .position(|r| r.name.starts_with("模型"))
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(25, 4)).unwrap();
        terminal
            .draw(|f| render(f, &app, &Palette::default()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        // Wide glyphs occupy two cells; the backing buffer includes a blank continuation cell.
        assert!(text.contains('模') && text.contains('型'));
    }
}
