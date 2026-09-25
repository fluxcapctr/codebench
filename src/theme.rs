//! Reads the active Omarchy theme and terminal font, and turns them into GTK
//! CSS plus a VTE palette.

use crate::store::home;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn theme_dir() -> PathBuf {
    home().join(".local/state/omarchy/current/theme")
}

/// Omarchy rewrites this on every theme switch, so it is the file to watch.
pub fn theme_marker() -> PathBuf {
    home().join(".local/state/omarchy/current/theme.name")
}

pub fn font_config() -> PathBuf {
    home().join(".config/ghostty/config")
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub muted: String,
    pub selection: String,
    pub dark_background: String,
    pub light_background: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub cursor: String,
    pub palette: [String; 16],
    pub font_family: String,
    pub font_size: f64,
}

/// Parses `key = value` lines, ignoring comments, quotes and section headers.
fn parse_kv(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with('[') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            Some((k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        })
        .collect()
}

fn read(path: PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

impl Theme {
    pub fn load() -> Theme {
        let dir = theme_dir();
        let colors = parse_kv(&read(dir.join("colors.toml")));
        let get = |k: &str, fallback: &str| {
            colors
                .get(k)
                .filter(|v| v.starts_with('#'))
                .cloned()
                .unwrap_or_else(|| fallback.to_string())
        };

        let background = get("background", "#1a1b26");
        let foreground = get("foreground", "#c0caf5");
        let accent = get("accent", &get("blue", "#7aa2f7"));
        let muted = get("muted", &get("color8", "#565f89"));

        // ghostty.conf carries the full 16-color palette Omarchy generated.
        let mut palette: [String; 16] = Default::default();
        let mut cursor = get("bright_foreground", &foreground);
        for line in read(dir.join("ghostty.conf")).lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "palette" => {
                    if let Some((idx, hex)) = v.trim().split_once('=')
                        && let Ok(i) = idx.trim().parse::<usize>()
                        && i < 16
                    {
                        palette[i] = hex.trim().to_string();
                    }
                }
                "cursor-color" => cursor = v.trim().to_string(),
                _ => {}
            }
        }
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        for (i, slot) in palette.iter_mut().enumerate() {
            if slot.is_empty() {
                let name = if i < 8 {
                    names[i].to_string()
                } else {
                    format!("bright_{}", names[i - 8])
                };
                let fallback = match i {
                    0 => background.clone(),
                    7 | 15 => foreground.clone(),
                    8 => muted.clone(),
                    _ => accent.clone(),
                };
                *slot = get(&format!("color{i}"), &get(&name, &fallback));
            }
        }

        let font = parse_kv(&read(font_config()));
        let font_family = font
            .get("font-family")
            .cloned()
            .unwrap_or_else(|| "monospace".into());
        let font_size = font
            .get("font-size")
            .and_then(|s| s.parse().ok())
            .unwrap_or(10.0);

        Theme {
            dark_background: get("dark_background", &background),
            light_background: get("lighter_background", &get("selection", &muted)),
            selection: get("selection", &muted),
            red: get("red", &palette[1]),
            green: get("green", &palette[2]),
            yellow: get("yellow", &palette[3]),
            background,
            foreground,
            accent,
            muted,
            cursor,
            palette,
            font_family,
            font_size,
        }
    }

    pub fn css(&self) -> String {
        let Theme {
            background: bg,
            foreground: fg,
            accent,
            muted,
            selection,
            dark_background: dark,
            light_background: light,
            font_family: font,
            font_size: size,
            green,
            red,
            ..
        } = self;
        format!(
            r#"
* {{ font-family: "{font}", monospace; font-size: {size}pt; border-radius: 0; box-shadow: none; text-shadow: none; -gtk-icon-shadow: none; }}
window, .cb-root {{ background: {bg}; color: {fg}; }}
.cb-sidebar {{ background: {dark}; border-right: 1px solid {muted}; }}
.cb-brand {{ color: {accent}; font-weight: bold; padding: 10px 12px 6px 12px; }}
list, list > row {{ background: transparent; color: {fg}; }}
list > row {{ padding: 7px 12px; min-height: 0; outline: none; }}
list > row label {{ padding-top: 1px; }}
list > row:hover {{ background: alpha({light}, 0.6); }}
list > row:selected {{ background: {selection}; color: {fg}; }}
list > row:selected label {{ color: {fg}; }}
.cb-project {{ margin-top: 8px; }}
.cb-dim {{ color: {muted}; }}
.cb-header {{ background: {dark}; color: {fg}; padding: 4px 12px; border-bottom: 1px solid {muted}; }}
.cb-footer {{ background: {dark}; color: {muted}; padding: 3px 12px; border-top: 1px solid {muted}; }}
.cb-footer.cb-flash {{ color: {accent}; }}
vte-terminal {{ padding: 6px 10px; }}
.cb-empty {{ color: {muted}; }}
.cb-board {{ padding: 18px 24px; color: {fg}; }}
.cb-split {{ border-left: 1px solid {muted}; }}
paned > separator {{ background: {muted}; min-width: 1px; }}
.cb-root > separator, .cb-root > separator:hover, .cb-root > separator:focus, .cb-root > separator:backdrop {{ background: {muted}; min-width: 1px; box-shadow: none; outline: none; }}
paned > separator:focus, paned > separator:hover {{ background: {muted}; box-shadow: none; outline: none; }}
.cb-tabs {{ background: {dark}; border-bottom: 1px solid {muted}; padding: 0 4px; }}
.cb-tab {{ padding: 8px 14px; min-height: 0; color: {muted}; background: transparent; border-bottom: 2px solid transparent; }}
.cb-tab:hover {{ color: {fg}; background: alpha({light}, 0.6); }}
.cb-tab.active {{ color: {fg}; background: {bg}; border-bottom-color: {accent}; }}
.cb-views {{ background: {bg}; border-bottom: 1px solid {muted}; padding: 2px 4px; }}
.cb-view {{ padding: 7px 14px; min-height: 0; color: {fg}; background: transparent; border-bottom: 2px solid transparent; }}
.cb-view:hover {{ color: {accent}; background: alpha({light}, 0.6); }}
.cb-view.active {{ color: {accent}; border-bottom-color: {accent}; }}
.cb-page-title {{ padding: 16px 24px 8px; }}
.cb-page {{ padding: 0 16px 16px; background: transparent; }}
.cb-page > row {{ padding: 6px 8px; }}
.cb-new {{ margin-top: 6px; }}
@keyframes cb-pulse {{ 0% {{ opacity: 1; }} 50% {{ opacity: 0.25; }} 100% {{ opacity: 1; }} }}
.cb-pulse {{ animation: cb-pulse 1.4s ease-in-out infinite; }}
list > row.cb-done {{ background: alpha({green}, 0.14); border-left: 3px solid {green}; padding-left: 9px; }}
list > row.cb-needs {{ background: alpha({red}, 0.14); border-left: 3px solid {red}; padding-left: 9px; }}
textview.cb-editor, textview.cb-editor text {{ background: {bg}; color: {fg}; caret-color: {accent}; }}
textview.cb-editor text selection {{ background: {selection}; }}
popover.cb-menu > contents {{ background: {dark}; border: 1px solid {muted}; padding: 4px 0; }}
popover.cb-menu button {{ color: {fg}; padding: 3px 14px; min-height: 0; }}
popover.cb-menu button:hover, popover.cb-menu button:focus {{ background: {selection}; }}
popover.cb-menu separator {{ background: {muted}; margin: 3px 0; min-height: 1px; }}
.cb-picker {{ background: {dark}; border: 1px solid {accent}; padding: 10px; }}
.cb-picker-title {{ color: {accent}; font-weight: bold; margin-bottom: 6px; }}
.cb-picker entry {{ background: {bg}; color: {fg}; border: 1px solid {muted}; padding: 4px 8px; min-height: 0; caret-color: {accent}; outline: none; }}
.cb-picker entry:focus-within {{ border-color: {accent}; }}
.cb-picker list {{ margin-top: 6px; }}
scrollbar {{ background: transparent; }}
scrollbar slider {{ background: {muted}; min-width: 4px; }}
"#
        )
    }
}
