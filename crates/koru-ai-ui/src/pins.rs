// SPDX-License-Identifier: MPL-2.0

//! **Pins for moving the model picker and the approval-mode list out of this
//! crate** (Bots P1.8, `docs/features/bots/P1.8.PICKER.notes.md` §4.4, slice
//! S0). Written against the code as it was before anything moved, they
//! record what the two controls draw, declare, hit, show under the pointer,
//! do with each key and send, as plain text compared with goldens recorded
//! from that code. A later slice that changes any of it fails here.
//!
//! The one field left out is an inspector record's `source`, the
//! construction site's `file:line`: a move changes it by definition.
//!
//! Goldens live beside each pin file as `.txt` files read at compile time
//! (`include_str!`), so a test reads no file. A pin that differs prints the
//! whole text it produced between `=====PIN <name> BEGIN=====` and
//! `=====PIN <name> END=====`, so a golden can be recorded, or a change
//! reviewed, from the test's own output.
//!
//! **They measure text with the bundled fonts** (`assets/fonts`, fetched by
//! `scripts/setup/fetch-fonts.sh`, as CI does). Without them the system's
//! fallbacks measure differently, pill widths and elisions move, and these
//! pins fail where nothing changed: fetch the fonts first.

use koru_draw::el::SemNode;
use koru_draw::geometry::Rect;
use koru_draw::inspect::Snapshot;

/// A coordinate, to the hundredth, with no negative zero.
pub(crate) fn num(value: f64) -> String {
    let text = format!("{value:.2}");
    if text == "-0.00" {
        "0.00".to_owned()
    } else {
        text
    }
}

/// A rectangle as `[x, y, w, h]`.
pub(crate) fn rect(rect: Rect) -> String {
    format!(
        "[{} {} {} {}]",
        num(f64::from(rect.x)),
        num(f64::from(rect.y)),
        num(f64::from(rect.w)),
        num(f64::from(rect.h))
    )
}

fn corners(bounds: [f64; 4]) -> String {
    format!(
        "[{} {} {} {}]",
        num(bounds[0]),
        num(bounds[1]),
        num(bounds[2]),
        num(bounds[3])
    )
}

/// **Every inspector record but its `source`**: its index, parent, kind,
/// name, bounds, clip, paint order, hoisting and properties.
pub(crate) fn records(snapshot: &Snapshot) -> String {
    let mut out = format!(
        "records {} truncated {}\n",
        snapshot.records.len(),
        snapshot.truncated
    );
    for (index, record) in snapshot.records.iter().enumerate() {
        let parent = record
            .parent
            .map_or_else(|| "-".to_owned(), |parent| parent.to_string());
        out.push_str(&format!(
            "{index} ^{parent} {} {:?} b{} c{} z{}{}\n",
            record.kind,
            record.name,
            corners(record.bounds),
            corners(record.clip),
            record.z,
            if record.hoisted { " hoisted" } else { "" }
        ));
        for property in &record.properties {
            out.push_str(&format!(
                "    {} = {:?} ({})\n",
                property.name, property.value, property.origin
            ));
        }
    }
    out
}

/// **The whole semantics tree**, depth first, a level an indent.
pub(crate) fn semantics(nodes: &[SemNode]) -> String {
    fn walk(nodes: &[SemNode], depth: usize, out: &mut String) {
        for node in nodes {
            out.push_str(&format!(
                "{}{:?} {:?} key={:?} b{} selected={:?} disabled={} focusable={} actions={:?}\n",
                "  ".repeat(depth),
                node.role,
                node.name,
                node.key,
                rect(node.bounds),
                node.selected,
                node.disabled,
                node.focusable,
                node.actions,
            ));
            walk(&node.children, depth + 1, out);
        }
    }
    let mut out = String::new();
    walk(nodes, 0, &mut out);
    if out.is_empty() {
        out.push_str("(none)\n");
    }
    out
}

/// **A grid of what a point is**, run-length encoded: each distinct answer
/// gets a number in the legend, in the order first met, and each grid row is
/// its runs of numbers.
#[derive(Default)]
pub(crate) struct Grid {
    legend: Vec<String>,
    rows: Vec<String>,
}

impl Grid {
    /// Every `step` units over `area` grown by `margin` on each side,
    /// row by row: what `cell` says each point is.
    pub(crate) fn over(
        area: Rect,
        margin: f32,
        step: f32,
        mut cell: impl FnMut(f32, f32) -> String,
    ) -> String {
        let mut grid = Grid::default();
        let mut y = area.y - margin;
        while y <= area.y + area.h + margin {
            let mut runs: Vec<(usize, usize)> = Vec::new();
            let mut x = area.x - margin;
            while x <= area.x + area.w + margin {
                let answer = cell(x, y);
                let index = match grid.legend.iter().position(|known| *known == answer) {
                    Some(index) => index,
                    None => {
                        grid.legend.push(answer);
                        grid.legend.len() - 1
                    }
                };
                match runs.last_mut() {
                    Some((last, count)) if *last == index => *count += 1,
                    _ => runs.push((index, 1)),
                }
                x += step;
            }
            grid.rows.push(format!(
                "y{} {}",
                num(f64::from(y)),
                runs.iter()
                    .map(|(index, count)| format!("{index}x{count}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
            y += step;
        }
        let mut out = format!(
            "grid over {} margin {} step {}\n",
            rect(area),
            num(f64::from(margin)),
            num(f64::from(step))
        );
        for (index, answer) in grid.legend.iter().enumerate() {
            out.push_str(&format!("{index}: {answer}\n"));
        }
        for row in &grid.rows {
            out.push_str(row);
            out.push('\n');
        }
        out
    }
}

/// **A command as the transcripts write it**: what it asks for, without the
/// chat's id, which is new each run.
pub(crate) fn command(command: &crate::reader::Command) -> String {
    use crate::reader::Command;
    match command {
        Command::UpdateModel(_, request) => format!("update_model {:?}", request.model),
        Command::UpdateAgentSettings(_, request) => format!(
            "update_agent_settings mode={:?} fast={:?}",
            request.mode, request.fast
        ),
        Command::Validate(provider)
        | Command::DiscoverModels(provider)
        | Command::RefreshModels(provider) => {
            format!("{} {}", command.name(), provider.as_str())
        }
        Command::SelectProvider(request) => {
            format!("select_provider {}", request.provider.as_str())
        }
        Command::UpdateOpenAi(request) => format!(
            "update_openai model={:?} reasoning={:?}",
            request.model, request.reasoning
        ),
        Command::UpdateOllama(request) => format!(
            "update_ollama model={:?} reasoning={:?}",
            request.model, request.reasoning
        ),
        Command::Create(request) => format!("create {:?}", request.model),
        other => other.name().to_owned(),
    }
}

/// Every command queued since the last drain, in order.
pub(crate) fn drain(commands: &std::sync::mpsc::Receiver<crate::reader::Command>) -> String {
    let all: Vec<String> = commands.try_iter().map(|c| command(&c)).collect();
    format!("[{}]", all.join(", "))
}

/// **Pins compared with their goldens**, every one of them before the test
/// fails, so one run shows every difference.
#[derive(Default)]
pub(crate) struct Pins {
    changed: Vec<String>,
}

impl Pins {
    /// Compare `actual` with `golden`. A golden checked out with Windows
    /// line ends reads the same. On a difference the whole new text is
    /// printed for the test's output, and the first line that differs is
    /// kept for [`Self::finish`].
    pub(crate) fn check(&mut self, name: &str, actual: &str, golden: &str) {
        let golden = golden.replace("\r\n", "\n");
        if actual == golden {
            return;
        }
        println!("=====PIN {name} BEGIN=====\n{actual}=====PIN {name} END=====");
        let first = actual
            .lines()
            .zip(golden.lines())
            .position(|(a, g)| a != g)
            .unwrap_or_else(|| actual.lines().count().min(golden.lines().count()));
        self.changed.push(format!(
            "pin {name} changed at line {}:\n  now: {:?}\n  was: {:?}",
            first + 1,
            actual.lines().nth(first),
            golden.lines().nth(first),
        ));
    }

    /// Fail with every pin that changed.
    #[track_caller]
    pub(crate) fn finish(self) {
        assert!(self.changed.is_empty(), "{}", self.changed.join("\n"));
    }
}
