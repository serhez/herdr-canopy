use ratatui::style::Color;

/// An RGB color, as a terminal reports it.
pub type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, Copy)]
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

impl Palette {
    pub fn from_config(text: &str) -> Self {
        Self::from_config_with(text, Self::default())
    }

    /// [`Self::from_config`], with `base`'s colors where the config sets none.
    pub fn from_config_with(text: &str, base: Palette) -> Self {
        let mut palette = base;
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

/// The color in a terminal's reply to the OSC `code` query (`10` for text,
/// `11` for background, `4;N` for palette color N): `ESC ] code ; rgb:R/G/B`,
/// with 1 to 4 hex digits a channel.
pub fn osc_color(reply: &str, code: &str) -> Option<Rgb> {
    let start = format!("\x1b]{code};rgb:");
    let rest = &reply[reply.find(&start)? + start.len()..];
    let body = &rest[..rest.find(['\x1b', '\x07'])?];
    let mut channels = body.split('/').map(|hex| {
        let digits = u32::try_from(hex.len())
            .ok()
            .filter(|n| (1..=4).contains(n))?;
        let value = u32::from_str_radix(hex, 16).ok()?;
        let max = 16u32.pow(digits) - 1;
        u8::try_from((value * 255 + max / 2) / max).ok()
    });
    let rgb = (channels.next()??, channels.next()??, channels.next()??);
    channels.next().is_none().then_some(rgb)
}

/// `background` moved by `amount` (0 to 1) of the way toward `toward`.
pub fn lift(background: Rgb, toward: Rgb, amount: f64) -> Color {
    let channel = |from: u8, to: u8| {
        let moved = f64::from(from) + (f64::from(to) - f64::from(from)) * amount;
        moved.round().clamp(0.0, 255.0) as u8
    };
    Color::Rgb(
        channel(background.0, toward.0),
        channel(background.1, toward.1),
        channel(background.2, toward.2),
    )
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
    fn a_host_chooses_the_colors_used_where_none_are_configured() {
        let base = Palette {
            background: Color::Reset,
            selection: Color::Rgb(1, 2, 3),
            ..Palette::default()
        };
        let p = Palette::from_config_with("", Palette { ..base });
        assert_eq!(p.background, Color::Reset);
        assert_eq!(p.selection, Color::Rgb(1, 2, 3));
        assert_eq!(p.accent, Color::Cyan);
        let p = Palette::from_config_with(
            "[theme.custom]\nsidebar_bg = \"#171B20\"\nactive_row_bg = \"#2C313A\"",
            base,
        );
        assert_eq!(p.background, Color::Rgb(23, 27, 32));
        assert_eq!(p.selection, Color::Rgb(44, 49, 58));
    }

    #[test]
    fn terminal_color_replies_are_read_in_any_precision() {
        let reply = "\x1b]10;rgb:e7e7/eded/f3f3\x1b\\\x1b]11;rgb:0e/11/17\x07\x1b[?62;22c";
        assert_eq!(osc_color(reply, "10"), Some((0xe7, 0xed, 0xf3)));
        assert_eq!(osc_color(reply, "11"), Some((0x0e, 0x11, 0x17)));
        assert_eq!(
            osc_color("\x1b]11;rgb:f/0/8\x07", "11"),
            Some((255, 0, 136))
        );
        assert_eq!(osc_color(reply, "12"), None, "not asked");
        assert_eq!(osc_color("\x1b]11;rgb:zz/11/17\x07", "11"), None);
        assert_eq!(osc_color("\x1b]11;rgb:0e/11\x07", "11"), None);
        let palette = "\x1b]4;1;rgb:f7/3b/61\x1b\\\x1b]4;16;rgb:ffff/9898/5858\x1b\\";
        assert_eq!(
            osc_color(palette, "4;16"),
            Some((255, 152, 88)),
            "a palette color"
        );
        assert_eq!(osc_color(palette, "4;1"), Some((247, 59, 97)));
    }

    #[test]
    fn a_lift_moves_the_background_toward_the_text() {
        let (text, background) = ((231, 234, 238), (29, 34, 40));
        assert_eq!(lift(background, text, 0.08), Color::Rgb(45, 50, 56));
        assert_eq!(lift(background, text, 0.0), Color::Rgb(29, 34, 40));
        // A light theme lifts downward.
        assert_eq!(
            lift((250, 250, 250), (0, 0, 0), 0.1),
            Color::Rgb(225, 225, 225)
        );
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
