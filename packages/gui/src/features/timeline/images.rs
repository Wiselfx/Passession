//! Port of `markdown_image.dart`: assistant-message images. HTTP(S) uses core
//! host-origin admission and the selected connection transport; `data:` decodes in place,
//! workspace files are read through the host (SDK `read_file`). Decoding runs
//! off the GUI thread; the texture is created on it. SVGs (`data:` and
//! workspace files) render through resvg on desktop as flutter_svg did; an
//! image that cannot be read or decoded falls back to its alt text.

use super::links::is_absolute_file_path;
use base64::Engine;
use makepad_widgets::image_cache::ImageBuffer;
use makepad_widgets::*;
use paseo_gui_core::{CoreHandle, RequestId};
use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};

/// acquisition-cache.ts: at most 500 acquisitions, least recently used first.
const CAPACITY: usize = 500;
const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub enum ImageState {
    Loading,
    Ready {
        texture: Texture,
        width: usize,
        height: usize,
    },
    Failed,
    Error(String),
}

/// utils/assistant-image-source.ts AssistantImageSourceResolution.
#[derive(Clone, Debug, PartialEq)]
pub enum ImageSource {
    Direct(String),
    File { cwd: String, path: String },
}

fn is_home_relative(path: &str) -> bool {
    path == "~" || path.starts_with("~/") || path.starts_with("~\\")
}

fn is_within(candidate: &str, root: &str) -> bool {
    let normalize = |value: &str| {
        let mut normalized = value.replace('\\', "/");
        let drive_root = normalized.len() <= 3 && normalized.as_bytes().get(1) == Some(&b':');
        if normalized != "/" && !drive_root {
            normalized = normalized.trim_end_matches('/').to_owned();
        }
        if normalized.len() >= 3
            && normalized.as_bytes()[1] == b':'
            && normalized.as_bytes()[2] == b'/'
        {
            let mut chars = normalized.chars();
            let first = chars.next().unwrap().to_ascii_uppercase();
            normalized = format!("{first}{}", chars.as_str());
        }
        normalized
    };
    let path = normalize(candidate);
    let base = normalize(root);
    if base == "/" {
        return path.starts_with('/');
    }
    path == base || path.starts_with(&format!("{base}/"))
}

/// resolveAssistantImageSource.
pub fn resolve_image_source(source: &str, workspace_root: Option<&str>) -> Option<ImageSource> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    if lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("data:")
        || lower.starts_with("blob:")
    {
        return Some(ImageSource::Direct(trimmed.to_owned()));
    }
    let path_source = trimmed.split(['?', '#']).next().unwrap_or("");
    let mut path = if let Some(rest) = path_source.strip_prefix("file://") {
        let path = match rest.find('/') {
            Some(slash) => &rest[slash..],
            None => "",
        };
        let decoded = super::links::percent_decode(path).unwrap_or_else(|| path.to_owned());
        let bytes = decoded.as_bytes();
        if bytes.len() >= 4
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
            && bytes[3] == b'/'
        {
            decoded[1..].to_owned()
        } else {
            decoded
        }
    } else {
        super::links::percent_decode(path_source).unwrap_or_else(|| path_source.to_owned())
    };
    path = path.trim().to_owned();
    if path.is_empty() {
        return None;
    }
    if is_home_relative(&path) {
        return Some(ImageSource::File {
            cwd: "~".into(),
            path,
        });
    }
    let root = workspace_root.unwrap_or("").trim();
    if !is_absolute_file_path(&path) {
        if root.is_empty() || !is_absolute_file_path(root) {
            return None;
        }
        return Some(ImageSource::File {
            cwd: root.to_owned(),
            path,
        });
    }
    if !root.is_empty() && is_absolute_file_path(root) && is_within(&path, root) {
        return Some(ImageSource::File {
            cwd: root.to_owned(),
            path,
        });
    }
    let bytes = path.as_bytes();
    let fs_root = if path.starts_with('/') {
        Some("/".to_owned())
    } else if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        Some(format!("{}/", &path[..2]))
    } else if let Some(rest) = path.strip_prefix("\\\\") {
        let mut parts = rest.splitn(3, '\\');
        match (parts.next(), parts.next()) {
            (Some(server), Some(share)) if !server.is_empty() && !share.is_empty() => {
                Some(format!("\\\\{server}\\{share}"))
            }
            _ => None,
        }
    } else {
        None
    };
    fs_root.map(|cwd| ImageSource::File { cwd, path })
}

fn is_svg(source: &str) -> bool {
    let lower = source.trim().to_lowercase();
    lower.starts_with("data:image/svg+xml") || {
        let path = lower.split(['?', '#']).next().unwrap_or("");
        path.ends_with(".svg")
    }
}

struct Decoded {
    key: String,
    result: Result<ImageBuffer, String>,
}

/// Off-thread decode results, picked up on `Event::Signal`.
static DECODED: LazyLock<Mutex<Vec<Decoded>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// File bytes and their mime type.
type FileBytes = (Vec<u8>, String);

/// Bytes read by a core operation, keyed by the operation's request id.
static FILE_BYTES: LazyLock<Mutex<HashMap<String, FileBytes>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct HttpImage {
    host: String,
    key: String,
    core: CoreHandle,
}

#[derive(Default)]
struct Images {
    states: HashMap<String, ImageState>,
    order: VecDeque<String>,
    http: HashMap<RequestId, HttpImage>,
    operations: HashMap<RequestId, String>,
    /// Changes when an acquisition settles or a route is invalidated, never on a
    /// new `Loading` entry.
    version: u64,
    generations: HashMap<String, Option<u64>>,
    /// Draw lists that drew each still-loading key; its outcome repaints them.
    waiting: HashMap<String, Vec<DrawListId>>,
    /// Owners of loading keys evicted (possibly mid-draw), repainted by `handle_event`
    /// so they acquire again.
    retry: Vec<DrawListId>,
}

thread_local! {
    static IMAGES: std::cell::RefCell<Images> = std::cell::RefCell::new(Images::default());
}

pub fn version() -> u64 {
    IMAGES.with(|i| i.borrow().version)
}

fn host_generations(state: &serde_json::Value) -> HashMap<String, Option<u64>> {
    let mut generations = HashMap::new();
    for host in state["hosts"].as_array().into_iter().flatten() {
        let generation = host["httpGeneration"].as_u64();
        for id in [host["serverId"].as_str(), host["config"]["id"].as_str()]
            .into_iter()
            .flatten()
            .chain(
                host["connections"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|config| config["id"].as_str()),
            )
        {
            generations.insert(id.to_string(), generation);
        }
    }
    generations
}

fn http_cache_key(host: &str, generation: Option<u64>, uri: &str) -> String {
    format!("http:{host}\0{generation:?}\0{uri}")
}

/// Invalidate before dispatching responses: a prior route must not satisfy the
/// same URL after a normal/tailnet switch, reconfiguration or account change.
/// Owners still waiting on a dropped route repaint to acquire through the new one.
pub fn sync_hosts(cx: &mut Cx, core: &CoreHandle, state: &serde_json::Value) {
    IMAGES.with(|cell| {
        let images = &mut *cell.borrow_mut();
        let generations = host_generations(state);
        let changed: Vec<String> = images
            .generations
            .iter()
            .filter(|(host, generation)| generations.get(*host) != Some(*generation))
            .map(|(host, _)| host.clone())
            .collect();
        for host in changed {
            let prefix = format!("http:{host}\0");
            images.states.retain(|key, _| !key.starts_with(&prefix));
            images.order.retain(|key| !key.starts_with(&prefix));
            images.http.retain(|id, request| {
                if request.host != host {
                    return true;
                }
                core.cancel_request(id);
                false
            });
            images.waiting.retain(|key, owners| {
                if !key.starts_with(&prefix) {
                    return true;
                }
                for owner in owners.drain(..) {
                    cx.redraw_list(owner);
                }
                false
            });
            images.version = images.version.wrapping_add(1);
        }
        images.generations = generations;
    });
}

fn decode_in_background(key: String, bytes: Vec<u8>, mime: String) {
    std::thread::spawn(move || {
        let result = decode(&bytes, &mime);
        DECODED.lock().unwrap().push(Decoded { key, result });
        SignalToUI::set_ui_signal();
    });
}

/// image-decoding.ts: an image counts only once it decodes to positive
/// dimensions. Raster formats and SVG (rendered by resvg, as flutter_svg
/// draws `SvgPicture.memory` on desktop) share the Files decoder; SVGs
/// rasterize at twice the 320px image box so they stay crisp.
fn decode(bytes: &[u8], mime: &str) -> Result<ImageBuffer, String> {
    let decoded = crate::features::files::images::decode(bytes, mime, 640)?;
    if decoded.width == 0 || decoded.height == 0 {
        return Err("Image has no decodable dimensions".into());
    }
    ImageBuffer::new(&decoded.rgba, decoded.width, decoded.height).map_err(|e| format!("{e:?}"))
}

fn remember(images: &mut Images, key: &str, state: ImageState) {
    images.order.retain(|k| k != key);
    images.order.push_back(key.to_owned());
    let loading = matches!(state, ImageState::Loading);
    images.states.insert(key.to_owned(), state);
    while images.order.len() > CAPACITY {
        if let Some(oldest) = images.order.pop_front() {
            images.states.remove(&oldest);
            if let Some(owners) = images.waiting.remove(&oldest) {
                images.retry.extend(owners);
            }
            images.http.retain(|id, request| {
                if request.key != oldest {
                    return true;
                }
                request.core.cancel_request(id);
                false
            });
        }
    }
    // A new acquisition shows the placeholder its requester already drew.
    if !loading {
        images.version = images.version.wrapping_add(1);
    }
}

/// Stores a settled acquisition and repaints only the draw lists that showed it loading.
fn settle(cx: &mut Cx, images: &mut Images, key: &str, state: ImageState) {
    remember(images, key, state);
    for owner in images.waiting.remove(key).into_iter().flatten() {
        cx.redraw_list(owner);
    }
}

/// Records the draw list showing `key`'s placeholder, repainted once it settles.
fn wait(cx: &Cx2d, images: &mut Images, key: &str) {
    let Some(owner) = cx.get_current_draw_list_id() else {
        return;
    };
    let owners = images.waiting.entry(key.to_owned()).or_default();
    if !owners.contains(&owner) {
        owners.push(owner);
    }
}

/// The state of one assistant image occurrence, starting its acquisition on
/// first sight. `occurrence` scopes file reads to the rendered message. A
/// `Loading` answer records the draw list being drawn, repainted once it settles.
pub fn image(
    cx: &mut Cx2d,
    core: Option<&CoreHandle>,
    host_id: &str,
    workspace_root: Option<&str>,
    source: &str,
    occurrence: &str,
) -> ImageState {
    // source.ts: SVGs show their alt text where the renderer cannot draw
    // them (Android/iOS) and are never fetched over http(s).
    let svg = is_svg(source);
    if svg && super::paint::MOBILE {
        return ImageState::Failed;
    }
    let Some(resolved) = resolve_image_source(source, workspace_root) else {
        return ImageState::Failed;
    };
    let key = match &resolved {
        ImageSource::Direct(uri)
            if uri
                .get(..5)
                .is_some_and(|p| p.eq_ignore_ascii_case("http:"))
                || uri
                    .get(..6)
                    .is_some_and(|p| p.eq_ignore_ascii_case("https:")) =>
        {
            let generation =
                IMAGES.with(|images| images.borrow().generations.get(host_id).copied().flatten());
            http_cache_key(host_id, generation, uri)
        }
        ImageSource::Direct(uri) => format!("direct:{uri}"),
        ImageSource::File { cwd, path } => {
            format!("{host_id}\u{0}{occurrence}\u{0}{cwd}\u{0}{path}")
        }
    };
    IMAGES.with(|cell| {
        let images = &mut *cell.borrow_mut();
        if let Some(state) = images.states.get(&key).cloned() {
            images.order.retain(|k| k != &key);
            images.order.push_back(key.clone());
            if matches!(state, ImageState::Loading) {
                wait(cx, images, &key);
            }
            return state;
        }
        match resolved {
            ImageSource::Direct(uri) => {
                let lower = uri.to_lowercase();
                if lower.starts_with("data:") {
                    let Some((head, data)) = uri.split_once(',') else {
                        return ImageState::Failed;
                    };
                    let head = head.to_lowercase();
                    let bytes = if head.contains(";base64") {
                        base64::engine::general_purpose::STANDARD
                            .decode(data.trim())
                            .ok()
                    } else {
                        super::links::percent_decode(data).map(String::into_bytes)
                    };
                    let Some(bytes) = bytes else {
                        return ImageState::Failed;
                    };
                    let mime = head
                        .trim_start_matches("data:")
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .to_owned();
                    remember(images, &key, ImageState::Loading);
                    wait(cx, images, &key);
                    decode_in_background(key, bytes, mime);
                    return ImageState::Loading;
                }
                if !(lower.starts_with("http:") || lower.starts_with("https:")) || svg {
                    // blob: URLs belong to a browser page; remote SVGs show their alt text.
                    return ImageState::Failed;
                }
                let Some(core) = core else {
                    return ImageState::Loading;
                };
                let request_id = core.host_http_get(host_id, &uri, MAX_FILE_BYTES);
                images.http.insert(
                    request_id,
                    HttpImage {
                        host: host_id.to_owned(),
                        key: key.clone(),
                        core: core.clone(),
                    },
                );
                remember(images, &key, ImageState::Loading);
                wait(cx, images, &key);
                ImageState::Loading
            }
            ImageSource::File { cwd, path } => {
                let Some(core) = core else {
                    return ImageState::Loading;
                };
                // The bytes stay in a private side buffer keyed by a token;
                // the response carries only the token (no base64 copy).
                let token = uuid::Uuid::new_v4().to_string();
                let request = core.operation(host_id, move |client| async move {
                    let file = client
                        .read_file(
                            &cwd,
                            &path,
                            MAX_FILE_BYTES,
                            paseo_client::RequestOptions::default(),
                        )
                        .await?;
                    // The mime travels with the bytes so an SVG decodes as one.
                    let mime = file
                        .metadata
                        .mime
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    FILE_BYTES
                        .lock()
                        .unwrap()
                        .insert(token.clone(), (file.bytes.to_vec(), mime));
                    Ok(serde_json::json!({"token": token}))
                });
                images.operations.insert(request, key.clone());
                remember(images, &key, ImageState::Loading);
                wait(cx, images, &key);
                ImageState::Loading
            }
        }
    })
}

/// Module hook: finished decodes and core-admitted HTTP/file reads. A settled
/// image repaints only the draw lists that showed it loading, so their pane measures
/// the visible rows again (a completed decode replaces the Markdown loading
/// placeholder with the raster's aspect-ratio height, including images in table
/// cells). Responses that only start a decode, and decodes of keys no longer
/// cached, repaint nothing.
pub fn handle_event(
    cx: &mut Cx,
    event: &Event,
    responses: &[(RequestId, Result<serde_json::Value, String>)],
) {
    IMAGES.with(|cell| {
        let images = &mut *cell.borrow_mut();
        for owner in images.retry.drain(..) {
            cx.redraw_list(owner);
        }
        for (request, result) in responses {
            if let Some(http) = images.http.remove(request) {
                let body = result
                    .as_ref()
                    .map_err(Clone::clone)
                    .and_then(crate::common::host_http::image_body);
                match body {
                    Ok((bytes, mime)) => decode_in_background(http.key, bytes, mime),
                    Err(error) => settle(cx, images, &http.key, ImageState::Error(error)),
                }
                continue;
            }
            let Some(key) = images.operations.remove(request) else {
                continue;
            };
            let bytes = result.as_ref().ok().and_then(|value| {
                let token = value.get("token")?.as_str()?;
                FILE_BYTES.lock().unwrap().remove(token)
            });
            match bytes {
                Some((bytes, mime)) => decode_in_background(key, bytes, mime),
                // A failed acquisition is dropped so the next sight retries.
                None => {
                    images.order.retain(|k| k != &key);
                    images.states.insert(key.clone(), ImageState::Failed);
                    images.version = images.version.wrapping_add(1);
                    for owner in images.waiting.remove(&key).into_iter().flatten() {
                        cx.redraw_list(owner);
                    }
                }
            }
        }
        if let Event::Signal = event {
            let finished: Vec<Decoded> = std::mem::take(&mut *DECODED.lock().unwrap());
            for done in finished {
                if !images.states.contains_key(&done.key) {
                    continue;
                }
                let state = match done.result {
                    Ok(buffer) => {
                        let (width, height) = (buffer.width, buffer.height);
                        ImageState::Ready {
                            texture: buffer.into_new_texture(cx),
                            width,
                            height,
                        }
                    }
                    Err(_) => ImageState::Failed,
                };
                settle(cx, images, &done.key, state);
            }
        }
    });
}

/// Whether a core response belongs to an image read.
pub fn owns(request: &str) -> bool {
    IMAGES.with(|cell| {
        let images = cell.borrow();
        images.operations.contains_key(request) || images.http.contains_key(request)
    })
}

#[cfg(test)]
mod http_tests {
    use super::*;

    #[test]
    fn host_http_cache_isolates_connection_generations_and_host_aliases() {
        let state = serde_json::json!({"hosts": [{
            "serverId": "daemon", "config": {"id": "active"},
            "connections": [{"id": "normal"}, {"id": "embedded"}],
            "httpGeneration": 42, "connectionGeneration": 99
        }]});
        let generations = host_generations(&state);
        for id in ["daemon", "active", "normal", "embedded"] {
            assert_eq!(generations.get(id), Some(&Some(42)));
        }
        let uri = "http://same-host/image.png";
        assert_ne!(
            http_cache_key("daemon", Some(41), uri),
            http_cache_key("daemon", Some(42), uri)
        );
        assert_ne!(
            http_cache_key("other", Some(42), uri),
            http_cache_key("daemon", Some(42), uri)
        );
    }
}
