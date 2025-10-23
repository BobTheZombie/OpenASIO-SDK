# OpenASIO – Open, ASIO-like driver ABI (Draft v0.2.0)

**Status:** draft v0.2.0  
**License:** MIT OR Apache-2.0

OpenASIO is a tiny **C99 ABI** that separates a DAW **host** from a low-latency **driver**. Drivers own the audio thread and *pull* the host’s audio via a `process()` callback every period.

> Not affiliated with Steinberg ASIO®.

## What’s new in v0.2.0
- **Buffer layout enum** (`oa_buffer_layout`) with explicit **interleaved** and **non-interleaved**.
- **Capabilities bits** (`oa_caps`): discover OUTPUT/INPUT/FULL_DUPLEX and reconfig support.
- Clarified **threading**, **error semantics**, and **lifecycle**.
- Extended header comments and normative language.

## Lifecycle
1. Host loads driver shared library and resolves:
   - `openasio_driver_create(const oa_create_params*, oa_driver**)`
   - `openasio_driver_destroy(oa_driver*)`
2. Host fills `oa_host_callbacks` (must be RT-safe) and calls `create`.
3. Host opens a device (optional name substring) with `open_device`.
4. Host retrieves config with `get_default_config`, adjusts (layout, channels, sample rate, buffer frames).
5. Host starts streaming with `start(&cfg)` — driver begins invoking `host.process(...)` on its RT thread.
6. Host may stop with `stop()`, then `close_device()` and destroy the driver.

## RT-Safety Rules (Normative)
- In `host.process`: **no allocations, locks, syscalls, or logging**. Use preallocated buffers and lock-free queues.
- Drivers must not block in the audio thread. Time-critical code only.
- Hosts should flush denormals (FTZ/DAZ) before processing.

## Buffering & Layout
- Interleaved: `out` is a single contiguous buffer, length = `frames * out_channels` samples.
- Non-interleaved: `out` is an array of `out_channels` pointers, each to `frames` samples.
- Sample format is negotiated at `start()`; float32 is strongly RECOMMENDED.

## Timing
- `oa_time_info` contains `host_time_ns` (monotonic) and an optional `device_time_ns`.
- Drivers should increment underrun/overrun counters; reset between callbacks is driver-defined.

## Capabilities
- `get_caps()` returns OR-combination of `oa_caps` flags. Hosts adapt to OUTPUT-only drivers, etc.

## Versioning
- Header declares `OA_VERSION_MAJOR.MINOR.PATCH`. MINOR/PATCH are additive.
- Breaking ABI bumps **MAJOR**.

## Error Handling
- Methods return `oa_result`. `OA_OK (0)` success; negative values are errors.
- If `host.process` returns `OA_FALSE`, the driver **should** stop soon.

## Extensions
- v0.2 keeps the core tight. Extensions are negotiated by driver-specific means (e.g. extra symbols). Future versions may add a key/value query.
