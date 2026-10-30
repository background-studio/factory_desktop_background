use std::{net::SocketAddr, time::Duration};

use reqwest::{
    header::{CONTENT_LENGTH, CONTENT_TYPE},
    redirect::Policy,
};
use tokio::net::lookup_host;

use crate::protocol::{
    hex_sha256, ip_is_loopback, validate_configure_url, validate_kind_mime, ConfigureSpec,
    MAX_MEDIA_BYTES,
};

pub async fn download_configured_media(spec: &ConfigureSpec) -> Result<Vec<u8>, String> {
    let url = validate_configure_url(&spec.media.url)?;
    let hostname = url
        .host_str()
        .ok_or_else(|| "媒体地址缺少主机。".to_string())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "端口无效。".to_string())?;
    let mut addresses = Vec::new();
    for address in lookup_host((hostname, port))
        .await
        .map_err(|error| format!("本机地址解析失败：{error}"))?
    {
        if !ip_is_loopback(address.ip()) {
            return Err("媒体地址解析到了非回环地址。".to_string());
        }
        addresses.push(SocketAddr::new(address.ip(), port));
    }
    if addresses.is_empty() {
        return Err("媒体地址没有可用的回环地址。".to_string());
    }

    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .resolve_to_addrs(hostname, &addresses)
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(url.clone())
        .header("User-Agent", "Factory-Background-Studio/0.2")
        .header("Accept", "image/*,video/*;q=0.9")
        .send()
        .await
        .map_err(|error| format!("下载连接失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "下载失败，服务器返回 HTTP {}。",
            response.status().as_u16()
        ));
    }
    if let Some(length) = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        if length > MAX_MEDIA_BYTES {
            return Err("媒体超过 64 MB 上限。".to_string());
        }
        if length != spec.media.byte_size {
            return Err("Content-Length 与声明大小不一致。".to_string());
        }
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "响应缺少有效的 Content-Type。".to_string())?;
    if content_type != spec.media.mime_type {
        return Err("Content-Type 与声明 mimeType 不一致。".to_string());
    }
    validate_kind_mime(&spec.media.kind, &content_type)?;

    let mut bytes = Vec::new();
    let mut stream = response;
    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|error| format!("下载中断：{error}"))?
    {
        let next = bytes.len() as u64 + chunk.len() as u64;
        if next > MAX_MEDIA_BYTES || next > spec.media.byte_size {
            return Err("媒体超过声明大小或 64 MB 上限。".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    verify_media_bytes(&bytes, spec)?;
    Ok(bytes)
}

pub fn verify_media_bytes(bytes: &[u8], spec: &ConfigureSpec) -> Result<(), String> {
    if bytes.len() as u64 != spec.media.byte_size {
        return Err("下载大小与 byteSize 不一致。".to_string());
    }
    if bytes.len() as u64 > MAX_MEDIA_BYTES {
        return Err("媒体超过 64 MB 上限。".to_string());
    }
    validate_kind_mime(&spec.media.kind, &spec.media.mime_type)?;
    let digest = hex_sha256(bytes);
    if digest != spec.media.sha256 {
        return Err("sha256 与声明值不一致。".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DisplaySettings, MediaKind};
    use crate::protocol::{hex_sha256, parse_configure, MediaSpec};
    use serde_json::json;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;

    fn spec_for(bytes: &[u8], url: String) -> ConfigureSpec {
        ConfigureSpec {
            revision: "r1".to_string(),
            media: MediaSpec {
                url,
                kind: MediaKind::Image,
                mime_type: "image/png".to_string(),
                sha256: hex_sha256(bytes),
                byte_size: bytes.len() as u64,
            },
            display: DisplaySettings::default(),
        }
    }

    fn serve_once(status: &str, headers: &str, body: Vec<u8>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let status = status.to_string();
        let headers = headers.to_string();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut request);
            let response = format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n");
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        port
    }

    #[test]
    fn verifies_hash_size_and_kind() {
        let bytes = b"png-bytes";
        let spec = spec_for(bytes, "http://127.0.0.1:9/x".to_string());
        assert!(verify_media_bytes(bytes, &spec).is_ok());
        let mut bad = spec.clone();
        bad.media.sha256 = "b".repeat(64);
        assert!(verify_media_bytes(bytes, &bad).is_err());
        bad = spec.clone();
        bad.media.byte_size = 1;
        assert!(verify_media_bytes(bytes, &bad).is_err());
        bad = spec;
        bad.media.kind = MediaKind::Video;
        assert!(verify_media_bytes(bytes, &bad).is_err());
    }

    #[tokio::test]
    async fn downloads_loopback_and_checks_headers() {
        let body = b"fake-png".to_vec();
        let port = serve_once(
            "200 OK",
            &format!(
                "Content-Type: image/png\r\nContent-Length: {}\r\n",
                body.len()
            ),
            body.clone(),
        );
        let spec = spec_for(&body, format!("http://127.0.0.1:{port}/file.png"));
        let downloaded = download_configured_media(&spec).await.unwrap();
        assert_eq!(downloaded, body);
    }

    #[tokio::test]
    async fn rejects_content_length_mismatch() {
        let body = b"fake-png".to_vec();
        let port = serve_once(
            "200 OK",
            "Content-Type: image/png\r\nContent-Length: 3\r\n",
            body.clone(),
        );
        let spec = spec_for(&body, format!("http://127.0.0.1:{port}/file.png"));
        assert!(download_configured_media(&spec).await.is_err());
    }

    #[tokio::test]
    async fn rejects_content_type_mismatch() {
        let body = b"fake-png".to_vec();
        let port = serve_once(
            "200 OK",
            &format!(
                "Content-Type: video/mp4\r\nContent-Length: {}\r\n",
                body.len()
            ),
            body.clone(),
        );
        let spec = spec_for(&body, format!("http://127.0.0.1:{port}/file.png"));
        assert!(download_configured_media(&spec).await.is_err());
    }

    #[test]
    fn parse_and_verify_roundtrip() {
        let bytes = b"abc";
        let spec = parse_configure(&json!({
            "schemaVersion": 1,
            "revision": "r",
            "media": {
                "url": "http://127.0.0.1:1/x",
                "kind": "image",
                "mimeType": "image/png",
                "sha256": hex_sha256(bytes),
                "byteSize": 3
            },
            "display": {}
        }))
        .unwrap();
        assert!(verify_media_bytes(bytes, &spec).is_ok());
    }
}
