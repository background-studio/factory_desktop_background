use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FitMode {
    #[default]
    Cover,
    Contain,
    Fill,
    Tile,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplaySettings {
    pub fit: FitMode,
    pub position_x: f64,
    pub position_y: f64,
    pub opacity: f64,
    pub blur: f64,
    pub scale: f64,
    pub overlay_color: String,
    pub overlay_opacity: f64,
    pub home_intensity: f64,
    pub task_intensity: f64,
    pub sidebar_opacity: f64,
    pub surface_opacity: f64,
    pub card_opacity: f64,
    pub composer_opacity: f64,
    pub menu_opacity: f64,
    pub terminal_opacity: f64,
    pub enabled_on_home: bool,
    pub enabled_on_tasks: bool,
    pub video_muted: bool,
    pub video_playback_rate: f64,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            fit: FitMode::Cover,
            position_x: 50.0,
            position_y: 50.0,
            opacity: 0.72,
            blur: 0.0,
            scale: 1.0,
            overlay_color: "#101416".to_string(),
            overlay_opacity: 0.12,
            home_intensity: 1.0,
            task_intensity: 0.32,
            sidebar_opacity: 0.18,
            surface_opacity: 0.12,
            card_opacity: 0.35,
            composer_opacity: 0.88,
            menu_opacity: 0.9,
            terminal_opacity: 0.9,
            enabled_on_home: true,
            enabled_on_tasks: true,
            video_muted: true,
            video_playback_rate: 1.0,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub phase: String,
    pub message: String,
    pub active_targets: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub factory_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl Default for RuntimeStatus {
    fn default() -> Self {
        Self {
            phase: "idle".to_string(),
            message: "尚未配置背景".to_string(),
            active_targets: 0,
            factory_version: None,
            last_error: None,
        }
    }
}

fn object<'a>(value: &'a Value, key: &str) -> Option<&'a serde_json::Map<String, Value>> {
    value.get(key)?.as_object()
}

fn number(map: Option<&serde_json::Map<String, Value>>, key: &str, fallback: f64) -> f64 {
    map.and_then(|value| value.get(key))
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
        .filter(|value| value.is_finite())
        .unwrap_or(fallback)
}

fn boolean(map: Option<&serde_json::Map<String, Value>>, key: &str, fallback: bool) -> bool {
    map.and_then(|value| value.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(fallback)
}

fn clamp(value: f64, minimum: f64, maximum: f64) -> f64 {
    value.clamp(minimum, maximum)
}

impl DisplaySettings {
    pub fn from_value(value: &Value) -> Self {
        let defaults = Self::default();
        let display = if value.get("fit").is_some() || value.get("opacity").is_some() {
            value.as_object()
        } else {
            object(value, "display").or_else(|| value.as_object())
        };
        let fit = display
            .and_then(|map| map.get("fit"))
            .and_then(Value::as_str)
            .and_then(|fit| match fit {
                "cover" => Some(FitMode::Cover),
                "contain" => Some(FitMode::Contain),
                "fill" => Some(FitMode::Fill),
                "tile" => Some(FitMode::Tile),
                _ => None,
            })
            .unwrap_or(defaults.fit);
        let overlay_color = display
            .and_then(|map| map.get("overlayColor"))
            .and_then(Value::as_str)
            .filter(|color| {
                color.len() == 7
                    && color.starts_with('#')
                    && color[1..]
                        .chars()
                        .all(|character| character.is_ascii_hexdigit())
            })
            .map(str::to_ascii_lowercase)
            .unwrap_or(defaults.overlay_color);
        Self {
            fit,
            position_x: clamp(
                number(display, "positionX", defaults.position_x),
                0.0,
                100.0,
            ),
            position_y: clamp(
                number(display, "positionY", defaults.position_y),
                0.0,
                100.0,
            ),
            opacity: clamp(number(display, "opacity", defaults.opacity), 0.0, 1.0),
            blur: clamp(number(display, "blur", defaults.blur), 0.0, 40.0),
            scale: clamp(number(display, "scale", defaults.scale), 1.0, 1.3),
            overlay_color,
            overlay_opacity: clamp(
                number(display, "overlayOpacity", defaults.overlay_opacity),
                0.0,
                0.9,
            ),
            home_intensity: clamp(
                number(display, "homeIntensity", defaults.home_intensity),
                0.0,
                1.0,
            ),
            task_intensity: clamp(
                number(display, "taskIntensity", defaults.task_intensity),
                0.0,
                1.0,
            ),
            sidebar_opacity: clamp(
                number(display, "sidebarOpacity", defaults.sidebar_opacity),
                0.0,
                1.0,
            ),
            surface_opacity: clamp(
                number(display, "surfaceOpacity", defaults.surface_opacity),
                0.0,
                1.0,
            ),
            card_opacity: clamp(
                number(display, "cardOpacity", defaults.card_opacity),
                0.0,
                1.0,
            ),
            composer_opacity: clamp(
                number(display, "composerOpacity", defaults.composer_opacity),
                0.0,
                1.0,
            ),
            menu_opacity: clamp(
                number(display, "menuOpacity", defaults.menu_opacity),
                0.0,
                1.0,
            ),
            terminal_opacity: clamp(
                number(display, "terminalOpacity", defaults.terminal_opacity),
                0.0,
                1.0,
            ),
            enabled_on_home: boolean(display, "enabledOnHome", defaults.enabled_on_home),
            enabled_on_tasks: boolean(display, "enabledOnTasks", defaults.enabled_on_tasks),
            video_muted: boolean(display, "videoMuted", defaults.video_muted),
            video_playback_rate: clamp(
                number(display, "videoPlaybackRate", defaults.video_playback_rate),
                0.25,
                2.0,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_display_and_allows_zero_opacity() {
        let settings = DisplaySettings::from_value(&json!({
            "fit": "invalid",
            "opacity": 9,
            "blur": -4,
            "overlayColor": "red; background:url(x)",
            "sidebarOpacity": 0,
            "surfaceOpacity": 0,
            "cardOpacity": 0,
            "composerOpacity": 0,
            "menuOpacity": 0,
            "terminalOpacity": 0
        }));
        assert_eq!(settings.opacity, 1.0);
        assert_eq!(settings.blur, 0.0);
        assert_eq!(settings.sidebar_opacity, 0.0);
        assert_eq!(settings.fit, FitMode::Cover);
    }
}
