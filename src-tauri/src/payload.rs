use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::models::{DisplaySettings, MediaKind};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/payload_assets.rs"));
}

const REVIEW_SHADOW_STYLE_ID: &str = "factory-background-review-shadow-style";
const MAX_INLINE_MEDIA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EARLY_INLINE_MEDIA_BYTES: usize = 192 * 1024;
const MAX_EARLY_SCRIPT_BYTES: usize = 400 * 1024;
const MEDIA_URL_SENTINEL: &str = "background-studio-media://pending";
pub const PENDING_MEDIA_URL_KEY: &str = "__BACKGROUND_STUDIO_PENDING_MEDIA_URL__";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RevisionInput<'a> {
    sha256: &'a str,
    display: &'a DisplaySettings,
    kind: &'a MediaKind,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PayloadConfig<'a> {
    media_url: &'a str,
    media_kind: &'a MediaKind,
    display: &'a DisplaySettings,
    revision: &'a str,
}

#[derive(Clone)]
pub struct ActivePayload {
    pub script: String,
    pub revision: String,
    pub media_bytes: Arc<[u8]>,
    pub media_mime_type: String,
    pub early_script: Option<String>,
}

fn digest(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    format!("{:x}", hasher.finalize())
}

fn render_script(
    media_url: &str,
    media_kind: &MediaKind,
    display: &DisplaySettings,
    payload_revision: &str,
) -> Result<String, String> {
    let serialized = serde_json::to_string(&PayloadConfig {
        media_url,
        media_kind,
        display,
        revision: payload_revision,
    })
    .map_err(|error| error.to_string())?
    .replace('<', "\\u003c");
    let css =
        serde_json::to_string(generated::BACKGROUND_CSS).map_err(|error| error.to_string())?;
    let review_css =
        serde_json::to_string(generated::REVIEW_SHADOW_CSS).map_err(|error| error.to_string())?;
    let review_style_id =
        serde_json::to_string(REVIEW_SHADOW_STYLE_ID).map_err(|error| error.to_string())?;
    Ok(generated::PAYLOAD_TEMPLATE
        .replace("${serialized}", &serialized)
        .replace("${css}", &css)
        .replace("${reviewShadowCss}", &review_css)
        .replace("${reviewShadowStyleId}", &review_style_id))
}

pub fn build_active_payload_from_bytes(
    bytes: Vec<u8>,
    kind: &MediaKind,
    mime_type: &str,
    display: &DisplaySettings,
) -> Result<ActivePayload, String> {
    if bytes.len() as u64 > MAX_INLINE_MEDIA_BYTES {
        return Err("背景媒体超过 64 MB 内嵌上限，请选择更小的文件。".to_string());
    }
    let file_digest = {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    };
    let revision_input = serde_json::to_vec(&RevisionInput {
        sha256: &file_digest,
        display,
        kind,
    })
    .map_err(|error| error.to_string())?;
    let revision = digest(&[&revision_input]);
    let payload_revision = digest(&[
        revision.as_bytes(),
        generated::BACKGROUND_CSS.as_bytes(),
        generated::REVIEW_SHADOW_CSS.as_bytes(),
    ]);
    let sentinel_literal =
        serde_json::to_string(MEDIA_URL_SENTINEL).map_err(|error| error.to_string())?;
    let pending_expression = format!(
        "window[{}]",
        serde_json::to_string(PENDING_MEDIA_URL_KEY).map_err(|error| error.to_string())?
    );
    let inline_script = render_script(MEDIA_URL_SENTINEL, kind, display, &payload_revision)?;
    if !inline_script.contains(&sentinel_literal) {
        return Err("背景媒体占位符生成失败。".to_string());
    }
    let script = inline_script.replacen(&sentinel_literal, &pending_expression, 1);
    let early_script = if bytes.len() <= MAX_EARLY_INLINE_MEDIA_BYTES {
        let media_url = format!("data:{mime_type};base64,{}", STANDARD.encode(&bytes));
        let candidate = render_script(&media_url, kind, display, &payload_revision)?;
        (candidate.len() <= MAX_EARLY_SCRIPT_BYTES).then_some(candidate)
    } else {
        None
    };
    Ok(ActivePayload {
        script,
        revision: payload_revision,
        media_bytes: Arc::from(bytes),
        media_mime_type: mime_type.to_string(),
        early_script,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::MediaKind;

    #[test]
    fn builds_payload_from_canonical_typescript_resource() {
        let payload = build_active_payload_from_bytes(
            b"payload bytes".to_vec(),
            &MediaKind::Image,
            "image/png",
            &DisplaySettings::default(),
        )
        .unwrap();
        assert!(payload.script.contains("factory-background-layer"));
        assert!(payload.script.contains("__FACTORY_BACKGROUND_STUDIO__"));
        assert!(payload.script.contains("factory-background-probe"));
        // 令牌覆盖必须排除探针，探针才能读到 Factory 当前主题的原始实色。
        assert!(payload
            .script
            .contains(r#":not(html):not(body):not([data-fbg-probe])"#));
        assert!(payload
            .script
            .contains("--core-v2-bg-page: transparent !important"));
        assert!(payload
            .script
            .contains("--text-inverted: var(--fbg-solid-page)"));
        assert!(payload
            .script
            .contains(r#"[data-windows-content-frame=\"true\"]"#));
        assert!(payload.script.contains("border-image: linear-gradient("));
        assert!(payload.script.contains("[data-windows-sidebar]"));
        assert!(payload
            .script
            .contains("[data-pinned-user-message-overlay]"));
        assert!(payload
            .script
            .contains(r#"[data-testid=\"chat-composer-wrapper\"]"#));
        assert!(!payload.script.contains("windowControlsOverlay"));
        assert!(payload.script.contains(PENDING_MEDIA_URL_KEY));
        assert!(!payload.script.contains("data:image/png;base64,"));
        assert!(payload
            .early_script
            .as_deref()
            .is_some_and(|script| script.contains("data:image/png;base64,")));
        assert_eq!(payload.media_bytes.as_ref(), b"payload bytes");
        assert_eq!(payload.revision.len(), 64);
    }

    #[test]
    fn keeps_large_media_out_of_cdp_script() {
        let bytes = vec![0x5a; 1024 * 1024];
        let payload = build_active_payload_from_bytes(
            bytes.clone(),
            &MediaKind::Image,
            "image/png",
            &DisplaySettings::default(),
        )
        .unwrap();
        assert_eq!(payload.media_bytes.len(), bytes.len());
        assert!(payload.early_script.is_none());
        assert!(payload.script.len() < MAX_EARLY_SCRIPT_BYTES);
        assert!(!payload.script.contains("data:image/png;base64,"));
    }
}
