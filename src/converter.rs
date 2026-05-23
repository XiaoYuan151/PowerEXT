use image::AnimationDecoder;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use std::sync::OnceLock;
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_data_dir(p: PathBuf) { let _ = DATA_DIR.set(p); }

fn find_tool(name: &str) -> Option<PathBuf> {
    if let Some(dd) = DATA_DIR.get() {
        if let Some(p) = crate::tools::tool_path(name, dd) {
            return Some(p);
        }
    }
    // fallback: system PATH
    let cmd = if cfg!(target_os = "windows") { "where" } else { "which" };
    std::process::Command::new(cmd).arg(name).output().ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| PathBuf::from(s.trim()))
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum ConvertError {
    UnsupportedPair,
    ImageError(image::ImageError),
    ToolNotFound(&'static str),
    ToolFailed(String),
    IoError(std::io::Error),
}

impl From<image::ImageError> for ConvertError {
    fn from(e: image::ImageError) -> Self { Self::ImageError(e) }
}
impl From<std::io::Error> for ConvertError {
    fn from(e: std::io::Error) -> Self { Self::IoError(e) }
}

pub enum ConvertOutput {
    SingleFile(Vec<u8>),
    FrameDir,
    ExternalFile(PathBuf),
}

// ── routing ──────────────────────────────────────────────────────────────────

const VIDEO_EXTS:  &[&str] = &["mp4", "webm", "avi", "mov", "mkv", "flv"];
const IMAGE_EXTS:  &[&str] = &["png", "jpg", "jpeg", "bmp", "tiff", "tif", "ico", "qoi"];
const ANIM_EXTS:   &[&str] = &["gif", "webp"];
const OFFICE_EXTS: &[&str] = &["docx", "doc", "odt", "pptx", "ppt", "odp",
                                "xlsx", "xls", "ods", "md", "rtf", "txt", "pdf"];

pub fn dispatch(src: &Path, from_ext: &str, to_ext: &str, dest_dir: &Path) -> Result<ConvertOutput, ConvertError> {
    let f = from_ext.to_ascii_lowercase();
    let t = to_ext.to_ascii_lowercase();

    // image → image (single frame)
    if IMAGE_EXTS.contains(&f.as_str()) && (IMAGE_EXTS.contains(&t.as_str()) || ANIM_EXTS.contains(&t.as_str())) {
        return Ok(ConvertOutput::SingleFile(convert(src, &f, &t)?));
    }

    // animated gif/webp → image frames
    if ANIM_EXTS.contains(&f.as_str()) && IMAGE_EXTS.contains(&t.as_str()) {
        std::fs::create_dir_all(dest_dir)?;
        extract_frames(src, &f, dest_dir, &t)?;
        return Ok(ConvertOutput::FrameDir);
    }

    // video → image frames
    if VIDEO_EXTS.contains(&f.as_str()) && IMAGE_EXTS.contains(&t.as_str()) {
        std::fs::create_dir_all(dest_dir)?;
        extract_frames(src, &f, dest_dir, &t)?;
        return Ok(ConvertOutput::FrameDir);
    }

    // pdf → image frames
    if f == "pdf" && IMAGE_EXTS.contains(&t.as_str()) {
        std::fs::create_dir_all(dest_dir)?;
        pdf_to_frames(src, dest_dir, &t)?;
        return Ok(ConvertOutput::FrameDir);
    }

    // office / md mutual conversion
    if OFFICE_EXTS.contains(&f.as_str()) && OFFICE_EXTS.contains(&t.as_str()) {
        let out = soffice_convert(src, &t, dest_dir)?;
        return Ok(ConvertOutput::ExternalFile(out));
    }

    Err(ConvertError::UnsupportedPair)
}

// ── image sequence merge ──────────────────────────────────────────────────────

pub fn frames_to_animated(frames: &[PathBuf], to_ext: &str, dest: &Path) -> Result<(), ConvertError> {
    match to_ext {
        "gif" => {
            let file = std::fs::File::create(dest)?;
            let mut enc = image::codecs::gif::GifEncoder::new(file);
            for path in frames {
                let img = image::open(path)?.into_rgba8();
                enc.encode_frame(image::Frame::new(img))?;
            }
        }
        "webp" => {
            // encode each frame as a separate webp; animated webp requires libwebp
            // fall back to saving first frame only with a warning
            tracing::warn!("animated WEBP not supported by image crate; saving first frame only");
            let bytes = convert(&frames[0], frames[0].extension().and_then(|e| e.to_str()).unwrap_or("png"), "webp")?;
            std::fs::write(dest, bytes)?;
        }
        _ => return Err(ConvertError::UnsupportedPair),
    }
    Ok(())
}

pub fn frames_to_video(frames: &[PathBuf], to_ext: &str, dest: &Path) -> Result<(), ConvertError> {
    let ffmpeg = find_tool("ffmpeg").ok_or(ConvertError::ToolNotFound("ffmpeg"))?;

    // write concat list to a temp file next to dest
    let list_path = dest.with_extension("_concat.txt");
    let list_content: String = frames.iter()
        .map(|p| format!("file '{}'\n", p.display()))
        .collect();
    std::fs::write(&list_path, list_content)?;

    let codec = match to_ext {
        "webm" => &["-c:v", "libvpx-vp9", "-b:v", "0", "-crf", "30"] as &[&str],
        "avi"  => &["-c:v", "mpeg4"],
        _      => &["-c:v", "libx264", "-pix_fmt", "yuv420p"],
    };

    let mut cmd = std::process::Command::new(&ffmpeg);
    cmd.args(["-y", "-f", "concat", "-safe", "0", "-i"])
       .arg(&list_path);
    cmd.args(codec);
    cmd.arg(dest);
    let out = cmd.output()?;
    let _ = std::fs::remove_file(&list_path);
    if !out.status.success() {
        return Err(ConvertError::ToolFailed(String::from_utf8_lossy(&out.stderr).into_owned()));
    }
    Ok(())
}

// ── frame extraction ──────────────────────────────────────────────────────────

pub fn extract_frames(src: &Path, from_ext: &str, dest_dir: &Path, to_ext: &str) -> Result<(), ConvertError> {
    if ANIM_EXTS.contains(&from_ext) {
        // use image crate
        let bytes = std::fs::read(src)?;
        let frames: Vec<_> = match from_ext {
            "gif" => image::codecs::gif::GifDecoder::new(Cursor::new(&bytes))?
                .into_frames().collect_frames()?,
            _ => {
                // webp: fall back to ffmpeg
                return extract_frames_ffmpeg(src, dest_dir, to_ext);
            }
        };
        for (i, frame) in frames.iter().enumerate() {
            let out = dest_dir.join(format!("frame_{:04}.{}", i + 1, to_ext));
            frame.buffer().save(&out).map_err(ConvertError::ImageError)?;
        }
        return Ok(());
    }
    extract_frames_ffmpeg(src, dest_dir, to_ext)
}

fn extract_frames_ffmpeg(src: &Path, dest_dir: &Path, to_ext: &str) -> Result<(), ConvertError> {
    let ffmpeg = find_tool("ffmpeg").ok_or(ConvertError::ToolNotFound("ffmpeg"))?;
    let pattern = dest_dir.join(format!("frame_%04d.{}", to_ext));
    let out = std::process::Command::new(&ffmpeg)
        .args(["-y", "-i"]).arg(src).arg(&pattern)
        .output()?;
    if !out.status.success() {
        return Err(ConvertError::ToolFailed(String::from_utf8_lossy(&out.stderr).into_owned()));
    }
    Ok(())
}

pub fn pdf_to_frames(src: &Path, dest_dir: &Path, to_ext: &str) -> Result<(), ConvertError> {
    let doc = mupdf::Document::open(src.to_str().ok_or(ConvertError::UnsupportedPair)?)
        .map_err(|e| ConvertError::ToolFailed(e.to_string()))?;
    let matrix = mupdf::Matrix::new_scale(150.0 / 72.0, 150.0 / 72.0);
    let n = doc.page_count().map_err(|e| ConvertError::ToolFailed(e.to_string()))?;
    for i in 0..n {
        let page = doc.load_page(i).map_err(|e| ConvertError::ToolFailed(e.to_string()))?;
        let pixmap = page.to_pixmap(&matrix, &mupdf::Colorspace::device_rgb(), false, true)
            .map_err(|e| ConvertError::ToolFailed(e.to_string()))?;
        let w = pixmap.width();
        let h = pixmap.height();
        let samples = pixmap.samples().to_vec();
        let img = image::RgbImage::from_raw(w, h, samples)
            .ok_or(ConvertError::UnsupportedPair)?;
        let out = dest_dir.join(format!("frame_{:04}.{}", i + 1, to_ext));
        img.save(&out).map_err(ConvertError::ImageError)?;
    }
    Ok(())
}

// ── office conversion ─────────────────────────────────────────────────────────

pub fn soffice_convert(src: &Path, to_ext: &str, dest_dir: &Path) -> Result<PathBuf, ConvertError> {
    let soffice = find_tool("soffice").ok_or(ConvertError::ToolNotFound("soffice"))?;
    std::fs::create_dir_all(dest_dir)?;
    let out = std::process::Command::new(&soffice)
        .args(["--headless", "--convert-to", to_ext, "--outdir"])
        .arg(dest_dir).arg(src)
        .output()?;
    if !out.status.success() {
        return Err(ConvertError::ToolFailed(String::from_utf8_lossy(&out.stderr).into_owned()));
    }
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    Ok(dest_dir.join(format!("{}.{}", stem, to_ext)))
}

// ── single-image conversion (unchanged logic) ─────────────────────────────────

pub fn convert(path: &Path, from_ext: &str, to_ext: &str) -> Result<Vec<u8>, ConvertError> {
    let src_fmt = image::ImageFormat::from_extension(from_ext)
        .ok_or(ConvertError::UnsupportedPair)?;
    let dst_fmt = image::ImageFormat::from_extension(to_ext)
        .ok_or(ConvertError::UnsupportedPair)?;
    let bytes = std::fs::read(path)?;
    let img = image::load(Cursor::new(bytes), src_fmt)?;
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, dst_fmt)?;
    Ok(buf.into_inner())
}

