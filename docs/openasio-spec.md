# OpenASIO – Open, ASIO-like driver ABI (Draft v0.1.0)

**Status:** draft v0.1.0 (experimental)  
**License:** MIT OR Apache-2.0

OpenASIO is a tiny, permissively licensed **C99 ABI** that separates a DAW **host** from a low-latency **driver**. Drivers own the audio thread and _pull_ the host’s callback each period, mirroring the control direction popularized by ASIO. This project is **not** affiliated with Steinberg ASIO®; it’s an independent, open alternative with similar goals.

## Design Goals
- **Stable C ABI:** single header `openasio.h`, fixed struct sizes/versioning.
- **RT-safety:** In `process()`, hosts/drivers avoid heap allocations, locks, syscalls and logging; denormals should be flushed by the host.
- **Buffer model:** One `process()` per period; interleaved or non-interleaved, format negotiated at `start()`.
- **Timing:** `oa_time_info` provides a host monotonic timestamp and optional device time.
- **Discovery:** Hosts `dlopen` driver .so/.dylib/.dll and resolve two symbols:
  - `openasio_driver_create(const oa_create_params*, oa_driver**)`
  - `openasio_driver_destroy(oa_driver*)`

## Lifecycle
1. Host loads driver shared library.
2. Host fills `oa_host_callbacks` and `oa_create_params` and calls `openasio_driver_create`.
3. Host calls `driver->vt->open_device(driver, NULL)` (or by name).
4. Host negotiates config via `get_default_config()` and adjusts fields if needed.
5. Host starts streaming with `start(&cfg)`; driver begins invoking `host.process(...)` on its RT thread.
6. Host stops with `stop()`, then destroys with `openasio_driver_destroy()`.

## Threading Model
- The **driver** owns the RT thread and invokes `host.process` for each period.
- The **host** may call control functions (e.g. `stop`, `set_sample_rate`) from a non-RT thread; drivers must document which calls are safe while running.

## Error Handling
All driver methods return `oa_result` (0 = `OA_OK`, negative = error). If `host.process` returns `OA_FALSE`, the driver should stop gracefully soon after.

## ABI Details
See the header for exact types and enums. Key types:
- `oa_stream_config`: sample rate, buffer size (frames), channel counts, format, layout.
- `oa_time_info`: timing and XRuns.
- `oa_driver_vtable`: function table the driver implements.

## Versioning Policy
- Semver-like triplet in the header. Minor/patch releases are strictly additive.
- Breaking ABI changes will bump **MAJOR**.

## Licensing
- Header/spec: **MIT OR Apache-2.0**.
- Example drivers may include additional licenses per backend.

## Notes
- This spec intentionally avoids policy-heavy features (device topology, MIDI, clock domains). These can be layered on via extensions in future versions.
