use base64::Engine as _;
use rig::tool::Tool;

use crate::agent::tools::{AskSender, PermCheck, ToolError, check_perm_path};

const MAX_BYTES: u64 = 5 * 1024 * 1024;
const MAX_DIM: u32 = 2048;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReadImageArgs {
    pub path: String,
    /// Short human note shown in the UI pile (optional, never required).
    #[serde(default)]
    pub note: Option<String>,
}

pub struct ReadImageTool {
    pub permission: Option<PermCheck>,
    pub ask_tx: Option<AskSender>,
}

impl ReadImageTool {
    pub fn new(permission: Option<PermCheck>, ask_tx: Option<AskSender>) -> Self {
        Self { permission, ask_tx }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageFormat {
    Jpeg,
    Png,
    Webp,
    Gif,
}

impl ImageFormat {
    fn mime(&self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
            Self::Gif => "GIF",
        }
    }
}

fn detect_format(data: &[u8], path: &str) -> Option<ImageFormat> {
    if data.len() >= 8 && data[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some(ImageFormat::Png);
    }
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return Some(ImageFormat::Jpeg);
    }
    if data.len() >= 6 && (data[0..6] == *b"GIF87a" || data[0..6] == *b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if data.len() >= 12 && data[0..4] == *b"RIFF" && data[8..12] == *b"WEBP" {
        return Some(ImageFormat::Webp);
    }
    // Fallback to extension for edge cases (e.g. truncated sniff window).
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        return Some(ImageFormat::Jpeg);
    }
    if lower.ends_with(".png") {
        return Some(ImageFormat::Png);
    }
    if lower.ends_with(".webp") {
        return Some(ImageFormat::Webp);
    }
    if lower.ends_with(".gif") {
        return Some(ImageFormat::Gif);
    }
    None
}

fn is_animated_gif(data: &[u8]) -> bool {
    // Genuine animated GIFs carry an Application Extension with NETSCAPE2.0
    // or more than one Graphic Control + Image Descriptor pair. Counting
    // Graphic Control Extensions is cheap and catches hand-rolled cases.
    if data.len() < 10 {
        return false;
    }
    if data.windows(11).any(|w| w == b"NETSCAPE2.0") {
        return true;
    }
    // Count Graphic Control Extensions (0x21 0xF9 0x04). Two+ often means animated,
    // but some encoders emit one per frame even for single-frame with transparency,
    // so require >1 Image Descriptors as well.
    let gce = data
        .windows(3)
        .filter(|w| w[0] == 0x21 && w[1] == 0xF9)
        .count();
    let img_desc = data.windows(2).filter(|w| w[0] == 0x2C).count();
    gce > 1 && img_desc > 1
}

fn is_animated_webp(data: &[u8]) -> bool {
    // Animated WebP contains an ANIM chunk.
    data.windows(4).any(|w| w == b"ANIM")
}

impl Tool for ReadImageTool {
    const NAME: &'static str = "read_image";

    type Error = ToolError;
    type Args = ReadImageArgs;
    type Output = String;

    fn description(&self) -> String {
        "Read an image file as an image (not text). Path only. JPEG/PNG/WebP/non-animated GIF, max 5MB and 2048px per side; larger/animated/unsupported returns an error telling the agent to use magick to convert or downscale."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the image file (relative or absolute)" },
                "note": { "type": "string", "description": "Short human note shown in the UI next to this action (optional)" }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, args: ReadImageArgs) -> Result<String, ToolError> {
        crate::engine::ask_freeze::wait_if_frozen().await;
        let path = crate::fs::expand_tilde(&args.path);
        let coaching = check_perm_path(&self.permission, &self.ask_tx, "read_image", &path).await?;

        // Size gate before reading bytes into memory (cheap stat).
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| ToolError::Msg(format!("read_image: cannot stat '{}': {e}", path)))?;
        let len = meta.len();
        if len > MAX_BYTES {
            return Err(ToolError::Msg(format!(
                "read_image: '{}' is {} bytes, exceeds the 5MB limit (5_242_880 bytes). file is too big use magick cli tool to convert it or downscale it — e.g. `magick \"{}\" -resize 2048x2048\\> -quality 85 /tmp/out.jpg` then read that file",
                path, len, path
            )));
        }

        let data = tokio::fs::read(&path)
            .await
            .map_err(|e| ToolError::Msg(format!("read_image: cannot read '{}': {e}", path)))?;
        if (data.len() as u64) > MAX_BYTES {
            return Err(ToolError::Msg(format!(
                "read_image: '{}' is {} bytes, exceeds the 5MB limit. file is too big use magick cli tool to convert it or downscale it — e.g. `magick \"{}\" -resize 2048x2048\\> /tmp/out.jpg`",
                path,
                data.len(),
                path
            )));
        }

        let fmt = detect_format(&data, &path).ok_or_else(|| {
            ToolError::Msg(format!(
                "read_image: '{}' is not a supported image format. Supported: JPEG, PNG, WebP, non-animated GIF. Use magick cli tool to convert it to jpg or png — e.g. `magick \"{}\" /tmp/out.jpg` then read that file",
                path, path
            ))
        })?;

        // Animated check before dimensions (small-file early fail).
        if fmt == ImageFormat::Gif && is_animated_gif(&data) {
            return Err(ToolError::Msg(format!(
                "read_image: '{}' is an animated GIF, which is not supported (stills only). Use magick cli tool to convert it to jpg or png — e.g. `magick \"{}\"[0] /tmp/out.png` (first frame) then read that file",
                path, path
            )));
        }
        if fmt == ImageFormat::Webp && is_animated_webp(&data) {
            return Err(ToolError::Msg(format!(
                "read_image: '{}' is an animated WebP, which is not supported (stills only). Use magick cli tool to convert it to jpg or png — e.g. `magick \"{}\" /tmp/out.jpg` then read that file",
                path, path
            )));
        }

        // Dimensions via lightweight header parse (no image crate).
        let size = imagesize::blob_size(&data).map_err(|e| {
            ToolError::Msg(format!(
                "read_image: '{}' — cannot parse image dimensions ({}). Use magick cli tool to convert it to jpg or png — e.g. `magick \"{}\" /tmp/out.jpg`",
                path, e, path
            ))
        })?;
        let w = size.width as u32;
        let h = size.height as u32;
        if w > MAX_DIM || h > MAX_DIM {
            return Err(ToolError::Msg(format!(
                "read_image: '{}' is {}x{}, exceeds the 2048px max dimension. file is too big use magick cli tool to convert it or downscale it — e.g. `magick \"{}\" -resize 2048x2048\\> /tmp/out.jpg` then read that file",
                path, w, h, path
            )));
        }

        let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
        let mime = fmt.mime();
        let label = fmt.label();

        let response = format!(
            "Image '{}' ({} {}x{}, {} bytes) — base64 follows in the image channel.",
            path,
            label,
            w,
            h,
            data.len()
        );
        let mut out = serde_json::json!({
            "response": response,
            "parts": [{ "type": "image", "data": b64, "mimeType": mime }]
        })
        .to_string();
        if let Some(msg) = coaching {
            // Prepend coaching so the LLM sees it above the image note, like other tools.
            out = format!("{}\n{}", msg, out);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1x1 transparent PNG (68 bytes) — imagesize parses it as 1x1.
    const PNG_1X1_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";

    fn png_1x1_bytes() -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(PNG_1X1_B64)
            .unwrap()
    }

    #[test]
    fn detect_format_png_by_magic() {
        let data = png_1x1_bytes();
        assert_eq!(detect_format(&data, "foo.bin"), Some(ImageFormat::Png));
    }

    #[test]
    fn detect_format_jpeg_by_magic() {
        let mut data = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        assert_eq!(detect_format(&data, "x"), Some(ImageFormat::Jpeg));
        // Extension fallback
        data = vec![0x00, 0x01];
        assert_eq!(detect_format(&data, "photo.jpg"), Some(ImageFormat::Jpeg));
    }

    #[test]
    fn detect_format_unsupported() {
        assert_eq!(detect_format(b"hello world", "doc.txt"), None);
    }

    #[test]
    fn blob_size_png_1x1() {
        let data = png_1x1_bytes();
        let sz = imagesize::blob_size(&data).unwrap();
        assert_eq!(sz.width, 1);
        assert_eq!(sz.height, 1);
    }

    #[test]
    fn mime_mapping() {
        assert_eq!(ImageFormat::Jpeg.mime(), "image/jpeg");
        assert_eq!(ImageFormat::Png.mime(), "image/png");
        assert_eq!(ImageFormat::Webp.mime(), "image/webp");
        assert_eq!(ImageFormat::Gif.mime(), "image/gif");
    }

    #[test]
    fn animated_gif_netscape_detected() {
        // Minimal NETSCAPE2.0 marker makes it animated.
        let mut data = b"GIF89a".to_vec();
        data.extend_from_slice(b"NETSCAPE2.0 extra");
        assert!(is_animated_gif(&data));
    }

    #[test]
    fn still_gif_not_animated() {
        let data = png_1x1_bytes();
        // PNG bytes are not gif, so not animated.
        assert!(!is_animated_gif(&data));
        // Single-frame GIF header without NETSCAPE and without double GCE: not animated.
        let gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00".to_vec();
        assert!(!is_animated_gif(&gif));
    }

    #[test]
    fn animated_webp_anim_chunk() {
        let data = b"RIFF....WEBPANIM....".to_vec();
        assert!(is_animated_webp(&data));
        assert!(!is_animated_webp(&png_1x1_bytes()));
    }

    #[tokio::test]
    async fn tool_rejects_oversize_and_unsupported() {
        // Unsupported format path: write a text file and call the tool.
        let tmp = std::env::temp_dir().join("zs_read_image_unsupported.txt");
        std::fs::write(&tmp, b"not an image").unwrap();
        let tool = ReadImageTool::new(None, None);
        let err = tool
            .call(ReadImageArgs {
                path: tmp.to_string_lossy().to_string(),
                note: None,
            })
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not a supported image format"), "{msg}");
        assert!(msg.contains("magick"), "{msg}");
        let _ = std::fs::remove_file(&tmp);

        // Oversize: create a temp file with 5MB+1 bytes of PNG header + padding.
        let tmp2 = std::env::temp_dir().join("zs_read_image_big.png");
        let mut big = vec![0u8; (MAX_BYTES + 1) as usize];
        big[0..8].copy_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        std::fs::write(&tmp2, &big).unwrap();
        let err2 = tool
            .call(ReadImageArgs {
                path: tmp2.to_string_lossy().to_string(),
                note: None,
            })
            .await
            .unwrap_err();
        let msg2 = err2.to_string();
        assert!(msg2.contains("exceeds the 5MB limit"), "{msg2}");
        assert!(msg2.contains("magick"), "{msg2}");
        let _ = std::fs::remove_file(&tmp2);
    }

    #[tokio::test]
    async fn tool_succeeds_png_envelope_is_image_channel() {
        let tmp = std::env::temp_dir().join("zs_read_image_ok.png");
        std::fs::write(&tmp, png_1x1_bytes()).unwrap();
        let tool = ReadImageTool::new(None, None);
        let out = tool
            .call(ReadImageArgs {
                path: tmp.to_string_lossy().to_string(),
                note: None,
            })
            .await
            .unwrap();
        // Must be JSON envelope parsed as ToolResultContent::Image, not plain text base64.
        let v: serde_json::Value = serde_json::from_str(&out).expect("envelope is JSON");
        assert!(v.get("parts").is_some(), "parts missing: {v}");
        assert_eq!(v["parts"][0]["type"], "image");
        assert_eq!(v["parts"][0]["mimeType"], "image/png");
        // Rig parses this into ToolResultContent::Image.
        let parsed = rig::message::ToolResultContent::from_tool_output(out);
        assert!(
            parsed
                .iter()
                .any(|c| matches!(c, rig::message::ToolResultContent::Image(_))),
            "rig did not parse image part"
        );
        let _ = std::fs::remove_file(&tmp);
    }
}
