//! OHOS system clipboard capability via the NDK C API.
//!
//! Wraps `libpasteboard` (`OH_Pasteboard_*`) and `libudmf` (`OH_Udmf*`) so Rust
//! can read and write the system clipboard directly, without the ArkTS
//! `@ohos.pasteboard` permission gate. Writing needs no permission; reading
//! mirrors what warp-ohos does (`OH_Pasteboard_GetData`) and does not depend on
//! `ohos.permission.READ_PASTEBOARD`.
//!
//! Three flavours travel: plain text, HTML, and encoded images. Images are
//! stored as `general.*` general entries, which carry the encoded file bytes —
//! UDMF's `OH_UdsPixelMap` would carry decoded pixels instead, and would need an
//! image codec on both sides of every paste.

use std::ffi::{c_char, c_int, c_uchar, c_uint, c_void, CStr, CString};
use std::slice;

/// libpasteboard.so (API 13+).
#[link(name = "pasteboard")]
unsafe extern "C" {
    fn OH_Pasteboard_Create() -> *mut c_void;
    fn OH_Pasteboard_Destroy(pasteboard: *mut c_void);
    fn OH_Pasteboard_GetData(pasteboard: *mut c_void, status: *mut c_int) -> *mut c_void;
    fn OH_Pasteboard_SetData(pasteboard: *mut c_void, data: *mut c_void) -> c_int;
}

/// libudmf.so (API 12+).
#[link(name = "udmf")]
unsafe extern "C" {
    fn OH_UdsPlainText_Create() -> *mut c_void;
    fn OH_UdsPlainText_Destroy(plain_text: *mut c_void);
    fn OH_UdsPlainText_GetContent(plain_text: *mut c_void) -> *const c_char;
    fn OH_UdsPlainText_SetContent(plain_text: *mut c_void, content: *const c_char) -> c_int;

    fn OH_UdsHtml_Create() -> *mut c_void;
    fn OH_UdsHtml_Destroy(html: *mut c_void);
    fn OH_UdsHtml_GetContent(html: *mut c_void) -> *const c_char;
    fn OH_UdsHtml_SetContent(html: *mut c_void, content: *const c_char) -> c_int;

    fn OH_UdmfRecord_Create() -> *mut c_void;
    fn OH_UdmfRecord_Destroy(record: *mut c_void);
    fn OH_UdmfRecord_AddPlainText(record: *mut c_void, plain_text: *mut c_void) -> c_int;
    fn OH_UdmfRecord_AddHtml(record: *mut c_void, html: *mut c_void) -> c_int;
    fn OH_UdmfRecord_GetHtml(record: *mut c_void, html: *mut c_void) -> c_int;
    fn OH_UdmfRecord_AddGeneralEntry(
        record: *mut c_void,
        type_id: *const c_char,
        entry: *mut c_uchar,
        count: c_uint,
    ) -> c_int;
    fn OH_UdmfRecord_GetGeneralEntry(
        record: *mut c_void,
        type_id: *const c_char,
        entry: *mut *mut c_uchar,
        count: *mut c_uint,
    ) -> c_int;

    fn OH_UdmfData_Create() -> *mut c_void;
    fn OH_UdmfData_Destroy(data: *mut c_void);
    fn OH_UdmfData_AddRecord(data: *mut c_void, record: *mut c_void) -> c_int;
    fn OH_UdmfData_GetPrimaryPlainText(data: *mut c_void, plain_text: *mut c_void) -> c_int;
    fn OH_UdmfData_GetRecords(data: *mut c_void, count: *mut c_uint) -> *mut *mut c_void;
}

/// The image flavours the pasteboard carries, as `(mime type, UDMF record type
/// of udmf_meta.h)`.
///
/// UDMF defines no record type for GIF, WebP or SVG, so those images cannot
/// travel through the pasteboard; the macOS back-end carries them because its
/// pasteboard accepts arbitrary type identifiers.
const IMAGE_TYPES: [(&str, &CStr); 3] = [
    ("image/png", c"general.png"),
    ("image/jpeg", c"general.jpeg"),
    ("image/tiff", c"general.tiff"),
];

/// One image flavour of the clipboard: the encoded bytes plus the mime type that
/// says how to decode them.
#[derive(Debug, Clone)]
pub struct ClipboardImage {
    pub data: Vec<u8>,
    pub mime_type: String,
}

/// Everything one pasteboard operation carries.
#[derive(Debug, Default, Clone)]
pub struct ClipboardContent {
    pub plain_text: String,
    pub html: Option<String>,
    pub images: Vec<ClipboardImage>,
}

/// Writes every flavour of `content` to the system clipboard. Returns `false`
/// when the pasteboard was left unchanged.
pub fn write_content(content: &ClipboardContent) -> bool {
    log::info!(
        "clipboard::write_content: text={} byte(s), html={}, image(s)={}",
        content.plain_text.len(),
        content.html.as_ref().map_or(0, String::len),
        content.images.len()
    );

    // SAFETY: the pasteboard handle is created and destroyed here, and every
    // UDMF object the write creates is released before returning.
    unsafe {
        let pasteboard = OH_Pasteboard_Create();
        if pasteboard.is_null() {
            log::error!("clipboard::write_content: OH_Pasteboard_Create failed");
            return false;
        }
        let result = write_content_inner(pasteboard, content);
        OH_Pasteboard_Destroy(pasteboard);
        result
    }
}

/// Builds the data set and hands it to the pasteboard.
///
/// The text and HTML flavours share one record so that a consumer reading only
/// the first record still sees both; each image gets a record of its own, since
/// a record carries at most one entry per type.
unsafe fn write_content_inner(pasteboard: *mut c_void, content: &ClipboardContent) -> bool {
    let data = OH_UdmfData_Create();
    if data.is_null() {
        log::error!("clipboard::write_content: OH_UdmfData_Create failed");
        return false;
    }

    let mut records: Vec<*mut c_void> = Vec::new();
    let mut success = true;

    let has_text = !content.plain_text.is_empty();
    let has_html = content.html.as_deref().is_some_and(|html| !html.is_empty());
    if has_text || has_html {
        match build_text_record(content) {
            Some(record) => records.push(record),
            None => success = false,
        }
    }

    for image in &content.images {
        let Some((_, type_id)) = IMAGE_TYPES
            .iter()
            .find(|(mime_type, _)| *mime_type == image.mime_type)
        else {
            log::warn!(
                "clipboard::write_content: the pasteboard has no record type for {}, dropping \
                 the image",
                image.mime_type
            );
            success = false;
            continue;
        };
        match build_image_record(type_id, image) {
            Some(record) => records.push(record),
            None => success = false,
        }
    }

    if records.is_empty() {
        log::warn!("clipboard::write_content: nothing to write, leaving the pasteboard unchanged");
        success = false;
    }

    if success {
        for record in &records {
            if unsafe { OH_UdmfData_AddRecord(data, *record) } != 0 {
                log::error!("clipboard::write_content: OH_UdmfData_AddRecord failed");
                success = false;
                break;
            }
        }
    }

    if success && unsafe { OH_Pasteboard_SetData(pasteboard, data) } != 0 {
        log::error!("clipboard::write_content: OH_Pasteboard_SetData failed");
        success = false;
    }

    // SAFETY: each pointer came from the matching create call and the data set
    // made its own copies when the records were added.
    for record in records {
        unsafe { OH_UdmfRecord_Destroy(record) };
    }
    unsafe { OH_UdmfData_Destroy(data) };
    success
}

/// Builds the record holding the plain text and HTML flavours.
unsafe fn build_text_record(content: &ClipboardContent) -> Option<*mut c_void> {
    let record = OH_UdmfRecord_Create();
    if record.is_null() {
        log::error!("clipboard::write_content: OH_UdmfRecord_Create failed");
        return None;
    }

    if !content.plain_text.is_empty() {
        let plain_text = OH_UdsPlainText_Create();
        if plain_text.is_null() {
            log::error!("clipboard::write_content: OH_UdsPlainText_Create failed");
            unsafe { OH_UdmfRecord_Destroy(record) };
            return None;
        }
        let Ok(text) = CString::new(content.plain_text.as_str()) else {
            log::error!("clipboard::write_content: the plain text holds an interior NUL byte");
            unsafe { OH_UdsPlainText_Destroy(plain_text) };
            unsafe { OH_UdmfRecord_Destroy(record) };
            return None;
        };
        let added = unsafe { OH_UdsPlainText_SetContent(plain_text, text.as_ptr()) } == 0
            && unsafe { OH_UdmfRecord_AddPlainText(record, plain_text) } == 0;
        // SAFETY: the record copies the content when it is added.
        unsafe { OH_UdsPlainText_Destroy(plain_text) };
        if !added {
            log::error!("clipboard::write_content: adding the plain text flavour failed");
            unsafe { OH_UdmfRecord_Destroy(record) };
            return None;
        }
    }

    if let Some(html) = content.html.as_deref().filter(|html| !html.is_empty()) {
        let html_uds = OH_UdsHtml_Create();
        if html_uds.is_null() {
            log::error!("clipboard::write_content: OH_UdsHtml_Create failed");
            unsafe { OH_UdmfRecord_Destroy(record) };
            return None;
        }
        let added = match CString::new(html) {
            Ok(html_c_string) => {
                let set =
                    unsafe { OH_UdsHtml_SetContent(html_uds, html_c_string.as_ptr()) } == 0;
                set && unsafe { OH_UdmfRecord_AddHtml(record, html_uds) } == 0
            }
            Err(_) => {
                log::error!("clipboard::write_content: the HTML flavour holds an interior NUL byte");
                false
            }
        };
        // SAFETY: the record copies the content when it is added.
        unsafe { OH_UdsHtml_Destroy(html_uds) };
        if !added {
            log::error!("clipboard::write_content: adding the HTML flavour failed");
            unsafe { OH_UdmfRecord_Destroy(record) };
            return None;
        }
    }

    Some(record)
}

/// Builds the record holding one encoded image.
unsafe fn build_image_record(type_id: &CStr, image: &ClipboardImage) -> Option<*mut c_void> {
    if image.data.is_empty() {
        log::warn!(
            "clipboard::write_content: the {} image carries no bytes, dropping it",
            image.mime_type
        );
        return None;
    }

    let record = OH_UdmfRecord_Create();
    if record.is_null() {
        log::error!("clipboard::write_content: OH_UdmfRecord_Create failed");
        return None;
    }

    // The C signature takes a mutable pointer although it only reads the bytes.
    let entry = image.data.as_ptr() as *mut c_uchar;
    let count = image.data.len() as c_uint;
    if unsafe { OH_UdmfRecord_AddGeneralEntry(record, type_id.as_ptr(), entry, count) } != 0 {
        log::error!(
            "clipboard::write_content: OH_UdmfRecord_AddGeneralEntry failed for {}",
            image.mime_type
        );
        // SAFETY: the record was created above and holds nothing yet.
        unsafe { OH_UdmfRecord_Destroy(record) };
        return None;
    }

    Some(record)
}

/// Reads every flavour the clipboard carries.
pub fn read_content() -> ClipboardContent {
    log::info!("clipboard::read_content: reading the pasteboard");

    // SAFETY: the pasteboard handle is created and destroyed here, and every
    // UDMF object the read creates is released before returning.
    unsafe {
        let pasteboard = OH_Pasteboard_Create();
        if pasteboard.is_null() {
            log::error!("clipboard::read_content: OH_Pasteboard_Create failed");
            return ClipboardContent::default();
        }
        let content = read_content_inner(pasteboard);
        OH_Pasteboard_Destroy(pasteboard);
        content
    }
}

unsafe fn read_content_inner(pasteboard: *mut c_void) -> ClipboardContent {
    let mut status: c_int = 0;
    let data = unsafe { OH_Pasteboard_GetData(pasteboard, &mut status) };
    if data.is_null() || status != 0 {
        log::warn!("clipboard::read_content: OH_Pasteboard_GetData status={status}");
        if !data.is_null() {
            // SAFETY: the data set was returned by the pasteboard above.
            unsafe { OH_UdmfData_Destroy(data) };
        }
        return ClipboardContent::default();
    }

    let mut content = ClipboardContent::default();

    let plain_text = unsafe { OH_UdsPlainText_Create() };
    if plain_text.is_null() {
        log::error!("clipboard::read_content: OH_UdsPlainText_Create failed");
    } else {
        if unsafe { OH_UdmfData_GetPrimaryPlainText(data, plain_text) } == 0 {
            let text = unsafe { OH_UdsPlainText_GetContent(plain_text) };
            if !text.is_null() {
                // SAFETY: the text lives as long as the plain text object.
                content.plain_text = unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned();
            }
        } else {
            log::debug!("clipboard::read_content: the pasteboard holds no primary plain text");
        }
        // SAFETY: the object was created above.
        unsafe { OH_UdsPlainText_Destroy(plain_text) };
    }

    let mut record_count: c_uint = 0;
    let records = unsafe { OH_UdmfData_GetRecords(data, &mut record_count) };
    if !records.is_null() && record_count > 0 {
        // SAFETY: the pasteboard reports a count that matches the array it owns.
        let records = unsafe { slice::from_raw_parts(records, record_count as usize) };
        for record in records {
            if content.html.is_none() {
                content.html = read_html(*record);
            }
            if let Some(image) = read_image(*record) {
                content.images.push(image);
            }
        }
    }

    log::info!(
        "clipboard::read_content: {} text byte(s), html={}, image(s)={}",
        content.plain_text.len(),
        content.html.as_ref().map_or(0, String::len),
        content.images.len()
    );

    // SAFETY: the data set was returned by the pasteboard above.
    unsafe { OH_UdmfData_Destroy(data) };
    content
}

/// Reads the HTML flavour of one record, or `None` when it carries none.
unsafe fn read_html(record: *mut c_void) -> Option<String> {
    let html = unsafe { OH_UdsHtml_Create() };
    if html.is_null() {
        log::error!("clipboard::read_content: OH_UdsHtml_Create failed");
        return None;
    }

    let mut content = None;
    if unsafe { OH_UdmfRecord_GetHtml(record, html) } == 0 {
        let text = unsafe { OH_UdsHtml_GetContent(html) };
        if !text.is_null() {
            // SAFETY: the text lives as long as the HTML object.
            content = Some(unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned());
        }
    } else {
        log::debug!("clipboard::read_content: the record carries no HTML flavour");
    }

    // SAFETY: the object was created above.
    unsafe { OH_UdsHtml_Destroy(html) };
    content
}

/// Reads the first image flavour of one record, or `None` when it carries none.
unsafe fn read_image(record: *mut c_void) -> Option<ClipboardImage> {
    for (mime_type, type_id) in IMAGE_TYPES {
        let mut entry: *mut c_uchar = std::ptr::null_mut();
        let mut count: c_uint = 0;
        let found =
            unsafe { OH_UdmfRecord_GetGeneralEntry(record, type_id.as_ptr(), &mut entry, &mut count) }
                == 0
                && !entry.is_null()
                && count > 0;
        if !found {
            log::debug!("clipboard::read_content: the record carries no {mime_type} image");
            continue;
        }
        // SAFETY: the record reports a count that matches the bytes it owns.
        let data = unsafe { slice::from_raw_parts(entry, count as usize) }.to_vec();
        return Some(ClipboardImage {
            data,
            mime_type: mime_type.to_string(),
        });
    }
    None
}
