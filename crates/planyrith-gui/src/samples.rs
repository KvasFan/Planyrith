//! Example documents (docs/00 §4 "small example files", docs/13 0.7
//! classroom scenario): each is built by running registered commands on an
//! empty document, so the application ships them as a few lines of
//! parameters rather than files.

use serde_json::{Value, json};

/// An example document.
pub struct Sample {
    /// Menu label.
    pub name: &'static str,
    /// The commands that build it.
    pub commands: fn() -> Vec<(&'static str, Value)>,
}

/// The examples, in menu order.
pub fn catalogue() -> Vec<Sample> {
    vec![
        Sample {
            name: "Room",
            commands: room,
        },
        Sample {
            name: "Bookcase",
            commands: bookcase,
        },
    ]
}

/// A room 6 × 4 m with walls 200 mm thick, a door and a table.
fn room() -> Vec<(&'static str, Value)> {
    vec![
        (
            "rectangle",
            json!({ "corner_a": [0.0, 0.0, 0.0], "corner_b": [6000.0, 4000.0, 0.0] }),
        ),
        (
            "offset",
            json!({ "face": { "Query": "face" }, "distance": -200.0 }),
        ),
        (
            "push-pull",
            json!({ "face": { "Query": "face & area<5000000" }, "distance": 2700.0 }),
        ),
        ("make-group", json!({ "targets": "all" })),
        (
            "rectangle",
            json!({ "corner_a": [2000.0, 1500.0, 0.0], "corner_b": [3500.0, 2500.0, 0.0] }),
        ),
        (
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 750.0 }),
        ),
        ("make-group", json!({ "targets": "face | edge" })),
        (
            "material-create",
            json!({ "name": "Oak", "color": "#B08455" }),
        ),
        (
            "material-apply",
            json!({ "targets": "group & last", "material": "Oak" }),
        ),
    ]
}

/// A bookcase 800 × 300 × 1800 mm of 18 mm boards with four shelves.
fn bookcase() -> Vec<(&'static str, Value)> {
    let mut commands = Vec::new();
    let mut board = |a: [f64; 3], b: [f64; 3], height: f64| {
        commands.push(("rectangle", json!({ "corner_a": a, "corner_b": b })));
        commands.push((
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": height }),
        ));
        commands.push(("make-group", json!({ "targets": "face | edge" })));
    };
    // Sides, then the shelves between them.
    board([0.0, 0.0, 0.0], [18.0, 300.0, 0.0], 1800.0);
    board([782.0, 0.0, 0.0], [800.0, 300.0, 0.0], 1800.0);
    for i in 0..5 {
        let z = 60.0 + f64::from(i) * 420.0;
        board([18.0, 0.0, z], [782.0, 300.0, z], 18.0);
    }
    commands
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use crate::session::Session;

    #[test]
    fn every_sample_builds() {
        for sample in super::catalogue() {
            let mut s = Session::new();
            for (name, params) in (sample.commands)() {
                s.execute(name, params)
                    .unwrap_or_else(|e| panic!("{}: {e}", sample.name));
            }
            assert!(!s.doc().model.order().is_empty(), "{}", sample.name);
        }
    }
}
