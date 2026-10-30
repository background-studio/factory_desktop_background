use serde::Serialize;
use serde_json::{json, Value};

pub const PLUGIN_PROTOCOL: u32 = 2;
pub const PLUGIN_ID: &str = "factory";
pub const PIPE_NAME: &str = r"\\.\pipe\background-studio-factory";
pub const SCHEMA_VERSION: u32 = 1;

pub fn runtime_pipe_name() -> Result<String, String> {
    #[cfg(feature = "integration-test-pipe")]
    if let Some(value) = std::env::var_os("BACKGROUND_STUDIO_TEST_PIPE") {
        let value = value
            .into_string()
            .map_err(|_| "测试管道名称不是有效 UTF-8。".to_string())?;
        if !value.starts_with(r"\\.\pipe\background-studio-test-") || value.len() > 160 {
            return Err("测试管道名称无效。".to_string());
        }
        return Ok(value);
    }
    Ok(PIPE_NAME.to_string())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCapabilities {
    pub commands: [&'static str; 7],
    pub media_kinds: [&'static str; 2],
    pub auto_takeover: bool,
    pub hot_update: bool,
    pub keeps_target_on_shutdown: bool,
    pub max_media_bytes: u64,
}

pub const CAPABILITIES: PluginCapabilities = PluginCapabilities {
    commands: [
        "hello",
        "configure",
        "status",
        "apply",
        "pause",
        "restore",
        "shutdown",
    ],
    media_kinds: ["image", "video"],
    auto_takeover: true,
    hot_update: true,
    keeps_target_on_shutdown: true,
    max_media_bytes: crate::protocol::MAX_MEDIA_BYTES,
};

pub fn hello_result() -> Value {
    json!({
        "pluginProtocol": PLUGIN_PROTOCOL,
        "pluginId": PLUGIN_ID,
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": CAPABILITIES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_matches_runtime_and_exposes_display_schema() {
        let manifest: Value =
            serde_json::from_str(include_str!("../../plugin.json")).expect("plugin.json");
        assert_eq!(manifest["pluginProtocol"], PLUGIN_PROTOCOL);
        assert_eq!(manifest["id"], PLUGIN_ID);
        assert_eq!(manifest["pipeName"], PIPE_NAME);
        assert_eq!(manifest["exeName"], "Factory Background Studio.exe");
        assert!(manifest["capabilities"].get("transparentWco").is_none());
        assert!(manifest["settingsSchema"]["properties"]
            .get("blockFillOpacity")
            .is_none());
        assert_eq!(
            manifest["capabilities"]["maxMediaBytes"],
            crate::protocol::MAX_MEDIA_BYTES
        );
        assert_eq!(
            manifest["settingsSchema"]["properties"]["cardOpacity"]["type"],
            "number"
        );
    }
}
