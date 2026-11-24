//! Diagnostics: what host GPU memory the title's textures take. Every title
//! texture gets a host texture of its own when it is made; the title places
//! many of its textures over the same memory (render targets that alias,
//! streamed textures), each of which then costs the host memory again.
//! Logged with the frame-rate line: the adapter's own figures, live host
//! textures by kind, and how much of them repeat a title address another
//! live texture already covers.

use std::collections::HashMap;
use std::sync::Mutex;

use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

/// A live host texture: title address, host bytes, render target or UAV.
#[derive(Clone, Copy)]
struct Live {
    address: u64,
    bytes: u64,
    target: bool,
    /// Asked for by draw, copy or view code since it was made.
    used: bool,
}

static LIVE: Mutex<Option<HashMap<usize, Live>>> = Mutex::new(None);

/// Notes the host texture `texture` of title resource `id` at title
/// `address`.
pub(crate) fn texture_made(id: usize, address: u64, texture: &ID3D12Resource, target: bool) {
    let Some(host) = crate::host::host() else { return };
    // SAFETY: a live resource and a plain size query.
    let bytes = unsafe {
        let desc = texture.GetDesc();
        host.device.GetResourceAllocationInfo(0, &[desc]).SizeInBytes
    };
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(id, Live { address, bytes, target, used: false });
}

/// Notes that draw, copy or view code asked for title resource `id`'s host
/// texture.
pub(crate) fn texture_used(id: usize) {
    if let Some(live) = LIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|m| m.get_mut(&id))
    {
        live.used = true;
    }
}

/// The bytes the ranges cover once, however many overlap.
fn union(mut ranges: Vec<(u64, u64)>) -> u64 {
    ranges.sort_unstable();
    let (mut total, mut end) = (0u64, 0u64);
    for (start, len) in ranges {
        let stop = start + len;
        if stop > end {
            total += stop - start.max(end);
            end = stop;
        }
    }
    total
}

/// Forgets title resource `id`'s host texture.
pub(crate) fn texture_gone(id: usize) {
    if let Some(map) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        map.remove(&id);
    }
}

/// The adapter's local (dedicated) and non-local (shared) use, in bytes.
fn adapter_usage() -> Option<(u64, u64)> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL,
        DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIFactory4,
    };
    let host = crate::host::host()?;
    // SAFETY: plain DXGI queries on our own device's adapter.
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory1().ok()?;
        let adapter: IDXGIAdapter3 = factory.EnumAdapterByLuid(host.device.GetAdapterLuid()).ok()?;
        let mut local = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        let mut shared = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        adapter
            .QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut local)
            .ok()?;
        adapter
            .QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, &mut shared)
            .ok()?;
        Some((local.CurrentUsage, shared.CurrentUsage))
    }
}

/// Logs the adapter's use and the live host textures.
pub(crate) fn log() {
    const MB: f64 = 1e6;
    let (mut targets, mut sampled, mut repeated, mut count) = (0u64, 0u64, 0u64, 0usize);
    let (mut unused_t, mut unused_s, mut targets_once) = (0u64, 0u64, 0u64);
    let mut largest: Vec<(u64, u64, bool)> = Vec::new();
    if let Some(map) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        count = map.len();
        let mut by_address: HashMap<u64, (u64, u64)> = HashMap::new();
        for live in map.values() {
            if live.target {
                targets += live.bytes;
            } else {
                sampled += live.bytes;
            }
            let e = by_address.entry(live.address).or_default();
            e.0 += live.bytes;
            e.1 = e.1.max(live.bytes);
        }
        for live in map.values().filter(|l| !l.used) {
            if live.target {
                unused_t += live.bytes;
            } else {
                unused_s += live.bytes;
            }
        }
        targets_once = union(
            map.values()
                .filter(|l| l.target)
                .map(|l| (l.address, l.bytes))
                .collect(),
        );
        // Past the largest texture at an address, the others repeat it.
        repeated = by_address.values().map(|&(all, max)| all - max).sum();
        largest = map.values().map(|l| (l.bytes, l.address, l.target)).collect();
        largest.sort_unstable_by_key(|l| std::cmp::Reverse(l.0));
        largest.truncate(5);
    }
    let (local, shared) = adapter_usage().unwrap_or_default();
    tracing::info!(
        "GPU memory: {:.0} MB dedicated, {:.0} MB shared; {count} host textures: {:.0} MB \
         render targets/UAVs ({:.0} MB never used, {:.0} MB of title memory), {:.0} MB \
         sampled ({:.0} MB never used), {:.0} MB repeating a title address",
        local as f64 / MB,
        shared as f64 / MB,
        targets as f64 / MB,
        unused_t as f64 / MB,
        targets_once as f64 / MB,
        sampled as f64 / MB,
        unused_s as f64 / MB,
        repeated as f64 / MB,
    );
    for (bytes, address, target) in largest {
        tracing::info!(
            "  host texture {:.1} MB at title {address:#x}{}",
            bytes as f64 / MB,
            if target { " (target)" } else { "" }
        );
    }
}
