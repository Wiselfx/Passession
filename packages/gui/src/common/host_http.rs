//! Decoding for core-admitted auxiliary HTTP replies. Routing, authentication,
//! redirects, cancellation and wire size limits remain owned by gui-core.

use base64::Engine;
use serde_json::Value;

pub const IMAGE_MAX_BYTES: usize = 32 * 1024 * 1024;

pub fn image_body(response: &Value) -> Result<(Vec<u8>, String), String> {
    let payload = &response["payload"];
    let invalid = || super::tr("tailnet.invalidHttpResponse");
    let encoded = payload["bodyBase64"].as_str().ok_or_else(invalid)?;
    if encoded.len() > IMAGE_MAX_BYTES.div_ceil(3) * 4 {
        return Err(invalid());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| invalid())?;
    if bytes.len() > IMAGE_MAX_BYTES {
        return Err(invalid());
    }
    Ok((
        bytes,
        payload["contentType"].as_str().unwrap_or("").to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_only_the_correlated_http_payload() {
        let reply =
            serde_json::json!({"payload": {"bodyBase64": "aW1hZ2U=", "contentType": "image/png"}});
        assert_eq!(
            image_body(&reply).unwrap(),
            (b"image".to_vec(), "image/png".to_string())
        );
        assert!(image_body(&serde_json::json!({"bodyBase64": "aW1hZ2U="})).is_err());
        assert!(image_body(&serde_json::json!({"payload": {"bodyBase64": "!invalid"}})).is_err());
    }
}
