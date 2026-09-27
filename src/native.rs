// SPDX-License-Identifier: MIT OR Apache-2.0
//! In-process bridge. Pixels cross a C ABI, never HTTP. Temporary storage keeps
//! memory bounded for large scans; its lifetime is the job, not the document.
use crate::{Analysis, Config};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Instant,
};

type BridgeResult<T> = std::result::Result<T, String>;
struct Job {
    id: u64,
    config: Config,
    source: File,
    _directory: tempfile::TempDir,
    uploaded: usize,
    state: &'static str,
    error: Option<String>,
    analysis: Option<Analysis>,
    cancel: Arc<AtomicBool>,
    analysis_ms: u128,
}
#[derive(Default)]
struct State {
    next_id: u64,
    job: Option<Job>,
    worker: Option<JoinHandle<()>>,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(Mutex::default)
}
fn native_tile_height(c: &Config) -> usize {
    // Up to 64 MiB of carrier + two masks, while every captured source strip
    // stays below the 32 MiB input bound. Avoid hundreds of small host writes.
    (64 * 1024 * 1024 / (c.width * (c.channels + 2) * 2))
        .min(32 * 1024 * 1024 / (c.width * c.channels * 2))
        .clamp(1, 1024)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    path: String,
    #[serde(default = "get")]
    method: String,
    body: Option<Value>,
}
fn get() -> String {
    "GET".into()
}
fn number(s: &str) -> BridgeResult<usize> {
    s.parse().map_err(|_| "Invalid index".into())
}
pub struct Reply {
    pub binary: bool,
    pub bytes: Vec<u8>,
}
fn json_reply(value: Value) -> BridgeResult<Reply> {
    Ok(Reply {
        binary: false,
        bytes: serde_json::to_vec(&value).map_err(|e| e.to_string())?,
    })
}
fn bytes(values: &[u16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Dispatch a bounded, path-free message. All input bytes are consumed before
/// returning, and no pointers into the JavaScript heap are retained.
pub fn dispatch(request: &[u8], pixels: &[u8]) -> BridgeResult<Reply> {
    if request.len() > 64 * 1024 || pixels.len() > 32 * 1024 * 1024 {
        return Err("Native request too large".into());
    }
    let req: Request = serde_json::from_slice(request).map_err(|e| e.to_string())?;
    let parts: Vec<_> = req.path.trim_start_matches('/').split('/').collect();
    let mut state = state().lock().map_err(|_| "Native state unavailable")?;
    if state.worker.as_ref().is_some_and(JoinHandle::is_finished) {
        let _ = state.worker.take().unwrap().join();
    }
    match (req.method.as_str(), parts.as_slice()) {
        ("GET", ["health"]) => json_reply(
            json!({"service":"photoshop-banding","protocol":1,"version":env!("CARGO_PKG_VERSION"),"transport":"native"}),
        ),
        ("POST", ["jobs"]) => {
            if state.job.is_some() || state.worker.is_some() {
                return Err("Previous correction is still running or cancelling".into());
            }
            let config: Config = serde_json::from_value(req.body.ok_or("Missing configuration")?)
                .map_err(|e| e.to_string())?;
            config.validate().map_err(|e| e.to_string())?;
            let directory = tempfile::Builder::new()
                .prefix("photoshop-banding-native-")
                .tempdir()
                .map_err(|e| e.to_string())?;
            let source = File::options()
                .read(true)
                .write(true)
                .create_new(true)
                .open(directory.path().join("source.u16"))
                .map_err(|e| e.to_string())?;
            state.next_id += 1;
            let id = state.next_id;
            let tile_height = native_tile_height(&config);
            state.job = Some(Job {
                id,
                config,
                source,
                _directory: directory,
                uploaded: 0,
                state: "uploading",
                error: None,
                analysis: None,
                cancel: Arc::new(AtomicBool::new(false)),
                analysis_ms: 0,
            });
            json_reply(json!({"id":id,"tile_height":tile_height}))
        }
        (_, ["jobs", id, rest @ ..]) => {
            let id = id.parse::<u64>().map_err(|_| "Invalid job ID")?;
            let job = state
                .job
                .as_mut()
                .filter(|j| j.id == id)
                .ok_or("Unknown job")?;
            match (req.method.as_str(), rest) {
                ("POST", ["compact-opacity"]) => {
                    if job.state != "ready" {
                        return Err("Opacity can only be set after fitting".into());
                    }
                    let opacity = req
                        .body
                        .as_ref()
                        .and_then(|b| b.get("opacity"))
                        .and_then(Value::as_f64)
                        .ok_or("Missing opacity")?;
                    if !opacity.is_finite() || !(0.0 < opacity && opacity <= 1.0) {
                        return Err("Invalid opacity".into());
                    }
                    job.config.compact_opacity = opacity;
                    job.analysis
                        .as_mut()
                        .ok_or("Analysis not ready")?
                        .config
                        .compact_opacity = opacity;
                    json_reply(json!({"opacity":opacity}))
                }
                ("DELETE", []) => {
                    job.cancel.store(true, Ordering::Relaxed);
                    if job.state == "analyzing" {
                        job.state = "cancelling";
                    } else if job.state != "cancelling" {
                        state.job = None;
                    }
                    json_reply(json!({"cancelled":true}))
                }
                ("PUT", ["rows", top]) => {
                    let top = number(top)?;
                    let row_bytes = job.config.width * job.config.channels * 2;
                    if job.state != "uploading"
                        || top != job.uploaded
                        || pixels.is_empty()
                        || !pixels.len().is_multiple_of(row_bytes)
                        || top + pixels.len() / row_bytes > job.config.height
                    {
                        return Err("Invalid or out-of-order source strip".into());
                    }
                    job.source.write_all(pixels).map_err(|e| e.to_string())?;
                    job.uploaded += pixels.len() / row_bytes;
                    json_reply(json!({"uploaded_rows":job.uploaded}))
                }
                ("POST", ["analyze"]) => {
                    if job.state != "uploading" || job.uploaded != job.config.height {
                        return Err("Source capture incomplete".into());
                    }
                    job.source.flush().map_err(|e| e.to_string())?;
                    let mut source = job.source.try_clone().map_err(|e| e.to_string())?;
                    let config = job.config.clone();
                    let cancel = job.cancel.clone();
                    let worker = std::thread::Builder::new()
                        .name("banding-fit".into())
                        .spawn(move || {
                            let started = Instant::now();
                            let result = catch_unwind(AssertUnwindSafe(|| {
                                crate::analyze(&mut source, config, &cancel)
                            }));
                            drop(source);
                            if let Ok(mut state) = self::state().lock() {
                                if cancel.load(Ordering::Relaxed) {
                                    if state.job.as_ref().is_some_and(|j| j.id == id) {
                                        state.job = None;
                                    }
                                } else if let Some(job) = state.job.as_mut().filter(|j| j.id == id)
                                {
                                    job.analysis_ms = started.elapsed().as_millis();
                                    match result {
                                        Ok(Ok(analysis)) => {
                                            job.analysis = Some(analysis);
                                            job.state = "ready";
                                        }
                                        result => {
                                            job.state = "failed";
                                            job.error = Some(match result {
                                                Ok(Err(e)) => e.to_string(),
                                                _ => "Numerical worker failed".into(),
                                            });
                                        }
                                    }
                                }
                            }
                        })
                        .map_err(|e| e.to_string())?;
                    job.state = "analyzing";
                    state.worker = Some(worker);
                    json_reply(json!({"state":"analyzing"}))
                }
                ("GET", ["status"]) => {
                    let report = job.analysis.as_ref().map(|a| {
                        let mut r = a.report();
                        r["engine_analysis_ms"] = json!(job.analysis_ms);
                        r["transport"] = json!("native");
                        r["tile_height"] = json!(native_tile_height(&job.config));
                        r
                    });
                    json_reply(json!({"state":job.state,"error":job.error,"report":report}))
                }
                ("GET", ["tile", kind, top, rows]) => {
                    let (top, rows) = (number(top)?, number(rows)?);
                    if rows == 0
                        || rows > native_tile_height(&job.config)
                        || top.checked_add(rows).is_none_or(|n| n > job.config.height)
                    {
                        return Err("Invalid tile bounds".into());
                    }
                    if !kind.starts_with("compact-") {
                        return Err("Unknown tile kind".into());
                    }
                    let component = if *kind == "compact-reference" {
                        None
                    } else {
                        Some(
                            kind.strip_prefix("compact-")
                                .unwrap()
                                .parse::<usize>()
                                .map_err(|_| "Invalid pure component")?,
                        )
                    };
                    let analysis = job.analysis.as_ref().ok_or("Analysis not ready")?;
                    let row_bytes = job.config.width * job.config.channels * 2;
                    job.source
                        .seek(SeekFrom::Start((top * row_bytes) as u64))
                        .map_err(|e| e.to_string())?;
                    let mut packed = vec![0; rows * row_bytes];
                    job.source
                        .read_exact(&mut packed)
                        .map_err(|e| e.to_string())?;
                    let raw: Vec<u16> = packed
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|p| u16::from_le_bytes(*p))
                        .collect();
                    let (tile, reference) = analysis
                        .render_compact_parallel(component, &raw, top, &job.cancel)
                        .map_err(|e| e.to_string())?;
                    let data = if let Some(tile) = tile {
                        let mut data = bytes(&tile.pixels);
                        data.extend(bytes(&tile.mask));
                        data
                    } else {
                        bytes(&reference)
                    };
                    Ok(Reply {
                        binary: true,
                        bytes: data,
                    })
                }
                _ => Err("Unknown native operation".into()),
            }
        }
        _ => Err("Unknown native operation".into()),
    }
}

/// Quiesce the worker before Photoshop unloads the library. Never join while
/// holding STATE: the worker needs that lock to complete.
pub fn shutdown() {
    let (worker, job) = {
        let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = &state.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        (state.worker.take(), state.job.take())
    };
    if let Some(worker) = worker {
        let _ = worker.join();
    }
    drop(job);
}

#[repr(C)]
pub struct Buffer {
    pub data: *mut u8,
    pub len: usize,
    pub kind: u32,
}
/// # Safety
/// Both input pointers must describe valid readable byte ranges for this call.
/// Release the returned allocation exactly once with banding_buffer_free.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn banding_dispatch(
    request: *const u8,
    request_len: usize,
    pixels: *const u8,
    pixels_len: usize,
) -> Buffer {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        if request.is_null()
            || request_len == 0
            || request_len > 65536
            || pixels_len > 32 * 1024 * 1024
            || (pixels_len > 0 && pixels.is_null())
        {
            return Err("Invalid native input".into());
        }
        let request = unsafe { std::slice::from_raw_parts(request, request_len) };
        let pixels = if pixels_len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(pixels, pixels_len) }
        };
        dispatch(request, pixels)
    }));
    let (data, kind) = match outcome {
        Ok(Ok(r)) => (r.bytes, u32::from(r.binary)),
        Ok(Err(e)) => (e.into_bytes(), 2),
        Err(_) => (b"Native engine panic".to_vec(), 2),
    };
    let mut data = data.into_boxed_slice();
    let out = Buffer {
        data: data.as_mut_ptr(),
        len: data.len(),
        kind,
    };
    std::mem::forget(data);
    out
}
/// # Safety
/// buffer must be an allocation returned by banding_dispatch, not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn banding_buffer_free(buffer: Buffer) {
    if !buffer.data.is_null() {
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buffer.data, buffer.len)) });
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn banding_shutdown() {
    let _ = catch_unwind(shutdown);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(path: &str, method: &str, body: Value, data: &[u8]) -> BridgeResult<Reply> {
        dispatch(
            &serde_json::to_vec(&json!({"path":path,"method":method,"body":body})).unwrap(),
            data,
        )
    }
    fn value(reply: Reply) -> Value {
        assert!(!reply.binary);
        serde_json::from_slice(&reply.bytes).unwrap()
    }
    #[test]
    fn native_abi_lifecycle_matches_reference_and_quiesces_worker() {
        shutdown();
        // Invalid pointers are rejected before dereference; errors retain an owned buffer.
        let invalid = unsafe { banding_dispatch(std::ptr::null(), 0, std::ptr::null(), 0) };
        assert_eq!(invalid.kind, 2);
        unsafe { banding_buffer_free(invalid) };
        let config = json!({"width":384,"height":64,"channels":1});
        let job = value(call("/jobs", "POST", config.clone(), &[]).unwrap());
        let base = format!("/jobs/{}", job["id"]);
        assert!(
            call(
                &format!("{base}/compact-opacity"),
                "POST",
                json!({"opacity":0.66}),
                &[]
            )
            .is_err()
        );
        assert!(call("/jobs", "POST", config.clone(), &[]).is_err());
        assert!(call(&format!("{base}/analyze"), "POST", Value::Null, &[]).is_err());
        let raw: Vec<u16> = (0..384 * 64)
            .map(|i| {
                (65535.0
                    * 0.14
                    * (0.045 * (std::f64::consts::TAU * (i % 384) as f64 / 37.3).cos()).exp())
                .round() as u16
            })
            .collect();
        assert!(call(&format!("{base}/rows/1"), "PUT", Value::Null, &bytes(&raw)).is_err());
        call(&format!("{base}/rows/0"), "PUT", Value::Null, &bytes(&raw)).unwrap();
        call(&format!("{base}/analyze"), "POST", Value::Null, &[]).unwrap();
        let start = Instant::now();
        loop {
            let status = value(call(&format!("{base}/status"), "GET", Value::Null, &[]).unwrap());
            assert_ne!(status["state"], "failed", "{status}");
            if status["state"] == "ready" {
                assert_eq!(
                    status["report"]["pure_components"]
                        .as_array()
                        .unwrap()
                        .len(),
                    1
                );
                break;
            }
            assert!(start.elapsed().as_secs() < 20);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let decode = |reply: Reply| {
            assert!(reply.binary);
            reply
                .bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .collect::<Vec<_>>()
        };
        for kind in [
            "correction",
            "reference",
            "adaptive",
            "adaptive-reference",
            "pure-0",
            "pure-reference",
        ] {
            assert!(call(&format!("{base}/tile/{kind}/0/64"), "GET", Value::Null, &[]).is_err());
        }
        for opacity in [0.0, -1.0, 1.01] {
            assert!(
                call(
                    &format!("{base}/compact-opacity"),
                    "POST",
                    json!({"opacity":opacity}),
                    &[]
                )
                .is_err()
            );
        }
        let opacity = 168.0 / 255.0;
        assert_eq!(
            value(
                call(
                    &format!("{base}/compact-opacity"),
                    "POST",
                    json!({"opacity":opacity}),
                    &[]
                )
                .unwrap()
            )["opacity"],
            opacity
        );
        let compact = decode(
            call(
                &format!("{base}/tile/compact-0/0/64"),
                "GET",
                Value::Null,
                &[],
            )
            .unwrap(),
        );
        let compact_reference = decode(
            call(
                &format!("{base}/tile/compact-reference/0/64"),
                "GET",
                Value::Null,
                &[],
            )
            .unwrap(),
        );
        assert_eq!(compact.len(), raw.len() * 2);
        for row in compact[..raw.len()].as_chunks::<384>().0 {
            assert_eq!(row, &compact[..384]);
        }
        for (i, &original) in raw.iter().enumerate() {
            let base = super::super::quantize(original as f64 / 65535.0) as f64 / 32768.0;
            let full = (base + 2.0 * compact[i] as f64 / 32768.0 - 1.0).clamp(0.0, 1.0);
            let actual = super::super::quantize(
                base + opacity * compact[raw.len() + i] as f64 / 32768.0 * (full - base),
            );
            assert_eq!(
                actual,
                super::super::quantize(compact_reference[i] as f64 / 65535.0)
            );
        }
        call(&base, "DELETE", Value::Null, &[]).unwrap();
        // Repeated unload/reload, including an active fitting worker.
        for _ in 0..3 {
            let job = value(call("/jobs", "POST", config.clone(), &[]).unwrap());
            let base = format!("/jobs/{}", job["id"]);
            call(&format!("{base}/rows/0"), "PUT", Value::Null, &bytes(&raw)).unwrap();
            call(&format!("{base}/analyze"), "POST", Value::Null, &[]).unwrap();
            call(&base, "DELETE", Value::Null, &[]).unwrap();
            shutdown();
            assert!(state().lock().unwrap().job.is_none());
            assert!(state().lock().unwrap().worker.is_none());
        }
    }
}
