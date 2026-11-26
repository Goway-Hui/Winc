//! Minimal debugger panel: execution controls, call stack, variables, and adapter output.

use winc_render::Rect;

use super::*;
use crate::app::debug::{DebugControl, DebugStatus};

const HEADER: f32 = 42.0;
const ROW: f32 = 22.0;
const BUTTON: f32 = 28.0;
const PAD: f32 = 10.0;

/// Draws the debugger in the terminal panel card instead of terminal tabs.
pub(super) fn draw_debug(app: &mut App, frame: &mut Frame<'_>) {
    let card = app.layout.terminal;
    if !app.debug_shown() || card.w <= 0.0 || card.h <= 0.0 {
        return;
    }
    app.hits.terminal.clear();
    app.debug.ui.control_hits.clear();
    app.debug.ui.stack_hits.clear();
    app.debug.ui.variable_hits.clear();
    let p = app.palette;
    frame.rect(card, p.editor_bg);
    let header = Rect::new(card.x, card.y, card.w, HEADER);
    frame.rect(header, p.sidebar_bg);
    frame.rect(
        Rect::new(card.x, header.bottom() - 1.0, card.w, 1.0),
        p.border,
    );
    draw_header(app, frame, header, card);
    let body = Rect::new(card.x, header.bottom(), card.w, (card.h - HEADER).max(0.0));
    if body.h <= 0.0 {
        return;
    }
    let left_w = (body.w * 0.48).clamp(220.0, 420.0).min(body.w * 0.7);
    let stack = Rect::new(body.x, body.y, left_w, body.h);
    let variables = Rect::new(stack.right() + 1.0, body.y, body.w - stack.w - 1.0, body.h);
    frame.rect(Rect::new(stack.right(), body.y, 1.0, body.h), p.border);
    draw_stack(app, frame, stack);
    draw_variables(app, frame, variables);
    draw_output(
        app,
        frame,
        Rect::new(body.x, body.bottom() - 90.0, body.w, 90.0),
        stack,
    );
}

fn draw_header(app: &mut App, frame: &mut Frame<'_>, header: Rect, card: Rect) {
    let p = app.palette;
    let active = app.debug_active();
    let stopped = app.debug.status == DebugStatus::Stopped;
    let controls = [
        (
            DebugControl::Start,
            "▶",
            !active,
            winc_i18n::t!("debug-start"),
        ),
        (DebugControl::Stop, "■", active, winc_i18n::t!("debug-stop")),
        (
            DebugControl::Continue,
            "➤",
            stopped,
            winc_i18n::t!("debug-continue"),
        ),
        (
            DebugControl::Next,
            "→",
            app.debug_can_step(),
            winc_i18n::t!("debug-next"),
        ),
        (
            DebugControl::StepIn,
            "↘",
            app.debug_can_step(),
            winc_i18n::t!("debug-step-in"),
        ),
        (
            DebugControl::StepOut,
            "↗",
            app.debug_can_step(),
            winc_i18n::t!("debug-step-out"),
        ),
    ];
    let mut x = header.x + 6.0;
    for (control, glyph, enabled, _title) in controls {
        if x + BUTTON > header.right() - 80.0 {
            break;
        }
        let rect = Rect::new(x, header.y + 7.0, BUTTON, BUTTON);
        let hovered = enabled && app.pointer.hover == Hover::DebugControl(control);
        draw_button(frame, p, rect, header, hovered, false);
        let text_w = frame.measure(FontKind::Ui, app.fonts.ui_small, glyph);
        ui_text_small(
            frame,
            app,
            rect.x + rect.w / 2.0 - text_w / 2.0,
            rect,
            glyph,
            if enabled { p.text } else { p.text_dim },
            header,
        );
        app.debug.ui.control_hits.push((rect, control));
        if enabled {
            app.hits
                .terminal
                .push((rect, crate::app::terminal::HeaderHit::Debug(control)));
        }
        x += BUTTON + 4.0;
    }
    let close = Rect::new(card.right() - 34.0, header.y + 7.0, BUTTON, BUTTON);
    let hovered = app.pointer.hover == Hover::DebugControl(DebugControl::Close);
    draw_button(frame, p, close, header, hovered, false);
    icons::close(
        frame,
        center(close),
        10.0,
        if hovered { p.text } else { p.text_muted },
        header,
    );
    app.debug.ui.control_hits.push((close, DebugControl::Close));
    app.hits.terminal.push((
        close,
        crate::app::terminal::HeaderHit::Debug(DebugControl::Close),
    ));
    let status = app.debug_status_text();
    let status_x =
        (header.right() - 44.0 - frame.measure(FontKind::Ui, app.fonts.ui_small, &status))
            .max(x + 8.0);
    ui_text_small(
        frame,
        app,
        status_x,
        header,
        &status,
        if app.debug.status == DebugStatus::Failed {
            p.error
        } else {
            p.text_muted
        },
        header,
    );
}

fn draw_stack(app: &mut App, frame: &mut Frame<'_>, area: Rect) {
    let p = app.palette;
    let title = winc_i18n::t!("debug-call-stack");
    ui_text_small(
        frame,
        app,
        area.x + PAD,
        Rect::new(area.x, area.y, area.w, 24.0),
        title,
        p.text_muted,
        area,
    );
    let rows = ((area.h - 110.0) / ROW).floor().max(0.0) as usize;
    for (index, frame_data) in app.debug.stack.iter().take(rows).enumerate() {
        let row = Rect::new(area.x, area.y + 26.0 + index as f32 * ROW, area.w, ROW);
        let selected = index == app.debug.ui.selected_frame;
        let hovered = app.pointer.hover == Hover::DebugStack(index);
        if selected || hovered {
            frame.rect_clipped(row, if selected { p.selected } else { p.hover }, 0.0, area);
        }
        let line = frame_data.line.map(|line| line + 1).unwrap_or_default();
        let location = frame_data
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let text = if location.is_empty() {
            format!("{}  {}", frame_data.name, line)
        } else {
            format!("{}  {}:{line}", frame_data.name, location)
        };
        let text = truncate_end(frame, &text, app.fonts.ui_small, area.w - 2.0 * PAD);
        ui_text_small(
            frame,
            app,
            row.x + PAD,
            row,
            &text,
            if selected { p.text } else { p.text_muted },
            area,
        );
        app.debug.ui.stack_hits.push((row, index));
        app.hits
            .terminal
            .push((row, crate::app::terminal::HeaderHit::DebugStack(index)));
    }
    if app.debug.stack.is_empty() {
        let empty = winc_i18n::t!("debug-stack-empty");
        ui_text_small(
            frame,
            app,
            area.x + PAD,
            Rect::new(area.x, area.y + 32.0, area.w, ROW),
            empty,
            p.text_dim,
            area,
        );
    }
}

fn draw_variables(app: &mut App, frame: &mut Frame<'_>, area: Rect) {
    let p = app.palette;
    let title = winc_i18n::t!("debug-variables");
    ui_text_small(
        frame,
        app,
        area.x + PAD,
        Rect::new(area.x, area.y, area.w, 24.0),
        title,
        p.text_muted,
        area,
    );
    let mut y = area.y + 26.0;
    for scope_index in 0..app.debug.scopes.len() {
        if y + ROW > area.bottom() - 90.0 {
            break;
        }
        let scope = &app.debug.scopes[scope_index];
        let row = Rect::new(area.x, y, area.w, ROW);
        let selected = scope_index == app.debug.ui.selected_scope;
        if selected {
            frame.rect_clipped(row, p.selected_unfocused, 0.0, area);
        }
        ui_text_small(frame, app, row.x + PAD, row, &scope.name, p.text, area);
        y += ROW;
        for variable_index in 0..scope.variables.len() {
            if y + ROW > area.bottom() - 90.0 {
                break;
            }
            let variable = app.debug.scopes[scope_index].variables[variable_index].clone();
            let path = crate::app::debug::DebugPath::new(&[scope_index, variable_index]);
            let row = Rect::new(area.x, y, area.w, ROW);
            draw_variable(app, frame, area, row, path, &variable, 0);
            let children = variable.children.len();
            y += ROW * (children + 1) as f32;
        }
    }

    if app.debug.scopes.is_empty() {
        let empty = winc_i18n::t!("debug-variables-empty");
        ui_text_small(
            frame,
            app,
            area.x + PAD,
            Rect::new(area.x, area.y + 32.0, area.w, ROW),
            empty,
            p.text_dim,
            area,
        );
    }
}

fn draw_variable(
    app: &mut App,
    frame: &mut Frame<'_>,
    area: Rect,
    row: Rect,
    path: crate::app::debug::DebugPath,
    variable: &crate::app::debug::Variable,
    depth: usize,
) {
    let p = app.palette;
    let hovered = app.pointer.hover == Hover::DebugVariable(path);
    if hovered {
        frame.rect_clipped(row, p.hover, 0.0, area);
    }
    let arrow = if variable.reference != 0 {
        if variable.expanded { "▾" } else { "▸" }
    } else {
        " "
    };
    let text = format!("{}{}: {}", "  ".repeat(depth), arrow, variable.name);
    let x = ui_text_small(frame, app, row.x + PAD, row, &text, p.text_muted, area);
    let value = truncate_end(
        frame,
        &variable.value,
        app.fonts.ui_small,
        (row.right() - x - PAD).max(0.0),
    );
    ui_text_small(frame, app, x + 4.0, row, &value, p.syntax_string, area);
    app.debug.ui.variable_hits.push((row, path));
    app.hits
        .terminal
        .push((row, crate::app::terminal::HeaderHit::DebugVariable(path)));
    if variable.expanded {
        let mut y = row.bottom();
        for (index, child) in variable.children.iter().enumerate() {
            let child_path = path.with_child(index);
            let child_row = Rect::new(row.x, y, row.w, ROW);
            draw_variable(app, frame, area, child_row, child_path, child, depth + 1);
            y += ROW;
        }
    }
}

fn draw_output(app: &mut App, frame: &mut Frame<'_>, area: Rect, _stack: Rect) {
    let p = app.palette;
    frame.rect(Rect::new(area.x, area.y, area.w, 1.0), p.border);
    let title = winc_i18n::t!("debug-output");
    ui_text_small(
        frame,
        app,
        area.x + PAD,
        Rect::new(area.x, area.y, area.w, 22.0),
        title,
        p.text_muted,
        area,
    );
    let lines = app.debug.output.lines().collect::<Vec<_>>();
    let visible = ((area.h - 24.0) / 18.0).floor().max(0.0) as usize;
    let start = if app.debug.ui.output_scroll == usize::MAX {
        lines.len().saturating_sub(visible)
    } else {
        app.debug
            .ui
            .output_scroll
            .min(lines.len().saturating_sub(visible))
    };
    for (index, line) in lines.iter().skip(start).take(visible).enumerate() {
        let row = Rect::new(area.x, area.y + 22.0 + index as f32 * 18.0, area.w, 18.0);
        let line = truncate_end(frame, line, app.fonts.ui_small, area.w - 2.0 * PAD);
        ui_text_small(frame, app, row.x + PAD, row, &line, p.text_muted, area);
    }
}
