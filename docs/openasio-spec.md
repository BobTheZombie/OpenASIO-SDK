# OpenASIO – Open, ASIO-like driver ABI (v1.0.0)

**Status:** 1.0.0 (stable)  
**License:** MIT OR Apache-2.0

OpenASIO is a compact **C99 ABI** dividing a DAW **host** from a low-latency **driver**. Drivers own the RT thread and pull the host’s `process()` every period.

> Not affiliated with Steinberg ASIO®.

## Highlights (1.0.0)
- **Full duplex** (input/output) with explicit capability bits.
- **Interleaved & non-interleaved** buffers.
- Clear **RT-safety** rules and lifecycle.
- Minimal surface: device open/start/stop, latency, optional reconfig.

## RT Rules (normative)
- In `host.process`: no heap allocations, locks, syscalls, or logging.
- Driver must not block the audio thread.
- Host should flush denormals (FTZ/DAZ).

## Buffering
- Interleaved: `[L0,R0, L1,R1, ...]` with `frames*out_channels` samples.
- Non-interleaved: `void**` array, `out_channels` pointers each to `frames` samples.

## Capabilities
- `get_caps()` returns OR of `OA_CAP_*`. Host adapts (e.g., OUTPUT-only drivers).

## Versioning
- Header defines `OA_VERSION_*`. Patch/minor are additive only. Breaking ABI bumps **MAJOR**.

## Error Handling
- All methods return `oa_result`. Negative values are errors. If `host.process` returns `OA_FALSE`, the driver should stop soon.

## Discovery
- Hosts `dlopen` a driver and resolve:
  - `openasio_driver_create(const oa_create_params*, oa_driver**)`
  - `openasio_driver_destroy(oa_driver*)`
