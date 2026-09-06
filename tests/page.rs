//! The page's own consistency: its glyphs, its palette, and the contrast of
//! the two together.
//!
//! `viewer.rs` is about serving the page and what it does once it is running.
//! This is about the page as a document — the checks that catch what a visual
//! change breaks quietly. A `<use>` pointing at a symbol that is not there
//! renders nothing at all, a `var()` naming a token nobody defined falls back
//! to the initial value, and a colour that misses AA looks fine to whoever
//! picked it on the monitor they picked it on. None of the three fails a test
//! or throws in a console; all three are arithmetic or a lookup, so they are
//! here rather than left to be noticed.
//!
//! There is no browser in any of this. What it cannot see is whether the page
//! LOOKS right — that a box does not overflow, that one thing is not behind
//! another. That is still a person's job.

use std::collections::{BTreeMap, BTreeSet};

use saneha::server::VIEWER;

/// The text between a marker and the brace that closes what follows it.
fn braced(page: &str, opener: &str) -> String {
    let at = page
        .find(opener)
        .unwrap_or_else(|| panic!("the page has no {opener:?}"));
    let open = page[at..].find('{').expect("a block to open") + at;
    let mut depth = 0usize;
    for (offset, character) in page[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return page[open + 1..open + offset].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("the block opened by {opener:?} never closes");
}

/// The text with its `/* comments */` taken out. They are prose, and the
/// colons in them would otherwise be read as declarations.
fn uncommented(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("/*") {
        out.push_str(&rest[..at]);
        let Some(end) = rest[at..].find("*/") else {
            break;
        };
        rest = &rest[at + end + 2..];
    }
    out.push_str(rest);
    out
}

/// Every `name: value` pair in a declaration block whose name is a custom
/// property. Values are taken as written, up to the semicolon.
fn tokens(block: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let block = uncommented(block);
    for declaration in block.split(';') {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let name = name.trim();
        // A custom property, and not a `var(--x)` inside somebody's value.
        if name.starts_with("--") && !name.contains(|c: char| c.is_whitespace()) {
            found.insert(name.to_string(), value.trim().to_string());
        }
    }
    found
}

fn light() -> BTreeMap<String, String> {
    tokens(&braced(VIEWER, "\n:root {"))
}

fn dark() -> BTreeMap<String, String> {
    let mut resolved = light();
    resolved.extend(tokens(&braced(VIEWER, "[data-theme=\"dark\"] {")));
    resolved
}

/// Every occurrence of `pattern` followed by a name, as that name: the page's
/// own references, read out of the page. A name that is not a plain
/// identifier is skipped, which is how the one `href="#' + id + '"` inside
/// `icon()` is left out — that is the template every real reference is built
/// by, not a reference itself.
fn referenced(pattern: &str, ends: &[char]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = VIEWER;
    while let Some(at) = rest.find(pattern) {
        rest = &rest[at + pattern.len()..];
        let end = rest.find(ends).unwrap_or(rest.len());
        let name = rest[..end].trim();
        let plain = !name.is_empty()
            && name
                .trim_start_matches("--")
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-');
        if plain {
            found.insert(name.to_string());
        }
    }
    found
}

/// The tokens a `var()` names WITHOUT a fallback beside them. One written as
/// `var(--hb, var(--panel3))` is saying out loud that it may not be defined
/// here — the host colours are on the element, not the palette — so it is not
/// something to hold the palette to.
fn required_tokens() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = VIEWER;
    while let Some(at) = rest.find("var(") {
        rest = &rest[at + 4..];
        let Some(end) = rest.find([',', ')']) else {
            break;
        };
        let has_fallback = rest.as_bytes()[end] == b',';
        let name = rest[..end].trim();
        if name.starts_with("--") && !has_fallback {
            found.insert(name.to_string());
        }
    }
    found
}

// ---- the glyphs -----------------------------------------------------------

/// A `<use>` pointing at a symbol that is not in the sprite draws nothing, and
/// says nothing about it: no console message, no fallback, just a control with
/// an empty square where its glyph should be. Every reference is checked
/// against the sprite, in both directions.
#[test]
fn every_glyph_the_page_asks_for_is_in_the_sprite() {
    let defined = referenced("<symbol id=\"", &['"']);
    assert!(
        defined.len() > 10,
        "the sprite was not found where this expected it: {defined:?}"
    );

    // Referenced two ways: written into the markup, and built by `icon(id)`
    // in the script.
    let mut asked: BTreeSet<String> = referenced("href=\"#", &['"']);
    asked.extend(referenced("icon(\"", &['"']));

    for id in &asked {
        assert!(
            defined.contains(id),
            "the page draws {id:?}, which is not a symbol in the sprite"
        );
    }

    // The harness glyphs are built rather than named: `harnessIcon` makes
    // "h" + the harness, for every harness the page knows, and "hx" for one it
    // does not. Those keys are read out of the page so this stays true when a
    // harness is added.
    let harnesses = braced(VIEWER, "const HARNESSES = ");
    let mut built: BTreeSet<String> = harnesses
        .split(',')
        .filter_map(|entry| entry.split_once(':'))
        .map(|(key, _)| format!("h{}", key.trim()))
        .collect();
    built.insert("hx".to_string());
    assert!(
        built.len() >= 4,
        "the harness list was not found where this expected it: {built:?}"
    );

    for id in &built {
        assert!(
            defined.contains(id),
            "the page draws {id:?} for a harness it knows, and the sprite has no such symbol"
        );
    }

    // And nothing is carried that nothing draws: a glyph left behind after the
    // thing that used it went is bytes in a page that is held to a budget.
    //
    // Looser than the check above, deliberately. Some glyphs are picked by a
    // variable rather than named at the call — a system line chooses between
    // `in`, `out` and `sk` on the message kind — so the id appearing anywhere
    // as a quoted string is what counts as drawn. That still catches the case
    // this is for: a glyph whose last mention went with the code that drew it.
    for id in &defined {
        let mentioned =
            VIEWER.contains(&format!("\"{id}\"")) || VIEWER.contains(&format!("#{id}\""));
        assert!(
            asked.contains(id) || built.contains(id) || mentioned,
            "the sprite carries {id:?}, which nothing in the page draws"
        );
    }
}

// ---- the palette ----------------------------------------------------------

/// A `var(--x)` naming a token nobody defined is not an error: the property
/// takes its initial value, so a colour becomes black or transparent and a
/// border disappears, in whichever theme the missing definition was in.
#[test]
fn every_token_the_page_uses_is_defined_in_both_themes() {
    let (light, dark) = (light(), dark());

    // Everything the stylesheet and the script ask for, minus the ones with a
    // fallback written into the `var()` itself, which are the host colours a
    // participant may not have.
    for name in required_tokens() {
        assert!(
            light.contains_key(&name),
            "the page uses {name:?}, which the light palette does not define"
        );
        assert!(
            dark.contains_key(&name),
            "the page uses {name:?}, which is defined in neither palette"
        );
    }
}

/// The two palettes are the same set of names. A token added to one and not
/// the other is a theme that inherits the other's value — which is how the
/// design canvas ended up with a light badge colour in its dark blocks, and is
/// invisible until somebody is looking at the theme that is wrong.
#[test]
fn the_two_palettes_carry_the_same_tokens() {
    let light = light();
    let dark = tokens(&braced(VIEWER, "[data-theme=\"dark\"] {"));

    for name in light.keys() {
        // The font stacks are deliberately one set: a theme changes colour,
        // not typeface.
        if matches!(name.as_str(), "--ui" | "--sans" | "--mono") {
            continue;
        }
        assert!(
            dark.contains_key(name),
            "{name:?} is in the light palette and not the dark one, so dark inherits light's value"
        );
    }

    for name in dark.keys() {
        assert!(
            light.contains_key(name),
            "{name:?} is in the dark palette and not the light one, so it is undefined in light"
        );
    }
}

// ---- the contrast ---------------------------------------------------------

/// The relative luminance of an `#rrggbb`, as WCAG defines it.
fn luminance(colour: &str) -> f64 {
    let short = colour.trim().trim_start_matches('#');
    // `#fff` is `#ffffff`, and the palette uses both spellings.
    let hex = if short.len() == 3 {
        short.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        short.to_string()
    };
    assert_eq!(
        hex.len(),
        6,
        "expected an #rgb or #rrggbb colour, got {colour:?}"
    );
    let hex = hex.as_str();
    let channel = |at: usize| {
        let value = u8::from_str_radix(&hex[at..at + 2], 16)
            .unwrap_or_else(|_| panic!("{colour:?} is not a colour"));
        let value = f64::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
}

fn contrast(one: &str, other: &str) -> f64 {
    let (a, b) = (luminance(one), luminance(other));
    let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Which token carries text on which. Written out rather than inferred,
/// because only the stylesheet knows what sits on what, and a list that says
/// so is the thing worth keeping: adding a surface means adding its pair here,
/// which is the moment to check it.
const ON: &[(&str, &str)] = &[
    // the transcript, and the panels either side of it
    ("--fg", "--panel"),
    ("--fg", "--panel2"),
    ("--fg", "--bg"),
    ("--fg2", "--panel"),
    ("--muted", "--panel"),
    ("--muted", "--panel2"),
    ("--muted", "--panel3"),
    ("--ink", "--soft"),
    ("--on", "--accent"),
    ("--danger", "--panel"),
    // the rail, which has its own surface and so its own arithmetic
    ("--rail-fg", "--rail"),
    ("--rail-fg", "--rail2"),
    ("--rail-fg", "--rail-btn"),
    ("--rail-muted", "--rail"),
    ("--rail-muted", "--rail2"),
    ("--rail-muted", "--rail-btn"),
    ("--rail-head", "--rail"),
    ("--rail-head", "--rail2"),
    ("--rail-sel-fg", "--rail-sel"),
    ("--rail-sel-sub", "--rail-sel"),
    ("--rail-badge-fg", "--rail-badge"),
];

/// Every pair above clears WCAG AA for body text in both themes.
///
/// 4.5:1, and not the 3:1 large text is allowed, because the page has almost
/// none: the rail's purpose line is 11px, an unread badge is 11px, and a
/// message body is 13.5px. A palette picked by eye passes this by luck — the
/// rail's purpose line went out at 3.6:1 in all four tints it was drawn in,
/// and the unread badge at 4.36:1, both of which looked perfectly fine to
/// whoever chose them.
#[test]
fn the_text_on_every_surface_clears_aa() {
    const AA: f64 = 4.5;
    for (theme, palette) in [("light", light()), ("dark", dark())] {
        for (ink, surface) in ON {
            let (Some(one), Some(other)) = (palette.get(*ink), palette.get(*surface)) else {
                panic!("{theme}: {ink} or {surface} is not a token");
            };
            let ratio = contrast(one, other);
            assert!(
                ratio >= AA,
                "{theme}: {ink} ({one}) on {surface} ({other}) is {ratio:.2}:1, under AA's {AA}:1"
            );
        }
    }
}

/// The host colours are a participant's identity at 9 to 11px, so the ink of
/// each pair is held to the same bar as everything else — in both themes,
/// because each pair is redefined for dark.
#[test]
fn every_host_colour_carries_legible_ink() {
    const AA: f64 = 4.5;
    for index in 0..8 {
        for (theme, opener) in [
            ("light", format!("\n.h{index} {{")),
            ("dark", format!("[data-theme=\"dark\"] .h{index} {{")),
        ] {
            let pair = tokens(&braced(VIEWER, &opener));
            let (Some(background), Some(ink)) = (pair.get("--hb"), pair.get("--hi")) else {
                panic!("{theme}: .h{index} does not define both --hb and --hi");
            };
            let ratio = contrast(background, ink);
            assert!(
                ratio >= AA,
                "{theme}: .h{index} is {ink} on {background}, {ratio:.2}:1, under AA's {AA}:1"
            );
        }
    }
}
