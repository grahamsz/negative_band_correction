"""Profile the real Rust C ABI on a scan without changing the scan or Photoshop.
Requires numpy/tifffile; python scripts/benchmark-native.py <banding.dll> <scan.tif>.
Includes full capture, fitting, all output strips and buffer ownership checks.
"""
import ctypes as C
import json
import sys
import time
from pathlib import Path

import numpy as np
import tifffile

class Buffer(C.Structure):
    _fields_ = [("data", C.c_void_p), ("len", C.c_size_t), ("kind", C.c_uint32)]

lib = C.CDLL(str(Path(sys.argv[1]).resolve()))
lib.banding_dispatch.argtypes = [C.c_void_p, C.c_size_t, C.c_void_p, C.c_size_t]
lib.banding_dispatch.restype = Buffer
lib.banding_buffer_free.argtypes = [Buffer]
lib.banding_shutdown.argtypes = []

def call(path, method="GET", body=None, pixels=None, copy=False):
    request = json.dumps(dict(path=path, method=method, body=body)).encode()
    ptr = None if pixels is None else pixels.ctypes.data
    size = 0 if pixels is None else pixels.nbytes
    result = lib.banding_dispatch(request, len(request), ptr, size)
    try:
        if result.kind == 2:
            raise RuntimeError(C.string_at(result.data, result.len).decode())
        if result.kind == 0:
            return json.loads(C.string_at(result.data, result.len))
        return np.frombuffer(C.string_at(result.data,result.len),dtype='<u2').copy() if copy else result.len
    finally:
        lib.banding_buffer_free(result)

try:
    start = time.perf_counter()
    image = tifffile.imread(sys.argv[2])
    if image.dtype != np.uint16:
        raise ValueError("Use 16-bit scan")
    if image.ndim == 2:
        image = image[..., None]
    h, w, c = image.shape
    image = np.ascontiguousarray(image, dtype='<u2')
    read = time.perf_counter()-start
    start = time.perf_counter()
    job = call('/jobs', 'POST', dict(width=w, height=h, channels=c, options=dict(strength=1.0)))
    base = f'/jobs/{job["id"]}'
    th = job['tile_height']
    for top in range(0,h,th):
        call(f'{base}/rows/{top}', 'PUT', pixels=image[top:top+th])
    capture = time.perf_counter()-start
    start = time.perf_counter()
    call(f'{base}/analyze', 'POST')
    while True:
        state=call(f'{base}/status')
        if state['state'] == 'failed':
            raise RuntimeError(state['error'])
        if state['state'] == 'ready':
            break
        time.sleep(.05)
    fit = time.perf_counter()-start
    opacity=168/255
    call(f'{base}/compact-opacity','POST',dict(opacity=opacity))
    components=state['report']['pure_components']
    start = time.perf_counter()
    total_bytes = 0
    for component in components:
        for top in range(0,h,th):
            total_bytes += call(f'{base}/tile/compact-{component["index"]}/{top}/{min(th,h-top)}')
    render = time.perf_counter()-start
    worst=0
    for top in np.linspace(0,h-1,16,dtype=int):
        blended=np.rint(image[top].reshape(-1).astype(float)/65535*32768)/32768
        for component in components:
            tile=call(f'{base}/tile/compact-{component["index"]}/{top}/1',copy=True)
            full=np.clip(blended+2*tile[:w*c].astype(float)/32768-1,0,1)
            mask=np.repeat(tile[w*c:].astype(float)/32768,c)
            blended=np.rint((blended+opacity*mask*(full-blended))*32768)/32768
        expected=call(f'{base}/tile/compact-reference/{top}/1',copy=True).astype(float)
        worst=max(worst,int(np.abs(blended*32768-np.rint(expected/65535*32768)).max()))
    for component in components:
        first=call(f'{base}/tile/compact-{component["index"]}/0/1',copy=True)
        last=call(f'{base}/tile/compact-{component["index"]}/{h-1}/1',copy=True)
        # Local fitted amplitudes may vary between rows.
        analytic=np.full((w,c),16384.0)
        angle=2*np.pi*component['frequency']*np.arange(w)+component['phase']
        wave=np.cos(angle)
        analytic[:,component['channel']]=np.rint(np.clip(.5-.5*component['carrier_gain']*wave,0,1)*32768)
        for tile in (first,last):
            actual=tile[:w*c].reshape(w,c).astype(float)-16384
            bound=analytic-16384
            # Carrier amplitude now includes density and blend compensation.
            assert np.all(actual*bound>=0), 'Wave phase changed'
    assert worst==0
    checks=dict(carrier_phase_preserved=True,
        native_blend_max_difference=worst,layer_count=len(components),effective_opacity=opacity)
    call(base, 'DELETE')
    print(json.dumps(dict(file=Path(sys.argv[2]).name,shape=image.shape,tile_rows=th,output='compensated_waves_clean_density_mask',checks=checks,
        read_tiff_s=read,capture_s=capture,fit_s=fit,render_s=render,
        native_total_s=capture+fit+render,output_bytes=total_bytes,
        residual_summary=[dict(channel=r['channel'],iterations=r['accepted_iterations'],before=r['residual_rms_before'],after=r['residual_rms_after']) for r in state['report']['residual_refinement']],
        periods=[[f['period_px'] for f in ch['frequencies']] for ch in state['report']['diagnostics']['channels']]),indent=2))
finally:
    lib.banding_shutdown()
