use ratatui::style::Color;
use std::path::PathBuf;

pub struct Palette {
    pub background: Color,
    pub selection: Color,
    pub text: Color,
    pub subtext: Color,
    pub accent: Color,
    pub muted: Color,
    pub red: Color,
    pub yellow: Color,
    pub green: Color,
    pub blue: Color,
    pub mauve: Color,
    pub peach: Color,
    pub teal: Color,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            background: Color::Black,
            selection: Color::DarkGray,
            text: Color::Reset,
            subtext: Color::Gray,
            accent: Color::Cyan,
            muted: Color::DarkGray,
            red: Color::Red,
            yellow: Color::Yellow,
            green: Color::Green,
            blue: Color::Blue,
            mauve: Color::Magenta,
            peach: Color::LightRed,
            teal: Color::Cyan,
        }
    }
}

pub fn herdr_config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("HERDR_CONFIG_PATH")
        && !path.is_empty()
    {
        return Some(path.into());
    }
    if let Ok(path) = std::env::var("XDG_CONFIG_HOME")
        && !path.is_empty()
    {
        return Some(PathBuf::from(path).join("herdr/config.toml"));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/herdr/config.toml"))
}

impl Palette {
    pub fn load() -> Self {
        Self::from_config(
            &herdr_config_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default(),
        )
    }

    pub fn from_config(text: &str) -> Self {
        let mut palette = Self::default();
        let Ok(value) = text.parse::<toml::Value>() else {
            return palette;
        };
        let custom = value.get("theme").and_then(|t| t.get("custom"));
        let color = |key: &str| {
            custom
                .and_then(|c| c.get(key))
                .and_then(|v| v.as_str())
                .and_then(parse_color)
        };
        let background = color("sidebar_bg")
            .or_else(|| color("surface_dim"))
            .or_else(|| {
                if let Some(Color::Rgb(r, g, b)) = color("panel_bg") {
                    Some(Color::Rgb(
                        r.saturating_sub(r / 5 + 1),
                        g.saturating_sub(g / 5 + 1),
                        b.saturating_sub(b / 5 + 1),
                    ))
                } else {
                    None
                }
            });
        if let Some(bg) = background {
            palette.background = bg;
        }
        if let Some(c) = color("active_row_bg").or_else(|| color("selection_bg")) {
            palette.selection = c;
        }
        if let Some(c) = color("text") {
            palette.text = c;
        }
        if let Some(c) = color("subtext0") {
            palette.subtext = c;
        }
        if let Some(c) = value
            .get("ui")
            .and_then(|u| u.get("accent"))
            .and_then(|v| v.as_str())
            .and_then(parse_color)
            .or_else(|| color("accent"))
        {
            palette.accent = c;
        }
        if let Some(c) = color("overlay0") {
            palette.muted = c;
        }
        if let Some(c) = color("red") {
            palette.red = c;
        }
        if let Some(c) = color("yellow") {
            palette.yellow = c;
        }
        if let Some(c) = color("green") {
            palette.green = c;
        }
        if let Some(c) = color("blue") {
            palette.blue = c;
        }
        if let Some(c) = color("mauve") {
            palette.mauve = c;
        }
        if let Some(c) = color("peach") {
            palette.peach = c;
        }
        if let Some(c) = color("teal") {
            palette.teal = c;
        }
        palette
    }
}

pub fn parse_color(text: &str) -> Option<Color> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        if !hex.is_ascii() {
            return None;
        }
        let expanded;
        let hex = if hex.len() == 3 {
            expanded = hex.chars().flat_map(|c| [c, c]).collect::<String>();
            &expanded
        } else {
            hex
        };
        if hex.len() != 6 {
            return None;
        }
        return Some(Color::Rgb(
            u8::from_str_radix(&hex[..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..], 16).ok()?,
        ));
    }
    if let Some(rgb) = text.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let values: Vec<u8> = rgb
            .split(',')
            .map(|s| s.trim().parse())
            .collect::<Result<_, _>>()
            .ok()?;
        if let [r, g, b] = values.as_slice() {
            return Some(Color::Rgb(*r, *g, *b));
        }
        return None;
    }
    match text.to_ascii_lowercase().as_str() {
        "reset" | "default" | "none" | "transparent" => Some(Color::Reset),
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "white" => Some(Color::White),
        "gray" | "grey" => Some(Color::Gray),
        "darkgray" | "darkgrey" => Some(Color::DarkGray),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherits_terminal_ansi_colors_without_a_theme_override() {
        let p = Palette::from_config("");
        assert_eq!(p.text, Color::Reset);
        assert_eq!(p.subtext, Color::Gray);
        assert_eq!(p.accent, Color::Cyan);
        assert_eq!(p.background, Color::Black);
        assert_eq!(p.mauve, Color::Magenta);
        assert_eq!(p.peach, Color::LightRed);
        assert_eq!(p.teal, Color::Cyan);
    }

    #[test]
    fn uses_herdr_sidebar_background_and_custom_terminal_palette() {
        let p = Palette::from_config(
            r##"
[theme]
name = "terminal"
[theme.custom]
sidebar_bg = "#171B20"
panel_bg = "#1D2228"
active_row_bg = "#2C313A"
text = "#E7EAEE"
subtext0 = "#A9B1D6"
accent = "#5CCEFF"
overlay0 = "#586172"
green = "#38FFA5"
mauve = "#B0A0FF"
peach = "#FF9858"
teal = "#41FFDC"
"##,
        );
        assert_eq!(p.background, Color::Rgb(23, 27, 32));
        assert_eq!(p.selection, Color::Rgb(44, 49, 58));
        assert_eq!(p.text, Color::Rgb(231, 234, 238));
        assert_eq!(p.subtext, Color::Rgb(169, 177, 214));
        assert_eq!(p.accent, Color::Rgb(92, 206, 255));
        assert_eq!(p.muted, Color::Rgb(88, 97, 114));
        assert_eq!(p.green, Color::Rgb(56, 255, 165));
        assert_eq!(p.mauve, Color::Rgb(176, 160, 255));
        assert_eq!(p.peach, Color::Rgb(255, 152, 88));
        assert_eq!(p.teal, Color::Rgb(65, 255, 220));
    }

    #[test]
    fn darkens_known_terminal_background_when_sidebar_color_is_absent() {
        let p = Palette::from_config("[theme.custom]\npanel_bg = \"#203040\"");
        match p.background {
            Color::Rgb(r, g, b) => {
                assert!(r < 32);
                assert!(g < 48);
                assert!(b < 64);
            }
            _ => panic!("expected a darker RGB background"),
        }
    }

    #[test]
    fn invalid_colors_fall_back_individually() {
        let p = Palette::from_config(
            "[theme.custom]\nsidebar_bg = \"invalid\"\ntext = \"#fff\"\naccent = \"cyan\"",
        );
        assert_eq!(p.background, Color::Black);
        assert_eq!(p.text, Color::Rgb(255, 255, 255));
        assert_eq!(p.accent, Color::Cyan);
        assert_eq!(Palette::from_config("bad [toml").background, Color::Black);
    }

    #[test]
    fn color_parser_handles_rgb_ansi_and_reset_without_panics() {
        assert_eq!(parse_color("rgb(12, 34, 56)"), Some(Color::Rgb(12, 34, 56)));
        assert_eq!(parse_color("#aBc"), Some(Color::Rgb(170, 187, 204)));
        assert_eq!(parse_color("reset"), Some(Color::Reset));
        for text in ["#💣00", "#12345", "#gggggg", "rgb(256,0,0)", "rgb(0,0)", ""] {
            assert_eq!(parse_color(text), None, "{text}");
        }
    }
}
