use std::{fmt::Write, io, sync::Arc};

use arcdos_transfers::{MAX_CHUNK_BYTES, TransferError};
use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{
            ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
            CONTENT_TYPE, ETAG, IF_RANGE, RANGE, X_CONTENT_TYPE_OPTIONS,
        },
    },
    response::{IntoResponse, Response},
    routing::{get, patch},
};
use futures_util::TryStreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::io::{ReaderStream, StreamReader};
use uuid::Uuid;

use super::{Error, FileApi};

const UPLOAD_OFFSET: HeaderName = HeaderName::from_static("upload-offset");
const READ_BUFFER_BYTES: usize = 64 * 1024;

pub(super) fn router(api: Arc<FileApi>) -> Router {
    Router::new()
        .route(
            "/api/files/v1/{entry_id}/content",
            get(download).head(download),
        )
        .route("/api/files/v1/uploads/{upload_id}", patch(append_upload))
        .with_state(api)
}

async fn download(
    State(api): State<Arc<FileApi>>,
    Path(entry_id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let (file, resource) = match api.run(move |store| store.download(&entry_id)).await {
        Ok(opened) => opened,
        Err(error) => return error_response(error),
    };
    let size = resource.entry.size;
    let tag = etag(&resource.revision);
    let (status, start, length, content_range) =
        match requested_range(&headers, &method, &tag, size) {
            ByteRange::Full => (StatusCode::OK, 0, size, None),
            ByteRange::Partial { start, end } => (
                StatusCode::PARTIAL_CONTENT,
                start,
                end - start + 1,
                Some(format!("bytes {start}-{end}/{size}")),
            ),
            ByteRange::Unsatisfiable => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                0,
                0,
                Some(format!("bytes */{size}")),
            ),
        };

    let body = if method == Method::HEAD || length == 0 {
        Body::empty()
    } else {
        let mut file = tokio::fs::File::from_std(file);
        if start != 0 && file.seek(SeekFrom::Start(start)).await.is_err() {
            return http_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not seek the file");
        }
        Body::from_stream(ReaderStream::with_capacity(
            file.take(length),
            READ_BUFFER_BYTES,
        ))
    };

    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(ETAG, tag);
    if let Some(content_range) = content_range {
        headers.insert(CONTENT_RANGE, generated_header(&content_range));
    }
    if status != StatusCode::RANGE_NOT_SATISFIABLE {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(CONTENT_DISPOSITION, attachment(&resource.name));
        headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    }
    response
}

async fn append_upload(
    State(api): State<Arc<FileApi>>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Ok(id) = Uuid::parse_str(&upload_id) else {
        return http_error(StatusCode::BAD_REQUEST, "Invalid upload ID");
    };
    let Some(offset) = integer_header(&headers, &UPLOAD_OFFSET) else {
        return http_error(StatusCode::BAD_REQUEST, "Invalid Upload-Offset");
    };
    if !headers.contains_key(CONTENT_LENGTH) {
        return http_error(StatusCode::LENGTH_REQUIRED, "Content-Length is required");
    }
    let Some(length) = integer_header(&headers, &CONTENT_LENGTH) else {
        return http_error(StatusCode::BAD_REQUEST, "Invalid Content-Length");
    };
    if length > MAX_CHUNK_BYTES {
        return http_error(StatusCode::PAYLOAD_TOO_LARGE, "Upload chunk is too large");
    }

    let reader = StreamReader::new(body.into_data_stream().map_err(io::Error::other));
    let mut response = match api.uploads.append(id, offset, length, reader).await {
        Ok(upload) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            response
                .headers_mut()
                .insert(UPLOAD_OFFSET, HeaderValue::from(upload.offset));
            response
        }
        Err(error @ TransferError::Offset { expected }) => {
            let mut response = error_response(error.into());
            response
                .headers_mut()
                .insert(UPLOAD_OFFSET, HeaderValue::from(expected));
            response
        }
        Err(error) => return error_response(error.into()),
    };
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn error_response(error: Error) -> Response {
    http_error(error.status(), error.message())
}

fn http_error(status: StatusCode, message: &'static str) -> Response {
    (
        status,
        [(CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        message,
    )
        .into_response()
}

fn single_header<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a HeaderValue> {
    let mut values = headers.get_all(name).into_iter();
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn integer_header(headers: &HeaderMap, name: &HeaderName) -> Option<u64> {
    let value = single_header(headers, name)?.to_str().ok()?;
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

#[derive(Debug, PartialEq, Eq)]
enum ByteRange {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

fn requested_range(
    headers: &HeaderMap,
    method: &Method,
    tag: &HeaderValue,
    size: u64,
) -> ByteRange {
    if method != Method::GET
        || (headers.contains_key(IF_RANGE) && single_header(headers, &IF_RANGE) != Some(tag))
    {
        return ByteRange::Full;
    }
    single_header(headers, &RANGE)
        .and_then(|value| value.to_str().ok())
        .map_or(ByteRange::Full, |value| parse_range(value, size))
}

fn parse_range(value: &str, size: u64) -> ByteRange {
    let Some((unit, range)) = value.split_once('=') else {
        return ByteRange::Full;
    };
    if !unit.eq_ignore_ascii_case("bytes") || range.contains(',') {
        return ByteRange::Full;
    }
    let Some((first, last)) = range.split_once('-') else {
        return ByteRange::Full;
    };
    if first.is_empty() {
        let Some(suffix) = range_number(last) else {
            return ByteRange::Full;
        };
        if size == 0 || suffix == 0 {
            return ByteRange::Unsatisfiable;
        }
        return ByteRange::Partial {
            start: size.saturating_sub(suffix),
            end: size - 1,
        };
    }
    let Some(start) = range_number(first) else {
        return ByteRange::Full;
    };
    let end = if last.is_empty() {
        u64::MAX
    } else if let Some(end) = range_number(last) {
        end
    } else {
        return ByteRange::Full;
    };
    if start > end {
        return ByteRange::Full;
    }
    if start >= size {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Partial {
        start,
        end: end.min(size - 1),
    }
}

// A decimal value can exceed u64. Clamp it before comparing with the file size.
fn range_number(value: &str) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    value.bytes().try_fold(0u64, |number, byte| {
        byte.is_ascii_digit().then(|| {
            number
                .saturating_mul(10)
                .saturating_add(u64::from(byte - b'0'))
        })
    })
}

fn etag(revision: &str) -> HeaderValue {
    let mut value = String::with_capacity(revision.len() * 2 + 2);
    value.push('"');
    for byte in revision.bytes() {
        write!(value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value.push('"');
    generated_header(&value)
}

fn attachment(name: &str) -> HeaderValue {
    let mut value = String::from("attachment; filename*=UTF-8''");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            value.push(char::from(byte));
        } else {
            write!(value, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    generated_header(&value)
}

fn generated_header(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).expect("generated headers use printable ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_open_ended_and_suffix_ranges_are_bounded_by_the_file() {
        assert_eq!(
            parse_range("bytes=2-4", 10),
            ByteRange::Partial { start: 2, end: 4 }
        );
        assert_eq!(
            parse_range("bytes=8-", 10),
            ByteRange::Partial { start: 8, end: 9 }
        );
        assert_eq!(
            parse_range("bytes=-3", 10),
            ByteRange::Partial { start: 7, end: 9 }
        );
        assert_eq!(
            parse_range("bytes=2-999999999999999999999999", 10),
            ByteRange::Partial { start: 2, end: 9 }
        );
        assert_eq!(
            parse_range("bytes=-999999999999999999999999", 10),
            ByteRange::Partial { start: 0, end: 9 }
        );
    }

    #[test]
    fn ranges_distinguish_unsatisfiable_from_unsupported_or_malformed() {
        assert_eq!(parse_range("bytes=10-", 10), ByteRange::Unsatisfiable);
        assert_eq!(
            parse_range("bytes=999999999999999999999999-", 10),
            ByteRange::Unsatisfiable
        );
        assert_eq!(parse_range("bytes=-0", 10), ByteRange::Unsatisfiable);
        assert_eq!(parse_range("bytes=0-0", 0), ByteRange::Unsatisfiable);
        assert_eq!(parse_range("bytes=0-2,4-6", 10), ByteRange::Full);
        assert_eq!(parse_range("other=0-2", 10), ByteRange::Full);
        assert_eq!(parse_range("bytes=8-4", 10), ByteRange::Full);
        assert_eq!(parse_range("bytes=-", 10), ByteRange::Full);
    }

    #[test]
    fn head_ignores_ranges_and_if_range_requires_a_matching_strong_tag() {
        let tag = etag("revision");
        let mut headers = HeaderMap::new();
        headers.insert(RANGE, HeaderValue::from_static("bytes=2-4"));
        headers.insert(IF_RANGE, tag.clone());
        assert_eq!(
            requested_range(&headers, &Method::GET, &tag, 10),
            ByteRange::Partial { start: 2, end: 4 }
        );
        assert_eq!(
            requested_range(&headers, &Method::HEAD, &tag, 10),
            ByteRange::Full
        );
        headers.insert(IF_RANGE, HeaderValue::from_static("W/\"7265766973696f6e\""));
        assert_eq!(
            requested_range(&headers, &Method::GET, &tag, 10),
            ByteRange::Full
        );
        headers.append(IF_RANGE, tag.clone());
        assert_eq!(
            requested_range(&headers, &Method::GET, &tag, 10),
            ByteRange::Full
        );
    }

    #[test]
    fn download_filename_is_encoded_as_one_header_value() {
        assert_eq!(
            attachment("naïve \"x\"\r\n.txt"),
            "attachment; filename*=UTF-8''na%C3%AFve%20%22x%22%0D%0A.txt"
        );
    }

    #[test]
    fn upload_framing_rejects_duplicate_invalid_and_overflowing_integers() {
        let mut headers = HeaderMap::new();
        headers.insert(UPLOAD_OFFSET, HeaderValue::from_static("123"));
        assert_eq!(integer_header(&headers, &UPLOAD_OFFSET), Some(123));
        headers.append(UPLOAD_OFFSET, HeaderValue::from_static("123"));
        assert_eq!(integer_header(&headers, &UPLOAD_OFFSET), None);
        headers.insert(UPLOAD_OFFSET, HeaderValue::from_static("+1"));
        assert_eq!(integer_header(&headers, &UPLOAD_OFFSET), None);
        headers.insert(
            UPLOAD_OFFSET,
            HeaderValue::from_static("18446744073709551616"),
        );
        assert_eq!(integer_header(&headers, &UPLOAD_OFFSET), None);
    }
}
