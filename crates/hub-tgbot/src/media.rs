//! Which Bot API method an [`hub_core::Attachment`] uses, and the size check
//! that runs before the request.

use std::path::{Path, PathBuf};

use frankenstein::input_file::FileUpload;
use hub_core::{Attachment, AttachmentKind, AttachmentSource};

use crate::error::{Error, Result, TransferDirection};
use crate::limits::{DOWNLOAD_MAX_BYTES, UPLOAD_MAX_BYTES};

/// Bot API method chosen for one attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaMethod {
    Photo,
    Video,
    Audio,
    Voice,
    Document,
    Animation,
}

/// Bytes the caller already has, or a URL `Telegram` should fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparedFile {
    /// Pass the URL through. This crate does not download it.
    Url(String),
    /// Upload the file as multipart.
    Path(PathBuf),
}

pub(crate) fn media_method(attachment: &Attachment) -> Result<MediaMethod> {
    match attachment.kind {
        AttachmentKind::Photo => Ok(MediaMethod::Photo),
        AttachmentKind::Video => Ok(MediaMethod::Video),
        AttachmentKind::Audio => Ok(MediaMethod::Audio),
        AttachmentKind::Voice => Ok(MediaMethod::Voice),
        AttachmentKind::File => Ok(MediaMethod::Document),
        AttachmentKind::Gif => Ok(MediaMethod::Animation),
        // TELEGRAM: a static sticker is a webp image. An animated or video sticker
        // is a tgs or webm file, which sendPhoto rejects, so it goes out as a document.
        AttachmentKind::Sticker => {
            if sticker_is_animated(attachment) {
                Ok(MediaMethod::Document)
            } else {
                Ok(MediaMethod::Photo)
            }
        }
        AttachmentKind::Location
        | AttachmentKind::Poll
        | AttachmentKind::BankCard
        | AttachmentKind::Contact => Err(Error::Permanent { status: None }),
    }
}

pub(crate) fn to_upload(file: PreparedFile) -> FileUpload {
    match file {
        PreparedFile::Url(url) => FileUpload::String(url),
        PreparedFile::Path(path) => FileUpload::from(path),
    }
}

/// Rejects an attachment that is over a named cap, then returns how to send it.
///
/// A URL uses the download cap when `size_bytes` is known. A local path uses the
/// upload cap, from `size_bytes` or from the file length, whichever is checked first.
///
/// # Errors
///
/// Returns [`Error::TooLarge`] when the attachment is over the cap.
/// Returns [`Error::Permanent`] when a local path cannot be read.
pub(crate) async fn prepare_file(attachment: &Attachment) -> Result<PreparedFile> {
    match &attachment.source {
        AttachmentSource::Url(url) => {
            if let Some(size_bytes) = attachment.size_bytes
                && size_bytes > DOWNLOAD_MAX_BYTES
            {
                return Err(Error::TooLarge {
                    direction: TransferDirection::Download,
                    size_bytes,
                    limit_bytes: DOWNLOAD_MAX_BYTES,
                });
            }
            Ok(PreparedFile::Url(url.clone()))
        }
        AttachmentSource::LocalPath(path) => {
            if let Some(size_bytes) = attachment.size_bytes
                && size_bytes > UPLOAD_MAX_BYTES
            {
                return Err(Error::TooLarge {
                    direction: TransferDirection::Upload,
                    size_bytes,
                    limit_bytes: UPLOAD_MAX_BYTES,
                });
            }
            let path_for_meta = path.clone();
            let meta = tokio::task::spawn_blocking(move || std::fs::metadata(&path_for_meta))
                .await
                .map_err(|_| Error::Permanent { status: None })?
                .map_err(|_| Error::Permanent { status: None })?;
            let size_bytes = meta.len();
            if size_bytes > UPLOAD_MAX_BYTES {
                return Err(Error::TooLarge {
                    direction: TransferDirection::Upload,
                    size_bytes,
                    limit_bytes: UPLOAD_MAX_BYTES,
                });
            }
            Ok(PreparedFile::Path(PathBuf::from(path)))
        }
    }
}

fn sticker_is_animated(attachment: &Attachment) -> bool {
    let mime = attachment.mime.as_deref().unwrap_or("");
    if mime == "application/x-tgsticker" || mime == "video/webm" {
        return true;
    }
    let name = attachment.file_name.as_deref().unwrap_or("");
    let source = match &attachment.source {
        AttachmentSource::Url(value) | AttachmentSource::LocalPath(value) => value.as_str(),
    };
    has_extension(name, "tgs")
        || has_extension(name, "webm")
        || has_extension(source, "tgs")
        || has_extension(source, "webm")
}

fn has_extension(value: &str, extension: &str) -> bool {
    let path = match value.split_once('?') {
        Some((path, _)) => path,
        None => value,
    };
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(
        kind: AttachmentKind,
        source: AttachmentSource,
        name: Option<&str>,
    ) -> Attachment {
        Attachment {
            kind,
            mime: None,
            size_bytes: None,
            source,
            file_name: name.map(str::to_owned),
        }
    }

    #[test]
    fn each_kind_picks_one_method() {
        let url = AttachmentSource::Url("https://cdn.example/a".into());
        let cases = [
            (AttachmentKind::Photo, MediaMethod::Photo),
            (AttachmentKind::Video, MediaMethod::Video),
            (AttachmentKind::Audio, MediaMethod::Audio),
            (AttachmentKind::Voice, MediaMethod::Voice),
            (AttachmentKind::File, MediaMethod::Document),
            (AttachmentKind::Gif, MediaMethod::Animation),
        ];
        for (kind, method) in cases {
            assert_eq!(
                media_method(&attachment(kind, url.clone(), None)).unwrap(),
                method
            );
        }
        assert!(media_method(&attachment(AttachmentKind::Poll, url, None)).is_err());
    }

    #[test]
    fn stickers_split_on_animation() {
        let source = AttachmentSource::LocalPath("/tmp/s.webp".into());
        let mut still = attachment(AttachmentKind::Sticker, source, Some("s.webp"));
        still.mime = Some("image/webp".into());
        assert_eq!(media_method(&still).unwrap(), MediaMethod::Photo);

        let mut animated = attachment(
            AttachmentKind::Sticker,
            AttachmentSource::Url("https://cdn.example/s.tgs".into()),
            Some("s.tgs"),
        );
        animated.mime = Some("application/x-tgsticker".into());
        assert_eq!(media_method(&animated).unwrap(), MediaMethod::Document);

        let video = attachment(
            AttachmentKind::Sticker,
            AttachmentSource::LocalPath("/tmp/s.webm".into()),
            None,
        );
        assert_eq!(media_method(&video).unwrap(), MediaMethod::Document);
    }
}
