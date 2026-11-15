use base64::Engine;
use nexo_core::message::ImageSource;
use nexo_core::ToolOutput;

use crate::picture;
/// A picture as the image the model looks at, with one line of text; refused over the size cap,
/// and only described where the model cannot see pictures.
pub(super) fn show_picture(shown: &str, pic: picture::Picture, bytes: &[u8], vision: bool) -> ToolOutput {
    let what = format!("{} image of {}x{} px, {} bytes", pic.label(), pic.width, pic.height, bytes.len());
    if !vision {
        return ToolOutput::ok(format!("{shown}: a {what}; this model cannot view images."));
    }
    if bytes.len() > picture::MAX_BYTES || pic.width.max(pic.height) > picture::MAX_SIDE {
        return ToolOutput::error(format!(
            "read: {shown} is a {what}, too large to show (at most 5 MB and {} px on the long side); \
             shrink a copy first, e.g. with `bash` and `magick in.png -resize 2000x2000 out.png`",
            picture::MAX_SIDE
        ));
    }
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    ToolOutput::ok(format!("{shown}: {what}"))
        .with_images(vec![ImageSource::Base64 { media_type: pic.media_type.into(), data }])
}

/// A PPM, PGM or BMP as a PNG the model looks at; same caps and wording as `show_picture`.
pub(super) fn show_raster(
    shown: &str,
    raster: Result<picture::Raster, String>,
    len: usize,
    vision: bool,
) -> ToolOutput {
    let r = match raster {
        Ok(r) => r,
        Err(why) => {
            return ToolOutput::error(format!(
                "read: {shown} is an image `read` cannot decode ({why}); convert a copy to PNG with `bash`, \
                 e.g. `magick in.img out.png`, and read that"
            ));
        }
    };
    let what = format!("{} image of {}x{} px, {len} bytes", r.label, r.width, r.height);
    if !vision {
        return ToolOutput::ok(format!("{shown}: a {what}; this model cannot view images."));
    }
    let too_large = |extra: &str| {
        ToolOutput::error(format!(
            "read: {shown} is a {what}{extra}, too large to show (at most 5 MB and {} px on the long side); \
             shrink a copy first, e.g. with `bash` and `magick in.img -resize 2000x2000 out.png`",
            picture::MAX_SIDE
        ))
    };
    if r.width.max(r.height) > picture::MAX_SIDE {
        return too_large("");
    }
    let png = match r.to_png() {
        Ok(p) => p,
        Err(e) => return ToolOutput::error(format!("read: {shown} is a {what} that could not be converted: {e}")),
    };
    if png.len() > picture::MAX_BYTES {
        return too_large(&format!(" ({} bytes as PNG)", png.len()));
    }
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    ToolOutput::ok(format!("{shown}: {what}, shown as PNG"))
        .with_images(vec![ImageSource::Base64 { media_type: "image/png".into(), data }])
}
